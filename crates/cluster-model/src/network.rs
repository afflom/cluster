//! The typed shape of `model/network.toml` (`SPEC.md` §3.2, §4).
//!
//! The route table is derived here and never declared. A transit hop written by
//! hand would be a second source for a fact the topology already determines,
//! which is what R1 forbids (§4.2).

use std::net::Ipv4Addr;

use serde::Deserialize;

/// `model/network.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkFile {
    /// The schema tag.
    pub spec: String,
    /// The prefix the management plane's declared ports are opened to (§4.4).
    pub lan_prefix: String,
    /// The three planes (§3.2).
    pub plane: Vec<Plane>,
    /// What shape the mesh is, and what it can grow to (§1.1).
    pub topology: Topology,
    /// The point-to-point links (§4.1).
    pub link: Vec<Link>,
    /// Forwarding and route metrics (§4.2).
    pub routing: Routing,
    /// The rendered firewall (§4.4).
    pub firewall: Firewall,
    /// Name suffixes for the rendered hosts file (§4.3).
    pub hosts: HostSuffixes,
}

/// One network plane.
#[derive(Debug, Clone, Deserialize)]
pub struct Plane {
    /// `mgmt`, `mesh`, or `bmc`.
    pub id: String,
    /// The interface roles carrying it, named as in [`crate::cluster::Macs`].
    pub interfaces: Vec<String>,
    /// The MTU every interface on this plane takes.
    pub mtu: u32,
    /// The default route, on the one plane that has one. The mesh has no
    /// gateway and must not acquire one (§14.2).
    #[serde(default)]
    pub gateway: Option<String>,
    /// Upstream resolvers. §4.3 declares no *cluster* DNS; `/etc/hosts` still
    /// answers first for every name this repository owns.
    #[serde(default)]
    pub dns: Vec<String>,
    /// What the plane is for.
    pub description: String,
}

/// The mesh's shape, and the ceiling that shape implies (§1.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Topology {
    /// `direct-triangle`. A switched mesh would be a different kind, and would
    /// invalidate §4.4's blanket trust of the mesh.
    pub kind: String,
    /// The node count this topology admits.
    pub max_nodes: u32,
}

/// A point-to-point link between two nodes (§4.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    /// Stable identifier, e.g. `l12`.
    pub id: String,
    /// A `/31` per RFC 3021: exactly two hosts, no network or broadcast address.
    pub prefix: String,
    /// The node taking the even address.
    pub a: String,
    /// The node taking the odd address.
    pub b: String,
    /// Which interface role on `a` carries it.
    pub a_interface: String,
    /// Which interface role on `b` carries it.
    pub b_interface: String,
}

/// The two addresses a `/31` link carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkAddresses {
    /// The even address, taken by [`Link::a`].
    pub a: Ipv4Addr,
    /// The odd address, taken by [`Link::b`].
    pub b: Ipv4Addr,
}

impl Link {
    /// The two host addresses, derived from the prefix.
    ///
    /// Returns `None` when the prefix is not a well-formed `/31`, which
    /// [`crate::Cluster::check`] turns into a model error rather than a panic.
    pub fn addresses(&self) -> Option<LinkAddresses> {
        let (addr, len) = self.prefix.split_once('/')?;
        if len != "31" {
            return None;
        }
        let base: Ipv4Addr = addr.parse().ok()?;
        let octets = u32::from(base);
        // A /31 is aligned on an even address; the odd one is its peer. An
        // unaligned prefix is a typo, not a smaller subnet.
        if !octets.is_multiple_of(2) {
            return None;
        }
        Some(LinkAddresses {
            a: Ipv4Addr::from(octets),
            b: Ipv4Addr::from(octets + 1),
        })
    }

    /// Does this link touch `node`?
    pub fn touches(&self, node: &str) -> bool {
        self.a == node || self.b == node
    }

    /// The peer on the other end of this link from `node`.
    pub fn peer_of(&self, node: &str) -> Option<&str> {
        if self.a == node {
            Some(&self.b)
        } else if self.b == node {
            Some(&self.a)
        } else {
            None
        }
    }

    /// `node`'s own address on this link.
    pub fn address_of(&self, node: &str) -> Option<Ipv4Addr> {
        let addrs = self.addresses()?;
        if self.a == node {
            Some(addrs.a)
        } else if self.b == node {
            Some(addrs.b)
        } else {
            None
        }
    }

    /// The interface role `node` carries this link on.
    pub fn interface_of(&self, node: &str) -> Option<&str> {
        if self.a == node {
            Some(&self.a_interface)
        } else if self.b == node {
            Some(&self.b_interface)
        } else {
            None
        }
    }
}

/// Forwarding and the two route metrics (§4.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Routing {
    /// On every node, so that a transit route has something to transit.
    pub ip_forward: bool,
    /// Metric of the direct route to a peer loopback.
    pub direct_metric: u32,
    /// Metric of the route via the remaining peer. `systemd-networkd` withdraws
    /// the direct route on carrier loss, so this one takes over with no daemon.
    pub transit_metric: u32,
}

/// The rendered firewall (§4.4).
#[derive(Debug, Clone, Deserialize)]
pub struct Firewall {
    /// Default policy on the input hook. `drop`.
    pub input_policy: String,
    /// Default policy on the forward hook.
    pub forward_policy: String,
    /// Default policy on the output hook.
    pub output_policy: String,
    /// Every accept is a declared one.
    pub rule: Vec<FirewallRule>,
}

/// One accepted flow.
#[derive(Debug, Clone, Deserialize)]
pub struct FirewallRule {
    /// The plane the rule applies on, or the pseudo-planes `tailscale` and `lo`.
    pub plane: String,
    /// `tcp`, `icmp`, or `any`.
    pub protocol: String,
    /// Destination port; `0` when the protocol has none.
    pub port: u16,
    /// `lan`, `tailnet`, `mesh`, or `any`.
    pub source: String,
    /// Restrict the rule to named nodes. Empty means every node.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Why the flow is accepted.
    pub comment: String,
}

impl FirewallRule {
    /// Does this rule render on `node`?
    pub fn applies_to(&self, node: &str) -> bool {
        self.nodes.is_empty() || self.nodes.iter().any(|n| n == node)
    }
}

/// Suffixes for the rendered hosts file (§4.3).
#[derive(Debug, Clone, Deserialize)]
pub struct HostSuffixes {
    /// Suffix for loopback names, e.g. `mesh` giving `n1.mesh`.
    pub mesh_suffix: String,
    /// Suffix for management names, e.g. `mgmt` giving `n1.mgmt`.
    pub mgmt_suffix: String,
}

impl NetworkFile {
    /// Every link touching `node`, in declaration order.
    pub fn links_of<'a>(&'a self, node: &str) -> Vec<&'a Link> {
        self.link.iter().filter(|l| l.touches(node)).collect()
    }

    /// The link joining two nodes, in either direction.
    pub fn link_between(&self, a: &str, b: &str) -> Option<&Link> {
        self.link
            .iter()
            .find(|l| (l.a == a && l.b == b) || (l.a == b && l.b == a))
    }

    /// The plane a named interface role belongs to.
    pub fn plane_of(&self, interface: &str) -> Option<&Plane> {
        self.plane
            .iter()
            .find(|p| p.interfaces.iter().any(|i| i == interface))
    }
}
