//! The typed shape of `model/cluster.toml` (`SPEC.md` §2, §3.1, §4.1, §16.2).
//!
//! The type that is *not* here is the interesting one. There is no `Macs`, no
//! `mgmt_address`, and no declared node table: a machine's identity is
//! discovered on the machine (§2.3, §3.1), and a model that named it would be
//! keeping a fact about hardware somewhere the hardware cannot see.
//!
//! What replaces it is [`Node`] --- an **ordinal slot**, derived rather than
//! parsed. Ordinals `1..=fleet.size` exist whether or not any machine is
//! holding one, so the renderer can still emit a complete firewall and a
//! complete scrape list, while nothing in the tree says which chassis is which.

use serde::Deserialize;

/// `model/cluster.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClusterFile {
    /// The schema tag.
    pub spec: String,
    /// The cluster's domain, e.g. `devcluster` (§4.3).
    pub domain: String,
    /// The tailnet the control plane is published on.
    pub tailnet: String,
    /// Tailscale's MagicDNS suffix. A client reaches a node at
    /// `<name>.<tailnet>.<magic_dns_suffix>`, which is the only stable way to
    /// reach one: management addresses come from DHCP (§3.2).
    pub magic_dns_suffix: String,
    /// The logins permitted to drive the cluster (§16.2).
    ///
    /// A list, because `afflom` is a user account and a personal namespace has
    /// no membership API to ask instead. §16.2 states that limit rather than
    /// leaving it to be discovered by whoever tries to add a second person.
    pub authorized_logins: Vec<String>,
    /// The shape of a conforming fleet (§2.3).
    pub fleet: Fleet,
    /// How a machine names itself before it has an ordinal (§2.3.2).
    pub identity: Identity,
    /// The predicate that makes a machine the storage node (§2.3.1).
    pub detection: Detection,
    /// The roles a machine may hold, and how it comes to hold one.
    pub role: Vec<Role>,
    /// Hardware uniform across the fleet, declared once (§2.1).
    pub profile: Profile,
    /// The devices a conforming machine may carry.
    pub disk: Vec<Disk>,
    /// The M.2 partition layout, identical on every node (§5.1).
    pub partition: Vec<Partition>,
    /// The GitHub App a browser authorizes against (§16.2).
    pub github_app: GithubApp,
    /// Firmware settings this pipeline cannot reach (§2.4).
    pub firmware: Vec<Firmware>,
}

/// The shape of a conforming fleet (§2.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Fleet {
    /// How many ordinals exist. Three is structural (§1.1).
    pub size: u32,
    /// How an ordinal becomes a name. `{ordinal}` and `{domain}` substitute.
    pub name_template: String,
    /// The bare cluster name, resolving to whichever ordinal holds `storage`.
    pub entry_name: String,
}

impl Fleet {
    /// Every ordinal, in ascending order.
    pub fn ordinals(&self) -> impl Iterator<Item = u32> {
        1..=self.size
    }

    /// The fully-qualified name of an ordinal, e.g. `node2.devcluster`.
    pub fn name_of(&self, ordinal: u32, domain: &str) -> String {
        self.name_template
            .replace("{ordinal}", &ordinal.to_string())
            .replace("{domain}", domain)
    }
}

/// Where a machine reads its own stable identifier (§2.3.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    /// The file holding it. `/etc/machine-id`: written by systemd on first
    /// boot, living in writable state, and not derived from any MAC.
    pub source: String,
}

/// The predicate that makes a machine the storage node (§2.3.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Detection {
    /// A non-boot block device at or above this makes its holder the storage
    /// node. Chosen to sit far above the container-graph SSDs and far below the
    /// bulk device, so it is true on exactly one machine of a conforming fleet.
    pub bulk_disk_min_gb: u32,
}

