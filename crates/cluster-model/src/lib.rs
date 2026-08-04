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
    ClusterFile, Disk, Firmware, GithubApp, Macs, Node, NodeStorage, Partition, Profile,
};
pub use images::{
    Base, ImagesFile, Isolation, Quadlet, QuadletMount, Registries, Runner, Runtime, Signing,
    Variant,
};
pub use network::{Firewall, FirewallRule, Link, LinkAddresses, NetworkFile, Plane, Routing};
pub use policy::{
    Alert, Auth, Drain, DrainBudget, Gc, Greenboot, Health, PolicyFile, Reclaim, Rollout, Tunnel,
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
    /// Nodes, roles, MACs, update positions, storage tiers.
    pub cluster: ClusterFile,
    /// Planes, links, routes, firewall.
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

    /// Cross-check the model against itself.
    ///
    /// Every rule here is one a rendered artifact would otherwise encode
    /// silently. A duplicate MAC renders two `.network` files that match the
    /// same card; an update position that is not a permutation renders a
    /// rollout that either never starts or starts twice. Catching those in the
    /// model is what keeps the renderers total.
    pub fn check(&self) -> Result<(), ClusterError> {
        self.check_spec()?;
        self.check_nodes()?;
        self.check_topology()?;
        self.check_firewall()?;
        self.check_variants()?;
        self.check_policy()?;
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

    /// Node identity: names, positions, loopbacks, MACs, profiles (§2, §3.1).
    fn check_nodes(&self) -> Result<(), ClusterError> {
        if self.cluster.node.is_empty() {
            return Err(bad("model/cluster.toml declares no nodes"));
        }

        let mut names = BTreeSet::new();
        let mut loopbacks = BTreeSet::new();
        let mut macs = BTreeSet::new();
        let mut positions = Vec::new();

        for node in &self.cluster.node {
            if !names.insert(node.name.as_str()) {
                return Err(bad(format!("{}: declared twice", node.name)));
            }
            if !loopbacks.insert(node.loopback.as_str()) {
                return Err(bad(format!(
                    "{}: loopback {} is already taken. Every mesh service binds \
                     its node's loopback, so a shared one is a silent collision (§4.1)",
                    node.name, node.loopback
                )));
            }
            if node.loopback.parse::<std::net::Ipv4Addr>().is_err() {
                return Err(bad(format!(
                    "{}: loopback `{}` is not an address",
                    node.name, node.loopback
                )));
            }
            for (role, mac) in node.mac.roles() {
                let normalised = mac.to_ascii_lowercase();
                if !is_mac(&normalised) {
                    return Err(bad(format!(
                        "{}.{role}: `{mac}` is not a MAC address",
                        node.name
                    )));
                }
                if !macs.insert(normalised) {
                    // Two `.network` files matching the same card is not a
                    // configuration error the node reports; it is one that
                    // produces an interface with an address nobody expected.
                    return Err(bad(format!(
                        "{}.{role}: MAC {mac} is declared more than once in the fleet (§3.1)",
                        node.name
                    )));
                }
            }
            if self
                .cluster
                .profile
                .iter()
                .all(|p| p.id != node.profile.as_str())
            {
                return Err(bad(format!(
                    "{}: names profile `{}`, which is not declared",
                    node.name, node.profile
                )));
            }
            positions.push(node.update_position);
        }

        // A permutation of 1..=n. Anything else is a rollout that either never
        // starts or has two nodes believing they are first (§13.2).
        positions.sort_unstable();
        let expected: Vec<u32> = (1..=self.cluster.node.len() as u32).collect();
        if positions != expected {
            return Err(bad(format!(
                "update positions are {positions:?}, which is not a permutation of {expected:?}. \
                 §13.2's predicate is true for exactly one node only when they are (§2.3)"
            )));
        }
        Ok(())
    }

    /// The mesh is the shape the topology says it is (§1.1, §4.1).
    fn check_topology(&self) -> Result<(), ClusterError> {
        let n = self.cluster.node.len() as u32;
        if n > self.network.topology.max_nodes {
            return Err(bad(format!(
                "{n} nodes, but topology `{}` admits {}. The direct mesh needs one 10 GbE \
                 port per peer and each node has two; a fourth node needs a switch and a \
                 different topology kind (§1.1)",
                self.network.topology.kind, self.network.topology.max_nodes
            )));
        }

        let mut prefixes = BTreeSet::new();
        for link in &self.network.link {
            if link.addresses().is_none() {
                return Err(bad(format!(
                    "link {}: `{}` is not an aligned /31. RFC 3021 point-to-point \
                     addressing is what §4.1 declares (§4.1)",
                    link.id, link.prefix
                )));
            }
            if !prefixes.insert(link.prefix.as_str()) {
                return Err(bad(format!("link {}: prefix declared twice", link.id)));
            }
            for end in [&link.a, &link.b] {
                if self.cluster.node(end).is_none() {
                    return Err(bad(format!(
                        "link {}: names node `{end}`, which is not declared",
                        link.id
                    )));
                }
            }
            if link.a == link.b {
                return Err(bad(format!(
                    "link {}: both ends are the same node",
                    link.id
                )));
            }
            for (node, interface) in [(&link.a, &link.a_interface), (&link.b, &link.b_interface)] {
                let plane = self.network.plane_of(interface).ok_or_else(|| {
                    bad(format!(
                        "link {}: interface `{interface}` belongs to no declared plane",
                        link.id
                    ))
                })?;
                if plane.id != "mesh" {
                    return Err(bad(format!(
                        "link {}: `{interface}` is on plane `{}`, and a mesh link must be \
                         on the mesh plane (§3.2)",
                        link.id, plane.id
                    )));
                }
                if self
                    .cluster
                    .node(node)
                    .and_then(|nd| nd.mac.get(interface))
                    .is_none()
                {
                    return Err(bad(format!(
                        "link {}: {node} declares no MAC for `{interface}` (§3.1)",
                        link.id
                    )));
                }
            }
        }

        // A direct triangle: every pair joined exactly once, and no node using
        // one of its two mesh ports twice.
        if self.network.topology.kind == "direct-triangle" {
            for a in &self.cluster.node {
                for b in &self.cluster.node {
                    if a.name >= b.name {
                        continue;
                    }
                    let joined = self
                        .network
                        .link
                        .iter()
                        .filter(|l| l.touches(&a.name) && l.touches(&b.name))
                        .count();
                    if joined != 1 {
                        return Err(bad(format!(
                            "{} and {} are joined by {joined} links; a direct triangle joins \
                             every pair exactly once (§4.1)",
                            a.name, b.name
                        )));
                    }
                }
                let mut used = BTreeSet::new();
                for link in self.network.links_of(&a.name) {
                    let interface = link
                        .interface_of(&a.name)
                        .expect("a link touching a node names its interface");
                    if !used.insert(interface.to_string()) {
                        return Err(bad(format!(
                            "{}: interface `{interface}` carries more than one link. Each \
                             node has two 10 GbE ports and a triangle needs both (§1.1)",
                            a.name
                        )));
                    }
                }
            }
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
            // `tailscale` and `lo` are pseudo-planes: one is an overlay
            // interface with no declared MAC, the other is not a plane at all.
            let known = ["tailscale", "lo"].contains(&rule.plane.as_str())
                || self.network.plane.iter().any(|p| p.id == rule.plane);
            if !known {
                return Err(bad(format!(
                    "firewall rule on plane `{}`, which is not declared",
                    rule.plane
                )));
            }
            for node in &rule.nodes {
                if self.cluster.node(node).is_none() {
                    return Err(bad(format!(
                        "firewall rule names node `{node}`, which is not declared"
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

        for node in &self.cluster.node {
            if self.images.variant_for(&node.name).is_none() {
                return Err(bad(format!(
                    "{}: no variant in model/images.toml builds an image for it",
                    node.name
                )));
            }
        }

        let mut isolated = Vec::new();
        for variant in &self.images.variant {
            if self.cluster.node(&variant.node).is_none() {
                return Err(bad(format!(
                    "variant {}: names node `{}`, which is not declared. R1 makes a \
                     dangling reference a build failure rather than a stale file (§17.2)",
                    variant.id, variant.node
                )));
            }
            if self.images.runtime_of(variant).is_none() {
                return Err(bad(format!(
                    "variant {}: runtime `{}` is not declared (§8.2)",
                    variant.id, variant.runtime
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

        // Isolation is `n3`'s alone. A second isolated variant would mean a node
        // that both measures and serves, which §2.3 exists to prevent.
        if isolated.len() > 1 {
            return Err(bad(format!(
                "variants {isolated:?} all declare CPU isolation; measurement is one node's \
                 job (§2.3)"
            )));
        }
        Ok(())
    }

    /// Drain, reclamation and health thresholds are internally coherent
    /// (§14, §15.3, §10.1).
    fn check_policy(&self) -> Result<(), ClusterError> {
        let drain = &self.policy.drain;
        if self.cluster.node(&drain.migration_target).is_none() {
            return Err(bad(format!(
                "migration_target `{}` is not a declared node",
                drain.migration_target
            )));
        }
        for node in &drain.never_receives {
            if self.cluster.node(node).is_none() {
                return Err(bad(format!(
                    "never_receives names `{node}`, which is not a declared node"
                )));
            }
            if node == &drain.migration_target {
                return Err(bad(format!(
                    "{node} is both the migration target and a node that never receives \
                     work (§2.3, §14.1)"
                )));
            }
            // A node that must receive nothing must also mount nothing: NFS
            // client activity, RPC timers and interrupt handling inject jitter
            // into exactly the quantity being measured (§2.3).
            if let Some(variant) = self.images.variant_for(node) {
                if let Some(mount) = variant.mount.first() {
                    return Err(bad(format!(
                        "{node} receives no migrated workload, but its variant mounts {} \
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
            .plane
            .iter()
            .find(|p| p.id == "mesh")
            .map(|p| p.mtu)
            .ok_or_else(|| bad("no mesh plane declared"))?;
        let expected = mesh_mtu - 28;
        if self.policy.health.mesh_mtu_probe_bytes != expected {
            return Err(bad(format!(
                "mesh_mtu_probe_bytes is {} but the mesh MTU is {mesh_mtu}, so the probe \
                 should be {expected} (§10.1)",
                self.policy.health.mesh_mtu_probe_bytes
            )));
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

    /// Every peer of `node`, in declaration order.
    pub fn peers_of<'a>(&'a self, node: &str) -> Vec<&'a Node> {
        self.cluster
            .node
            .iter()
            .filter(|n| n.name != node)
            .collect()
    }
}

impl Cluster {
    /// Substitute `{<node>.loopback}` with the address the model declares.
    ///
    /// Addresses appear in `model/images.toml` --- a Quadlet's published port, an
    /// NFS export --- and writing them there literally would give every loopback
    /// two sources: `model/cluster.toml` and whatever was typed next to the
    /// port. A placeholder that fails to resolve is a model error rather than a
    /// string that renders unchanged into a unit file nobody reads closely.
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
            let address = self
                .cluster
                .node(node)
                .ok_or_else(|| bad(format!("`{text}`: `{node}` is not a declared node")))?;
            out.push_str(&address.loopback);
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Substitute `{policy.<field>}` with a threshold from `model/policy.toml`.
    ///
    /// The same reason `{n1.loopback}` exists: a retention written beside a
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

/// Six colon-separated hex octets, and nothing else.
fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
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
