//! The typed cluster model, and the renderers that make R1 cover every
//! infrastructure artifact (`SPEC.md` §7).
//!
//! The template applies R1 to documentation: `CONFORMANCE.md` is generated from
//! `model/`, and a hand-edit is a gate failure. This crate extends the same rule
//! to `.network` files, firewall rules, Quadlets, kernel arguments, timer units,
//! the kickstart, and `ssh_config`. A hand-edited `.network` file is the same
//! class of error as a hand-edited `CONFORMANCE.md`, and `cargo xtask
//! check-render` reports it the same way.
//!
//! Nothing on a node parses this model at runtime. What ships inside an image is
//! the *rendered* tree, which is the grain bootc uses for everything else: a
//! node's configuration is a property of the image it booted, not of a file
//! something wrote after it booted.

#![deny(missing_docs)]

pub mod cluster;
pub mod images;
pub mod network;
pub mod policy;
pub mod render;

pub use cluster::{
    ClusterFile, Detection, Disk, Firmware, Fleet, GithubApp, Identity, Node, Partition, Profile,
    Role, RoleDevices,
};
pub use images::{
    Base, ImagesFile, Isolation, Quadlet, QuadletMount, Registries, Runner, Runtime, Signing,
    Upstream, Variant,
};
pub use network::{
    Addressing, Class, Discovery, Firewall, FirewallRule, Hosts, Link, NetworkFile, Routing,
};
pub use policy::{
    Alert, Auth, Drain, DrainBudget, Gc, Greenboot, Health, PolicyFile, Reclaim, Rollout, Secret,
    Tunnel,
};
pub use render::{render_all, Rendered};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where the rendered tree is committed, relative to the repository root.
pub const GENERATED_DIR: &str = "generated";

/// Everything `model/{cluster,network,images,policy}.toml` says, parsed and
/// cross-checked.
#[derive(Debug, Clone)]
pub struct Cluster {
    /// The fleet's shape, the roles, and the hardware a conforming machine has.
    pub cluster: ClusterFile,
    /// Interface classes, addressing arithmetic, routes, firewall.
    pub network: NetworkFile,
    /// Variants, base digest, runtime, packages, units, kargs.
    pub images: ImagesFile,
    /// Rollout timings, drain budgets, GC thresholds.
    pub policy: PolicyFile,
}

/// A failure to load or to cross-check the cluster model.
#[derive(Debug)]
pub enum ClusterError {
    /// A model file could not be read.
    Io(PathBuf, std::io::Error),
    /// A model file could not be parsed.
    Parse(PathBuf, toml::de::Error),
    /// The model disagrees with itself.
    Inconsistent(String),
}

impl std::fmt::Display for ClusterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "reading {}: {e}", p.display()),
            Self::Parse(p, e) => write!(f, "parsing {}: {e}", p.display()),
            Self::Inconsistent(m) => write!(f, "the cluster model is inconsistent: {m}"),
        }
    }
}

impl std::error::Error for ClusterError {}

fn bad(m: impl Into<String>) -> ClusterError {
    ClusterError::Inconsistent(m.into())
}

impl Cluster {
    /// Load every cluster model file from a `model/` directory.
    pub fn load(dir: &Path) -> Result<Self, ClusterError> {
        Ok(Self {
            cluster: read(dir, "cluster.toml")?,
            network: read(dir, "network.toml")?,
            images: read(dir, "images.toml")?,
            policy: read(dir, "policy.toml")?,
        })
    }

    /// Load from the repository root, resolved from this crate's manifest
    /// directory so that it works from any working directory.
    pub fn load_from_repo_root() -> Result<Self, ClusterError> {
        Self::load(&repo_root().join("model"))
    }

    /// Every ordinal slot the fleet has, in ascending order (§2.3, §4.1).
    ///
    /// **Derived, not parsed.** There is no `[[node]]` table to read. An
    /// ordinal exists whether or not a machine is holding it, and everything
    /// about the slot --- its name, its role, its rollout position, its
    /// loopback --- is a function of the number. That is what lets the renderer
    /// emit a complete firewall and a complete scrape list while nothing in the
    /// tree says which chassis is which.
    ///
    /// Total by construction: [`Cluster::check`] validates the fleet size, the
    /// role table and the addressing bases before anything renders, so the
    /// derivation cannot fail on a model that passed.
    pub fn nodes(&self) -> Vec<Node> {
        self.cluster
            .fleet
            .ordinals()
            .filter_map(|ordinal| self.node_at(ordinal))
            .collect()
    }

    /// The slot at one ordinal, or `None` when the model does not admit it.
    pub fn node_at(&self, ordinal: u32) -> Option<Node> {
        let role = self.cluster.role_of_ordinal(ordinal)?;
        let loopback = self.network.addressing.loopback_of(ordinal)?;
        let fqdn = self.cluster.fleet.name_of(ordinal, &self.cluster.domain);
        // The short name is the fully-qualified one without the domain, so the
        // two cannot disagree: `node2` is whatever `node2.devcluster` starts
        // with, not a second template.
        let name = fqdn
            .strip_suffix(&format!(".{}", self.cluster.domain))
            .unwrap_or(&fqdn)
            .to_string();
        Some(Node {
            ordinal,
            name,
            fqdn,
            role: role.id.clone(),
            update_position: role.update_position,
            loopback: loopback.to_string(),
        })
    }