/// A role a machine may hold (§2.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    /// `storage`, `compute`, or `testbed`.
    pub id: String,
    /// How a machine comes to hold it: `bulk-disk` (self-detected) or
    /// `assigned` (handed out by the registrar).
    pub detect: String,
    /// The ordinal this role always takes, for the self-detected role only.
    #[serde(default)]
    pub ordinal: Option<u32>,
    /// Position in the registrar's hand-out sequence, for assigned roles only:
    /// the first machine to register takes `assign_order = 1` (§2.3.2).
    #[serde(default)]
    pub assign_order: Option<u32>,
    /// Position in the rollout sequence, 1-based (§13.2).
    pub update_position: u32,
    /// What this role runs, for the reader of the model.
    pub runs: String,
    /// BMC power-on delay after AC restore (§2.5).
    pub power_on_delay_s: u32,
    /// Which device plays which storage role for a machine holding this role.
    #[serde(default)]
    pub devices: RoleDevices,
}

impl Role {
    /// Whether a machine works this role out from its own hardware, rather than
    /// being told (§2.3.1).
    pub fn is_self_detected(&self) -> bool {
        self.detect == "bulk-disk"
    }
}

/// Which device plays which storage role. Every field is optional because the
/// devices differ per role, and a role that has no cache device should say
/// nothing rather than say `none` (§5.1).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RoleDevices {
    /// The device caching the origin LV, or the M.2 partition standing in for
    /// it when the chassis will not take both SATA devices (§2.2).
    #[serde(default)]
    pub cache_device: Option<String>,
    /// Size of the M.2 cache partition used by the §2.2 fallback.
    #[serde(default)]
    pub cache_partition_gib: Option<u32>,
    /// The LVM volume group holding the data LV.
    #[serde(default)]
    pub volume_group: Option<String>,
    /// The origin logical volume the cache accelerates.
    #[serde(default)]
    pub origin_lv: Option<String>,
    /// The device carrying container graph storage, which is never NFS (§11.2).
    #[serde(default)]
    pub container_graph_device: Option<String>,
    /// The device carrying measurement working state.
    #[serde(default)]
    pub bench_device: Option<String>,
}

/// The GitHub App a browser authorizes against (§16.2).
///
/// Every field here is public by design. The device flow uses a public client ID
/// with no client secret --- which is exactly what lets a static page start an
/// authorization --- so none of this belongs in §12.2's table of secrets, and
/// putting it there would teach the reader of that table to skim.
#[derive(Debug, Clone, Deserialize)]
pub struct GithubApp {
    /// The App's public client identifier.
    pub client_id: String,
    /// What the browser token may do. `read:user` and nothing else: the token
    /// is identity, and repository access is a per-session credential that
    /// arrives over the tunnel and dies with the connection (§16.2).
    pub scopes: Vec<String>,
    /// Where a device authorization begins.
    pub device_code_url: String,
    /// Where the device code is exchanged for a token.
    pub token_url: String,
    /// Where a bearer token is resolved to a login.
    pub user_url: String,
}

/// Hardware uniform across the fleet (§2.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    /// The profile's name, for the reader.
    pub id: String,
    /// Chassis model.
    pub chassis: String,
    /// Mainboard model.
    pub board: String,
    /// CPU model.
    pub cpu: String,
    /// Physical cores.
    pub cores: u32,
    /// Hardware threads.
    pub threads: u32,
    /// Base clock.
    pub base_mhz: u32,
    /// Maximum clock. Equal to `base_mhz` on this part, which is why a node
    /// built on it can host a measurement (§2.1).
    pub max_mhz: u32,
    /// Thermal design power.
    pub tdp_watts: u32,
    /// Whether AVX-512 is available. It is not, and that bounds what any
    /// measurement taken here generalizes to.
    pub avx512: bool,
    /// Installed memory.
    pub memory_gb: u32,
    /// DIMM slots.
    pub memory_slots: u32,
    /// The board's memory ceiling.
    pub memory_ceiling_gb: u32,
    /// 10GBase-T ports, which is what bounds the mesh at three nodes (§1.1) and
    /// what §3.1's classifier must find.
    pub nic_10g: u32,
    /// 1GbE ports.
    pub nic_1g: u32,
    /// Whether a dedicated BMC port is present.
    pub ipmi: bool,
}

