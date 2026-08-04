//! The typed shape of `model/cluster.toml` (`SPEC.md` §2, §3.1, §5.1, §16.2).

use serde::Deserialize;

/// `model/cluster.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClusterFile {
    /// The schema tag.
    pub spec: String,
    /// The tailnet the control plane is published on.
    pub tailnet: String,
    /// The logins permitted to drive the cluster (§16.2).
    ///
    /// A list, because `afflom` is a user account and a personal namespace has
    /// no membership API to ask instead. §16.2 states that limit rather than
    /// leaving it to be discovered by whoever tries to add a second person.
    pub authorized_logins: Vec<String>,
    /// The GitHub App a browser authorizes against (§16.2).
    pub github_app: GithubApp,
    /// Hardware profiles, declared once and referenced by name.
    pub profile: Vec<Profile>,
    /// One row per node.
    pub node: Vec<Node>,
    /// The M.2 partition layout, identical on every node (§5.1).
    pub partition: Vec<Partition>,
    /// Firmware settings this pipeline cannot reach (§2.4).
    pub firmware: Vec<Firmware>,
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
    /// Referenced by [`Node::profile`].
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
    /// 10GBase-T ports, which is what bounds the mesh at three nodes (§1.1).
    pub nic_10g: u32,
    /// 1GbE ports.
    pub nic_1g: u32,
    /// Whether a dedicated BMC port is present.
    pub ipmi: bool,
}

/// One node.
#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    /// Short name, e.g. `n1`.
    pub name: String,
    /// The role that decides what this node runs (§2.3).
    pub role: String,
    /// The [`Profile`] this node is built on.
    pub profile: String,
    /// Position in the rollout sequence, 1-based (§13.2).
    pub update_position: u32,
    /// The mesh loopback every mesh service binds (§4.1).
    pub loopback: String,
    /// Management address with prefix length.
    pub mgmt_address: String,
    /// BMC address with prefix length. Never routed to WAN (§3.2).
    pub bmc_address: String,
    /// BMC power-on delay after AC restore (§2.5).
    pub power_on_delay_s: u32,
    /// Interface identity. Every rendered `.network` matches on these (§3.1).
    pub mac: Macs,
    /// Which device plays which storage role on this node (§5.1).
    pub storage: NodeStorage,
    /// The physical devices installed.
    pub disk: Vec<Disk>,
}

/// The four MAC addresses a node declares (§3.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Macs {
    /// Management plane, 1GbE.
    pub mgmt: String,
    /// First mesh port, 10GBase-T.
    pub mesh_a: String,
    /// Second mesh port, 10GBase-T.
    pub mesh_b: String,
    /// Dedicated IPMI port.
    pub bmc: String,
}

impl Macs {
    /// The four addresses paired with the interface role each carries.
    pub fn roles(&self) -> [(&'static str, &str); 4] {
        [
            ("mgmt", self.mgmt.as_str()),
            ("mesh_a", self.mesh_a.as_str()),
            ("mesh_b", self.mesh_b.as_str()),
            ("bmc", self.bmc.as_str()),
        ]
    }

    /// The address carrying a named interface role, if this node declares one.
    pub fn get(&self, role: &str) -> Option<&str> {
        self.roles()
            .into_iter()
            .find(|(name, _)| *name == role)
            .map(|(_, mac)| mac)
    }
}

/// Which device plays which storage role. Every field is optional because the
/// roles differ per node, and a node that has no cache device should say
/// nothing rather than say `none` (§5.1).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeStorage {
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

/// A physical device installed in a node.
#[derive(Debug, Clone, Deserialize)]
pub struct Disk {
    /// Stable identifier referenced by [`NodeStorage`].
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

impl ClusterFile {
    /// Look up a node by name.
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.node.iter().find(|n| n.name == name)
    }

    /// The hardware profile a node is built on.
    pub fn profile_of(&self, node: &Node) -> Option<&Profile> {
        self.profile.iter().find(|p| p.id == node.profile)
    }

    /// Nodes in rollout order (§13.2).
    pub fn in_update_order(&self) -> Vec<&Node> {
        let mut nodes: Vec<&Node> = self.node.iter().collect();
        nodes.sort_by_key(|n| n.update_position);
        nodes
    }
}