    /// The slot with a given short or fully-qualified name.
    pub fn node(&self, name: &str) -> Option<Node> {
        self.nodes()
            .into_iter()
            .find(|n| n.name == name || n.fqdn == name)
    }

    /// The slot holding a role, of which there is exactly one (§2.3).
    pub fn node_with_role(&self, role: &str) -> Option<Node> {
        self.nodes().into_iter().find(|n| n.role == role)
    }

    /// Slots in rollout order (§13.2).
    pub fn in_update_order(&self) -> Vec<Node> {
        let mut nodes = self.nodes();
        nodes.sort_by_key(|n| n.update_position);
        nodes
    }

    /// Cross-check the model against itself.
    ///
    /// Every rule here is one a rendered artifact would otherwise encode
    /// silently. An update position that is not a permutation renders a rollout
    /// that either never starts or starts twice; an interface class nothing
    /// recognises renders a node that configures no ports. Catching those in
    /// the model is what keeps the renderers total.
    pub fn check(&self) -> Result<(), ClusterError> {
        self.check_spec()?;
        self.check_fleet()?;
        self.check_topology()?;
        self.check_firewall()?;
        self.check_variants()?;
        self.check_runners()?;
        self.check_policy()?;
        Ok(())
    }

    /// A role that hosts a runner has the software and the credential to run one
    /// (§9.5, §12.2).
    ///
    /// Every part of this was declared and none of it was joined. Units were
    /// rendered for three runners, the loop they invoke was rendered per role,
    /// and the image installed no runner at all --- so `config.sh` was a path
    /// that did not exist, on a node whose whole purpose in CI was to run jobs.
    /// T2 and the browser client's node-served mirror both wait on those
    /// runners, and both had waited since the units were written.
    fn check_runners(&self) -> Result<(), ClusterError> {
        if !self.images.hosts_runners() {
            return Ok(());
        }
        let install = self.images.runner_install_dir().ok_or_else(|| {
            bad(format!(
                "a variant declares an Actions runner and no `[[variant.upstream]]` \
                 named `{}` installs one. The rendered unit would start a runner from \
                 a directory nothing unpacks (§9.5)",
                crate::images::RUNNER_ARTIFACT
            ))
        })?;
        if !install.starts_with('/') {
            return Err(bad(format!(
                "the runner's install_dir is `{install}`, which is not an absolute path"
            )));
        }

        // And the credential has a way to reach a node. It cannot be in the
        // image --- it is a credential --- so enrolment is the only path, and a
        // runner whose credential is declared nowhere is a unit that is skipped
        // for ever (§12.2).
        let pat = crate::render::RUNNER_PAT;
        if !self.policy.secret.iter().any(|s| s.path == pat) {
            return Err(bad(format!(
                "a variant declares an Actions runner and no enrolled secret lands at \
                 {pat}. A registration credential cannot be in an image, so enrolment \
                 is the only way it reaches a node --- and without it every runner unit \
                 is skipped for ever (§9.5, §12.2)"
            )));
        }
        Ok(())
    }

    /// Every file carries the tag this build understands (§7.1).
    fn check_spec(&self) -> Result<(), ClusterError> {
        for (name, spec) in [
            ("cluster.toml", &self.cluster.spec),
            ("network.toml", &self.network.spec),
            ("images.toml", &self.images.spec),
            ("policy.toml", &self.policy.spec),
        ] {
            if spec != repo_model::SPEC {
                return Err(bad(format!(
                    "model/{name}: spec is `{spec}`, but this build understands `{}` (R1)",
                    repo_model::SPEC
                )));
            }
        }
        Ok(())
    }