/// A device a conforming machine may carry.
#[derive(Debug, Clone, Deserialize)]
pub struct Disk {
    /// Stable identifier referenced by [`RoleDevices`].
    pub id: String,
    /// `nvme`, `ssd`, or `hdd`.
    pub kind: String,
    /// Nominal capacity.
    pub size_gb: u32,
    /// What it is for.
    pub purpose: String,
}

/// One partition of the M.2, identical on every node (§5.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Partition {
    /// `p1`, `p2`, `p3`.
    pub id: String,
    /// A size, or `remainder`.
    pub size: String,
    /// Filesystem.
    pub format: String,
    /// Mount point.
    pub mount: String,
}

/// A firmware setting applied by hand and re-verified by `CH-01` (§2.4).
#[derive(Debug, Clone, Deserialize)]
pub struct Firmware {
    /// The setting's name in the BIOS.
    pub setting: String,
    /// The value it must hold.
    pub value: String,
    /// How a hardware smoke run observes it.
    pub probe: String,
    /// Why it is set that way.
    pub reason: String,
}

/// An **ordinal slot**: everything the fleet knows about position `n` without
/// knowing which machine is holding it (§2.3, §4.1).
///
/// Derived, never parsed. There is no `[[node]]` table in the model and there is
/// no constructor taking one; [`crate::Cluster::nodes`] builds these from the
/// fleet size, the role table and the addressing arithmetic. Every field is a
/// function of the ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Position in the fleet, `1..=fleet.size`.
    pub ordinal: u32,
    /// Short name, e.g. `node2`. The fully-qualified name without the domain.
    pub name: String,
    /// Fully-qualified name, e.g. `node2.devcluster` (§4.3).
    pub fqdn: String,
    /// The role a machine in this slot holds (§2.3).
    pub role: String,
    /// Position in the rollout sequence, from the role (§13.2).
    pub update_position: u32,
    /// The mesh loopback every mesh service binds, derived from the ordinal
    /// (§4.1).
    pub loopback: String,
}

impl ClusterFile {
    /// A role by name.
    pub fn role(&self, id: &str) -> Option<&Role> {
        self.role.iter().find(|r| r.id == id)
    }

    /// The one role a machine works out for itself (§2.3.1).
    pub fn self_detected_role(&self) -> Option<&Role> {
        self.role.iter().find(|r| r.is_self_detected())
    }

    /// The roles the registrar hands out, in the order it hands them out
    /// (§2.3.2).
    pub fn assigned_roles(&self) -> Vec<&Role> {
        let mut roles: Vec<&Role> = self
            .role
            .iter()
            .filter(|r| r.assign_order.is_some())
            .collect();
        roles.sort_by_key(|r| r.assign_order);
        roles
    }

    /// Which role the ordinal `n` holds.
    ///
    /// The self-detected role pins its own ordinal; the rest follow in
    /// hand-out order. This is the inverse of the registrar's decision, and it
    /// is what lets the renderer name a slot's role without any machine having
    /// booted.
    pub fn role_of_ordinal(&self, ordinal: u32) -> Option<&Role> {
        if let Some(fixed) = self.role.iter().find(|r| r.ordinal == Some(ordinal)) {
            return Some(fixed);
        }
        let taken: Vec<u32> = self.role.iter().filter_map(|r| r.ordinal).collect();
        let free: Vec<u32> = self
            .fleet
            .ordinals()
            .filter(|o| !taken.contains(o))
            .collect();
        let position = free.iter().position(|o| *o == ordinal)?;
        self.assigned_roles().get(position).copied()
    }
}