    /// The fleet is a shape the derivation can work over (§2.3, §4.1).
    ///
    /// Every rule here fails a *derivation*, not a declaration. There are no
    /// duplicate names to catch and no duplicate MACs, because nothing declares
    /// either --- what can still go wrong is a role table that leaves an
    /// ordinal without a role, or update positions that are not a permutation.
    fn check_fleet(&self) -> Result<(), ClusterError> {
        let fleet = &self.cluster.fleet;
        if fleet.size == 0 {
            return Err(bad("model/cluster.toml declares a fleet of no nodes"));
        }
        for token in ["{ordinal}", "{domain}"] {
            if !fleet.name_template.contains(token) {
                return Err(bad(format!(
                    "fleet.name_template `{}` does not contain `{token}`. A template that \
                     ignores the ordinal names every slot the same thing (§4.3)",
                    fleet.name_template
                )));
            }
        }

        // Exactly one self-detected role, and it pins an ordinal. The registrar
        // is the machine that does not have to ask, so it cannot be assigned --
        // and two of them would be a fleet with two registrars and no tie-break
        // between them (§2.3.1).
        let detected: Vec<&Role> = self
            .cluster
            .role
            .iter()
            .filter(|r| r.is_self_detected())
            .collect();
        if detected.len() != 1 {
            return Err(bad(format!(
                "{} roles are self-detected. Exactly one is: the registrar is the machine \
                 that works its role out from its own disks, and every other role is handed \
                 out by it (§2.3.1)",
                detected.len()
            )));
        }
        if detected[0].ordinal.is_none() {
            return Err(bad(format!(
                "role `{}` is self-detected but pins no ordinal. The registrar cannot be \
                 assigned one, so it must declare the one it takes (§2.3.2)",
                detected[0].id
            )));
        }
        if detected[0].assign_order.is_some() {
            return Err(bad(format!(
                "role `{}` is both self-detected and assigned. It is one or the other, and \
                 a role that is both would be handed out to a machine that already had it \
                 (§2.3)",
                detected[0].id
            )));
        }

        let mut ids = BTreeSet::new();
        let mut orders = Vec::new();
        for role in &self.cluster.role {
            if !ids.insert(role.id.as_str()) {
                return Err(bad(format!("role `{}`: declared twice", role.id)));
            }
            if !role.is_self_detected() && role.detect != "assigned" {
                return Err(bad(format!(
                    "role `{}`: detect is `{}`, which is neither `bulk-disk` nor `assigned` \
                     (§2.3)",
                    role.id, role.detect
                )));
            }
            if let Some(order) = role.assign_order {
                orders.push(order);
                if role.ordinal.is_some() {
                    return Err(bad(format!(
                        "role `{}` is assigned but also pins an ordinal. Which ordinal an \
                         assigned role takes is the registrar's decision, and pinning one \
                         would make the model disagree with it (§2.3.2)",
                        role.id
                    )));
                }
            }
        }

        // Hand-out order is a permutation of 1..=k over the assigned roles. A
        // gap or a repeat is a registrar with two roles to hand out at the same
        // position and no way to choose (§2.3.2).
        orders.sort_unstable();
        let expected_orders: Vec<u32> = (1..=orders.len() as u32).collect();
        if orders != expected_orders {
            return Err(bad(format!(
                "assign_order values are {orders:?}, which is not a permutation of \
                 {expected_orders:?}. The registrar hands out one role per position (§2.3.2)"
            )));
        }

        if self.cluster.role.len() as u32 != fleet.size {
            return Err(bad(format!(
                "{} roles for a fleet of {}. Every ordinal holds exactly one role, so a \
                 slot with none would render a node nothing runs on (§2.3)",
                self.cluster.role.len(),
                fleet.size
            )));
        }

        // The derivation is total over the fleet, and the addressing bases
        // produce a distinct loopback per ordinal.
        let mut loopbacks = BTreeSet::new();
        let mut positions = Vec::new();
        for ordinal in fleet.ordinals() {
            let node = self.node_at(ordinal).ok_or_else(|| {
                bad(format!(
                    "ordinal {ordinal} derives no node. Either no role claims it or the \
                     addressing base does not reach it (§4.1)"
                ))
            })?;
            if !loopbacks.insert(node.loopback.clone()) {
                return Err(bad(format!(
                    "{}: loopback {} is already taken. Every mesh service binds its node's \
                     loopback, so a shared one is a silent collision (§4.1)",
                    node.name, node.loopback
                )));
            }
            positions.push(node.update_position);
        }

        // A permutation of 1..=n. Anything else is a rollout that either never
        // starts or has two nodes believing they are first (§13.2).
        positions.sort_unstable();
        let expected: Vec<u32> = (1..=fleet.size).collect();
        if positions != expected {
            return Err(bad(format!(
                "update positions are {positions:?}, which is not a permutation of {expected:?}. \
                 §13.2's predicate is true for exactly one node only when they are (§2.3)"
            )));
        }

        // Every assigned role must name the same second device.
        //
        // The installer prepares secondary storage from what it measures (§12.1),
        // and it cannot tell compute from testbed: that is the registrar's
        // decision, taken later over a network that does not exist during
        // Anaconda. Two roles wanting different devices would be a kickstart with
        // a branch it has no way to take.
        let local: BTreeSet<&str> = self
            .cluster
            .assigned_roles()
            .iter()
            .filter_map(|r| {
                r.devices
                    .container_graph_device
                    .as_deref()
                    .or(r.devices.bench_device.as_deref())
            })
            .collect();
        if local.len() > 1 {
            return Err(bad(format!(
                "the assigned roles name {local:?} as their local device. The installer \
                 cannot tell them apart --- which role a machine holds is decided after \
                 install, over a network that does not exist during Anaconda --- so they \
                 must agree (§2.3.2, §12.1)"
            )));
        }

        // The threshold that decides which machine is the storage node must sit
        // between the devices a conforming machine carries. One above every
        // disk is true on no machine; one below the container-graph SSD is true
        // on all three. Either way the registrar refuses and the fleet does not
        // come up (§2.3.1, §21.11).
        let threshold = self.cluster.detection.bulk_disk_min_gb;
        let bulk: Vec<&Disk> = self
            .cluster
            .disk
            .iter()
            .filter(|d| d.purpose != "boot" && d.size_gb >= threshold)
            .collect();
        if bulk.len() != 1 {
            return Err(bad(format!(
                "detection.bulk_disk_min_gb is {threshold} GB, which {} of the declared \
                 non-boot devices reach. The predicate must be true of exactly one device \
                 kind, or it is true on every machine or on none (§2.3.1)",
                bulk.len()
            )));
        }

        Ok(())
    }

    /// The mesh is the shape the topology says it is, and the addressing
    /// arithmetic covers it (§1.1, §4.1).
    fn check_topology(&self) -> Result<(), ClusterError> {
        let n = self.cluster.fleet.size;
        if n > self.network.topology.max_nodes {
            return Err(bad(format!(
                "{n} nodes, but topology `{}` admits {}. The direct mesh needs one 10 GbE \
                 port per peer and each node has two; a fourth node needs a switch and a \
                 different topology kind (§1.1)",
                self.network.topology.kind, self.network.topology.max_nodes
            )));
        }

        // A direct triangle needs one mesh port per peer, and the profile says
        // how many the board has. This is the check that made §1.1's ceiling a
        // consequence of the hardware rather than a number somebody wrote down.
        if self.network.topology.kind == "direct-triangle" {
            let needed = n.saturating_sub(1);
            if self.cluster.profile.nic_10g < needed {
                return Err(bad(format!(
                    "a fleet of {n} needs {needed} mesh ports per machine and the profile \
                     declares {}. The direct mesh gives each node one port per peer (§1.1)",
                    self.cluster.profile.nic_10g
                )));
            }
        }

        let addressing = &self.network.addressing;
        if addressing.link_prefix_len != 31 {
            return Err(bad(format!(
                "link_prefix_len is {}. §4.1 declares RFC 3021 point-to-point addressing, \
                 whose whole property is that a /31 has exactly two hosts and no network \
                 or broadcast address",
                addressing.link_prefix_len
            )));
        }
        if addressing.loopback_prefix_len != 32 {
            return Err(bad(format!(
                "loopback_prefix_len is {}, and a loopback is a host route (§4.1)",
                addressing.loopback_prefix_len
            )));
        }

        // Every pair derives a link, every link is an aligned /31, and no
        // address appears on two of them. Unlike the declared table this
        // replaced, a collision here is an arithmetic error rather than a typo
        // -- but it would be just as silent on a node, so it is still checked.
        let links = addressing.links(n);
        let expected_links = (n * n.saturating_sub(1)) / 2;
        if links.len() as u32 != expected_links {
            return Err(bad(format!(
                "the addressing derives {} links for a fleet of {n}, and a direct triangle \
                 joins every pair exactly once, which is {expected_links} (§4.1)",
                links.len()
            )));
        }
        let mut seen = BTreeSet::new();
        for link in &links {
            if !u32::from(link.lower_address).is_multiple_of(2) {
                return Err(bad(format!(
                    "link {}: {} is not an aligned /31. An unaligned prefix is a typo in \
                     the base, not a smaller subnet (§4.1)",
                    link.id(),
                    link.prefix()
                )));
            }
            for address in link.addresses() {
                if !seen.insert(address) {
                    return Err(bad(format!(
                        "link {}: {address} is already on another link. The bases overlap \
                         (§4.1)",
                        link.id()
                    )));
                }
            }
        }
        // The link and loopback ranges must not overlap either: a loopback that
        // collided with a link address would make a route to a peer resolve to
        // a cable rather than to the node.
        for ordinal in self.cluster.fleet.ordinals() {
            let loopback = addressing.loopback_of(ordinal).ok_or_else(|| {
                bad(format!(
                    "ordinal {ordinal}: the loopback base does not reach it"
                ))
            })?;
            if seen.contains(&loopback) {
                return Err(bad(format!(
                    "ordinal {ordinal}: loopback {loopback} is also a link address. The two \
                     bases overlap, and a route to a peer would resolve to a cable (§4.1)"
                )));
            }
        }

        // The classes a machine is sorted into. Both must exist, the mesh class
        // must ask for a port per peer, and the speeds must be ordered -- a
        // mesh threshold at or below the LAN one puts every port in one class.
        let mesh = self.network.mesh_class().ok_or_else(|| {
            bad(
                "no `mesh` interface class. §3.1 classifies ports by speed, and the mesh \
                 class is what recognises a 10GBase-T port",
            )
        })?;
        let lan = self
            .network
            .lan_class()
            .ok_or_else(|| bad("no `lan` interface class (§3.1)"))?;
        if mesh.min_speed_mbps <= lan.min_speed_mbps {
            return Err(bad(format!(
                "the mesh class starts at {} Mbps and the LAN class at {}. The mesh \
                 threshold must be the higher one, or every port classifies as mesh (§3.1)",
                mesh.min_speed_mbps, lan.min_speed_mbps
            )));
        }
        if mesh.count != n.saturating_sub(1) {
            return Err(bad(format!(
                "the mesh class expects {} ports and a fleet of {n} gives each machine {} \
                 peers. A machine that came up with fewer would join with no redundancy and \
                 nothing would say so (§3.1)",
                mesh.count,
                n.saturating_sub(1)
            )));
        }
        if mesh.count > self.cluster.profile.nic_10g {
            return Err(bad(format!(
                "the mesh class expects {} ports and the profile declares {} (§2.1, §3.1)",
                mesh.count, self.cluster.profile.nic_10g
            )));
        }
        if lan.count > self.cluster.profile.nic_1g {
            return Err(bad(format!(
                "the LAN class expects {} ports and the profile declares {} (§2.1, §3.1)",
                lan.count, self.cluster.profile.nic_1g
            )));
        }
        if lan.addressing != "dhcp" {
            return Err(bad(format!(
                "the LAN class addresses by `{}`. §3.2 makes it DHCP: there is no \
                 per-machine fact left to make it static from",
                lan.addressing
            )));
        }
        if mesh.addressing != "derived" {
            return Err(bad(format!(
                "the mesh class addresses by `{}`, and §4.1 derives mesh addresses from \
                 ordinals",
                mesh.addressing
            )));
        }

        if self.network.routing.direct_metric >= self.network.routing.transit_metric {
            return Err(bad(format!(
                "direct metric {} is not below transit metric {}. The transit route must \
                 take over only when the direct one is withdrawn (§4.2)",
                self.network.routing.direct_metric, self.network.routing.transit_metric
            )));
        }
        if !self.network.routing.ip_forward {
            return Err(bad(
                "ip_forward is false, so a transit route has nothing to transit (§4.2)",
            ));
        }

        // Discovery is what turns a cable into a peer, and a timeout shorter
        // than a cold boot would report a link unpeered because the machine on
        // the other end was still starting (§3.3, §12.1).
        let discovery = &self.network.discovery;
        if discovery.group.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(bad(format!(
                "discovery.group `{}` is not an IPv6 address (§3.3)",
                discovery.group
            )));
        }
        if !discovery.group.starts_with("ff02:") {
            return Err(bad(format!(
                "discovery.group `{}` is not link-local scope. §3.3 announces on one \
                 interface, and a wider scope would leak the announcement past the cable \
                 it is asking about",
                discovery.group
            )));
        }
        if discovery.interval_ms == 0 {
            return Err(bad(
                "discovery.interval_ms is zero, which is not a fast retry but a spin (§3.3)",
            ));
        }
        if u64::from(discovery.timeout_s) * 1000 <= u64::from(discovery.interval_ms) {
            return Err(bad(format!(
                "discovery times out after {}s having announced every {}ms, so it gives up \
                 before it has retried (§3.3)",
                discovery.timeout_s, discovery.interval_ms
            )));
        }
        Ok(())
    }
    /// The firewall drops by default and every accept is declared (§4.4).
    fn check_firewall(&self) -> Result<(), ClusterError> {
        if self.network.firewall.input_policy != "drop" {
            return Err(bad(format!(
                "input policy is `{}`. §4.4 declares drop, and a default-accept input chain \
                 makes every rule below it decoration",
                self.network.firewall.input_policy
            )));
        }
        for rule in &self.network.firewall.rule {
            // `tailscale` and `lo` are pseudo-classes: one is an overlay
            // interface no classifier sorts, the other is not an interface at
            // all.
            let known = ["tailscale", "lo"].contains(&rule.plane.as_str())
                || self.network.class.iter().any(|c| c.id == rule.plane);
            if !known {
                return Err(bad(format!(
                    "firewall rule on `{}`, which is not a declared interface class (§3.1)",
                    rule.plane
                )));
            }
            // Roles rather than nodes: one image means one ruleset, so a rule
            // true of one role only is rendered into its own include (§8.4). A
            // rule naming a role that does not exist would render an include
            // nothing ever links into place.
            for role in &rule.roles {
                if self.cluster.role(role).is_none() {
                    return Err(bad(format!(
                        "firewall rule names role `{role}`, which is not declared (§2.3)"
                    )));
                }
            }
        }
        if self.network.lan_prefix.parse::<IpPrefix>().is_err() {
            return Err(bad(format!(
                "lan_prefix `{}` is not a prefix",
                self.network.lan_prefix
            )));
        }
        Ok(())
    }

    /// One variant per node, a declared runtime, and a relabel on every mount
    /// (§8.2, §8.3, §8.4).
    fn check_variants(&self) -> Result<(), ClusterError> {
        if !self.images.base.digest.starts_with("sha256:") {
            return Err(bad(format!(
                "base digest `{}` is not a sha256 digest. A repository this careful about \
                 digest-pinning downstream cannot float its upstream (§8.1)",
                self.images.base.digest
            )));
        }
        // §12.3's identity is a *workflow* reference. An empty or repository-only
        // one would render a policy admitting anything any workflow in this
        // repository ever signed --- and it would look like a policy, which is
        // worse than having none.
        let signing = &self.images.signing;
        for (field, value) in [
            ("issuer", &signing.issuer),
            ("repository", &signing.repository),
            ("workflow", &signing.workflow),
            ("ref", &signing.ref_),
        ] {
            if value.trim().is_empty() {
                return Err(bad(format!(
                    "signing.{field} is empty. The certificate identity a node will \
                     accept is built from all four, and a missing one widens the \
                     policy silently (§12.3)"
                )));
            }
        }
        if !signing.workflow.ends_with(".yml") || !signing.workflow.contains('/') {
            return Err(bad(format!(
                "signing.workflow is `{}`, which is not a workflow path. §12.3 binds \
                 the identity to one workflow, not to the repository: an image signed \
                 by a different workflow in the same repository must not stage either",
                signing.workflow
            )));
        }

        if self.images.runtime(&self.images.default_runtime).is_none() {
            return Err(bad(format!(
                "default_runtime `{}` is not a declared runtime (§8.2)",
                self.images.default_runtime
            )));
        }

        for role in &self.cluster.role {
            if self.images.variant_for(&role.id).is_none() {
                return Err(bad(format!(
                    "role `{}`: no variant in model/images.toml says what it adds to the \
                     image. One image serves all three roles, and a role that contributes \
                     nothing is a role no machine can tell it is holding (§8.4)",
                    role.id
                )));
            }
        }

        let mut isolated = Vec::new();
        for variant in &self.images.variant {
            if self.cluster.role(&variant.role).is_none() {
                return Err(bad(format!(
                    "variant {}: names role `{}`, which is not declared. R1 makes a \
                     dangling reference a build failure rather than a stale file (§17.2)",
                    variant.id, variant.role
                )));
            }
            if self.images.runtime_of(variant).is_none() {
                return Err(bad(format!(
                    "variant {}: the default runtime `{}` is not declared (§8.2)",
                    variant.id, self.images.default_runtime
                )));
            }
            for quadlet in variant.all_quadlets(&self.images.base) {
                // Every placeholder resolves before anything is rendered, so the
                // renderers stay total and a typo is a model error rather than a
                // literal `{n9.loopback}` in a shipped unit file.
                for publish in &quadlet.publish {
                    self.substitute(publish)?;
                }
                for mount in &quadlet.mount {
                    if !["Z", "z"].contains(&mount.relabel.as_str()) {
                        return Err(bad(format!(
                            "variant {}: quadlet `{}` mounts {} with relabel `{}`. Every \
                             volume mount carries :Z or :z, because a missing relabel is an \
                             AVC denial at boot and a denial is a build failure (§8.3)",
                            variant.id, quadlet.name, mount.source, mount.relabel
                        )));
                    }
                }
            }
            for mount in &variant.mount {
                self.substitute(&mount.what)?;
                self.substitute(&mount.where_)?;
            }
            if let Some(isolation) = &variant.isolation {
                isolated.push(variant.id.as_str());
                let expected = format!("isolcpus={}", isolation.isolated_cpus);
                if !variant.kargs.iter().any(|k| k == &expected) {
                    return Err(bad(format!(
                        "variant {}: declares isolated CPUs `{}` but carries no `{expected}` \
                         karg. The isolation and the kernel argument are the same fact and \
                         must not be able to disagree (§8.5)",
                        variant.id, isolation.isolated_cpus
                    )));
                }
            }
        }

        // Isolation is the testbed's alone. A second isolated variant would mean
        // a node that both measures and serves, which §2.3 exists to prevent.
        if isolated.len() > 1 {
            return Err(bad(format!(
                "variants {isolated:?} all declare CPU isolation; measurement is one node's \
                 job (§2.3)"
            )));
        }
        // Isolation is applied after the role is known, not shipped in the
        // image: one image is installed on all three machines and isolating two
        // cores on the storage node would cost half its CPU to no purpose. A
        // variant that put its isolation kargs in the base would do exactly
        // that, so the base carries none of them (§8.5).
        if let Some(karg) =
            self.images.base.content.kargs.iter().find(|k| {
                k.starts_with("isolcpus=") || k.starts_with("nohz_full=") || *k == "nosmt"
            })
        {
            return Err(bad(format!(
                "the base carries kernel argument `{karg}`. One image boots all three roles, \
                 so an isolation karg in the base isolates the storage node's cores too. \
                 §8.5 applies these after the role is known, with \
                 `bootc loader-entries set-options-for-source`"
            )));
        }
        Ok(())
    }

    /// Drain, reclamation and health thresholds are internally coherent
    /// (§14, §15.3, §10.1).
    fn check_policy(&self) -> Result<(), ClusterError> {
        // Drain names *roles*, not machines. Which chassis receives a migrated
        // workload is not a fact this repository holds any more; which role
        // does is (§2.3, §14.1).
        let drain = &self.policy.drain;
        if self.cluster.role(&drain.migration_target).is_none() {
            return Err(bad(format!(
                "migration_target `{}` is not a declared role (§2.3)",
                drain.migration_target
            )));
        }
        for role in &drain.never_receives {
            if self.cluster.role(role).is_none() {
                return Err(bad(format!(
                    "never_receives names `{role}`, which is not a declared role (§2.3)"
                )));
            }
            if role == &drain.migration_target {
                return Err(bad(format!(
                    "`{role}` is both the migration target and a role that never receives \
                     work (§2.3, §14.1)"
                )));
            }
            // A role that must receive nothing must also mount nothing: NFS
            // client activity, RPC timers and interrupt handling inject jitter
            // into exactly the quantity being measured (§2.3).
            if let Some(variant) = self.images.variant_for(role) {
                if let Some(mount) = variant.mount.first() {
                    return Err(bad(format!(
                        "`{role}` receives no migrated workload, but its variant mounts {} \
                         at {}. A network filesystem on a measurement node is jitter in \
                         the quantity being measured (§2.3, §8.4)",
                        mount.what, mount.where_
                    )));
                }
            }
        }

        // §16.3: an exact origin, never a wildcard. This API drives a cluster,
        // and `*` would let any page the operator's browser happens to load use
        // the token that browser already holds. Checked here rather than left to
        // a rendered file's bytes, because a stale render fails for a different
        // reason and would make this look gated when it was not.
        let origin = &self.policy.auth.allowed_origin;
        if origin == "*" || !origin.starts_with("https://") || origin.contains('*') {
            return Err(bad(format!(
                "auth.allowed_origin is `{origin}`. Cross-origin access names one \
                 exact https origin: a wildcard lets any page a browser visits drive \
                 this cluster with a token it already holds (§16.3)"
            )));
        }
        if self.policy.auth.token_cache_ttl_s == 0 {
            return Err(bad(
                "auth.token_cache_ttl_s is zero, which is not a cache with no lag but \
                 a round trip to the identity provider on every request (§16.2)",
            ));
        }

        let r = &self.policy.reclaim;
        if !(r.notify_after_days < r.archive_after_days
            && r.archive_after_days < r.purge_after_days)
        {
            return Err(bad(format!(
                "reclamation thresholds are {}, {}, {} days and must be strictly \
                 increasing (§15.3)",
                r.notify_after_days, r.archive_after_days, r.purge_after_days
            )));
        }
        if r.purge_dirty {
            return Err(bad(
                "purge_dirty is true. Dirty is the one flag that overrides the retention \
                 policy, and a model that lets it be turned off has no dirty exemption \
                 (§15.2, §15.3)",
            ));
        }

        let mut classes = BTreeSet::new();
        for budget in &self.policy.drain_budget {
            if !classes.insert(budget.class.as_str()) {
                return Err(bad(format!(
                    "drain budget `{}` declared twice",
                    budget.class
                )));
            }
            if !["halt", "stop-with-notice"].contains(&budget.on_exceed.as_str()) {
                return Err(bad(format!(
                    "drain budget `{}`: on_exceed `{}` is neither halt nor stop-with-notice \
                     (§14.4)",
                    budget.class, budget.on_exceed
                )));
            }
        }
        if !classes.contains("total") {
            return Err(bad(
                "no `total` drain budget. Without a ceiling on the whole drain, a rollout \
                 that never finishes never alerts either (§14.4)",
            ));
        }

        // The MTU probe is the plane's MTU less 20 bytes of IP header and 8 of
        // ICMP. Deriving it here rather than trusting the literal is what makes
        // the probe fail when the mesh is not carrying jumbo frames (§10.1).
        let mesh_mtu = self
            .network
            .mesh_class()
            .map(|c| c.mtu)
            .ok_or_else(|| bad("no mesh interface class declared (§3.1)"))?;
        let expected = mesh_mtu - 28;
        if self.policy.health.mesh_mtu_probe_bytes != expected {
            return Err(bad(format!(
                "mesh_mtu_probe_bytes is {} but the mesh MTU is {mesh_mtu}, so the probe \
                 should be {expected} (§10.1)",
                self.policy.health.mesh_mtu_probe_bytes
            )));
        }

        // Enrolment (§12.2). Every rule here is one that would otherwise be
        // found by an operator whose cluster did not come up.
        if self.policy.secret.is_empty() {
            return Err(bad(
                "no secrets are declared for enrolment. A cluster that needs none is a \
                 cluster that pulls from nothing, joins no tailnet and admits no SSH \
                 (§12.2)",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for secret in &self.policy.secret {
            if !ids.insert(secret.id.as_str()) {
                return Err(bad(format!("secret `{}`: declared twice", secret.id)));
            }
            // The value must never be in the model. A row carries a destination
            // and a description; anything that looks like a credential in one is
            // a credential in a public repository (§9.1, §12.2).
            for (field, value) in [
                ("description", &secret.description),
                ("enables", &secret.enables),
            ] {
                if looks_secret(value) {
                    return Err(bad(format!(
                        "secret `{}`: its {field} looks like a value rather than a \
                         description. This file is public, and a row says where a secret \
                         goes rather than what it is (§12.2)",
                        secret.id
                    )));
                }
            }
            if secret.is_stored() {
                if !secret.path.starts_with('/') {
                    return Err(bad(format!(
                        "secret `{}`: `{}` is not an absolute path",
                        secret.id, secret.path
                    )));
                }
                if !paths.insert(secret.path.as_str()) {
                    return Err(bad(format!(
                        "secret `{}`: two secrets are written to {}. The second would \
                         overwrite the first, and which one survived would depend on the \
                         order they were entered (§12.2)",
                        secret.id, secret.path
                    )));
                }
                let mode = secret.mode_bits().ok_or_else(|| {
                    bad(format!(
                        "secret `{}`: mode `{}` is not octal",
                        secret.id, secret.mode
                    ))
                })?;
                if mode & 0o022 != 0 {
                    return Err(bad(format!(
                        "secret `{}`: mode {} is writable beyond its owner. A credential \
                         any local user can rewrite is a credential any local user has \
                         (§12.2)",
                        secret.id, secret.mode
                    )));
                }
            } else if secret.apply == "none" {
                return Err(bad(format!(
                    "secret `{}` is neither stored nor applied, so entering it would do \
                     nothing at all (§12.2)",
                    secret.id
                )));
            }
            if !["none", "tailscale-up"].contains(&secret.apply.as_str()) {
                return Err(bad(format!(
                    "secret `{}`: apply `{}` is not an action the control plane knows \
                     (§12.2)",
                    secret.id, secret.apply
                )));
            }

            // How the value becomes the file (§12.2). The registry token was
            // written verbatim into a JSON document podman parses, so every
            // pull failed --- unattended, at the next update, with the cause
            // three layers from the symptom. A format the control plane does
            // not implement fails here instead.
            if !crate::policy::SECRET_FORMATS.contains(&secret.format.as_str()) {
                return Err(bad(format!(
                    "secret `{}`: format `{}` is not one the control plane can \
                     materialise. It knows {} (§12.2)",
                    secret.id,
                    secret.format,
                    crate::policy::SECRET_FORMATS.join(", ")
                )));
            }
            if secret.format == "docker-auth" {
                if secret.registry.is_empty() {
                    return Err(bad(format!(
                        "secret `{}`: a docker-auth document is keyed by registry and \
                         this row names none, so nothing could look the credential up \
                         (§12.2)",
                        secret.id
                    )));
                }
                if !secret.is_stored() {
                    return Err(bad(format!(
                        "secret `{}`: a docker-auth document is a file, and this row \
                         declares no destination for it (§12.2)",
                        secret.id
                    )));
                }
            } else if !secret.registry.is_empty() {
                return Err(bad(format!(
                    "secret `{}`: format `{}` builds no document, so the registry \
                     `{}` names nothing (§12.2)",
                    secret.id, secret.format, secret.registry
                )));
            }
            // `@` separates the format from the registry in the rendered row, so
            // one inside either field would split into fields nobody declared.
            if secret.format.contains('@') || secret.registry.contains('@') {
                return Err(bad(format!(
                    "secret `{}`: `@` separates the format from the registry in the \
                     rendered policy, and neither field may contain one (§12.2)",
                    secret.id
                )));
            }
            if secret.registry.contains('/') {
                return Err(bad(format!(
                    "secret `{}`: registry `{}` carries a path. A containers-auth \
                     document is keyed by host, and podman would not find a key with \
                     a path in it (§12.2)",
                    secret.id, secret.registry
                )));
            }
        }

        if self.policy.rollout.peer_health_port != self.policy.health.port {
            return Err(bad(format!(
                "the rollout predicate reads port {} but the health service is served on {} \
                 (§10.1, §13.2)",
                self.policy.rollout.peer_health_port, self.policy.health.port
            )));
        }
        Ok(())
    }

    /// Every peer of `node`, in ordinal order.
    pub fn peers_of(&self, node: &str) -> Vec<Node> {
        self.nodes()
            .into_iter()
            .filter(|n| n.name != node)
            .collect()
    }
}

impl Cluster {
    /// Substitute `{<node>.loopback}` with the address the ordinal derives.
    ///
    /// Addresses appear in `model/images.toml` --- a Quadlet's published port, an
    /// NFS export --- and writing them there literally would give every loopback
    /// two sources: the addressing arithmetic and whatever was typed next to the
    /// port. A placeholder that fails to resolve is a model error rather than a
    /// string that renders unchanged into a unit file nobody reads closely.
    ///
    /// The name is an ordinal slot (`node1`), not a machine. Which chassis is
    /// holding it is not a fact this substitution has, or needs.
    pub fn substitute(&self, text: &str) -> Result<String, ClusterError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after
                .find('}')
                .ok_or_else(|| bad(format!("`{text}`: an unclosed placeholder")))?;
            let name = &after[..close];
            let node = name.strip_suffix(".loopback").ok_or_else(|| {
                bad(format!(
                    "`{text}`: `{{{name}}}` is not a placeholder this renderer knows. \
                     Only `<node>.loopback` is substituted."
                ))
            })?;
            let slot = self
                .node(node)
                .ok_or_else(|| bad(format!("`{text}`: `{node}` is not an ordinal in the fleet")))?;
            out.push_str(&slot.loopback);
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Substitute `{policy.<field>}` with a threshold from `model/policy.toml`.
    ///
    /// The same reason `{node1.loopback}` exists: a retention written beside a
    /// command-line flag and again in the policy file would be two sources for
    /// one number, and the one that drifted would be the one nobody looked at.
    pub fn expand_policy(&self, text: &str) -> String {
        text.replace(
            "{policy.prometheus_retention_days}",
            &self.policy.gc.prometheus_retention_days.to_string(),
        )
    }

    /// Substitute, or report the model error as a panic at render time.
    ///
    /// The renderers are total by construction: [`Cluster::check`] validates
    /// every placeholder before anything is written, so a failure here means the
    /// check and the renderer disagree, which is worth failing loudly over.
    pub(crate) fn expand(&self, text: &str) -> String {
        self.substitute(text)
            .expect("check_variants validates every placeholder before rendering")
    }
}

/// Whether a string looks like a credential rather than a description of one.
///
/// Deliberately coarse and deliberately about *shape*: long runs of
/// base64-ish or hex characters, and the prefixes the three secrets this
/// cluster uses actually carry. A model file is public (§9.1), so the cost of a
/// false positive is rewording a sentence and the cost of a false negative is a
/// published credential.
fn looks_secret(text: &str) -> bool {
    if text.contains("ghp_") || text.contains("github_pat_") || text.contains("tskey-") {
        return true;
    }
    if text.contains("ssh-rsa ") || text.contains("ssh-ed25519 ") {
        return true;
    }
    // A long unbroken run of credential-shaped characters. Prose has spaces.
    text.split_whitespace().any(|word| {
        word.len() >= 32
            && word.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '/'
                    || c == '+'
                    || c == '='
                    || c == '_'
                    || c == '-'
            })
    })
}

/// A CIDR prefix, parsed only far enough to reject a typo.
struct IpPrefix;

impl std::str::FromStr for IpPrefix {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, len) = s.split_once('/').ok_or(())?;
        addr.parse::<std::net::Ipv4Addr>().map_err(|_| ())?;
        let len: u8 = len.parse().map_err(|_| ())?;
        if len > 32 {
            return Err(());
        }
        Ok(Self)
    }
}

fn read<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T, ClusterError> {
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path).map_err(|e| ClusterError::Io(path.clone(), e))?;
    toml::from_str(&text).map_err(|e| ClusterError::Parse(path, e))
}

/// The repository root, resolved from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cluster-model is two levels below the repository root")
        .to_path_buf()
}
