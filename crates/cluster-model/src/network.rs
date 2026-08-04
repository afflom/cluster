//! The typed shape of `model/network.toml` (`SPEC.md` §3.1, §3.2, §4).
//!
//! Two things are derived here that used to be declared, and both derivations
//! exist so that the same fact is not written down twice.
//!
//! **Addresses come from ordinals.** [`Addressing`] holds two bases and the
//! arithmetic; [`Link`] is computed from a pair of ordinals rather than parsed
//! from a table naming machines. Both ends of a cable reach the same answer
//! from the same two numbers, which is what lets a node address a link it
//! discovered rather than one it was told about (§4.1).
//!
//! **Routes come from the topology.** In a triangle each node has exactly one
//! route to a peer that does not use the direct link: via the third node. A
//! transit hop written by hand would be a second source for a fact the topology
//! already determines (§4.2).

use std::net::Ipv4Addr;

use serde::Deserialize;

/// `model/network.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkFile {
    /// The schema tag.
    pub spec: String,
    /// The prefix the LAN plane's declared ports are opened to (§4.4).
    pub lan_prefix: String,
    /// How a port is recognised as mesh or LAN (§3.1).
    pub class: Vec<Class>,
    /// What shape the mesh is, and what it can grow to (§1.1).
    pub topology: Topology,
    /// The arithmetic turning an ordinal into an address (§4.1).
    pub addressing: Addressing,
    /// Forwarding and route metrics (§4.2).
    pub routing: Routing,
    /// How a node learns which peer is on the far end of a cable (§3.3).
    pub discovery: Discovery,
    /// The rendered firewall (§4.4).
    pub firewall: Firewall,
    /// Where the runtime-written hosts file goes (§4.3).
    pub hosts: Hosts,
}

/// An interface class: how a port is recognised, and what it gets (§3.1).
///
/// There is no MAC here and no interface name. A port is `mesh` if its driver
/// reports support for 10GBase-T and `lan` otherwise --- a property of the card,
/// not of a file in a repository.
#[derive(Debug, Clone, Deserialize)]
pub struct Class {
    /// `mesh` or `lan`.
    pub id: String,
    /// The lowest *supported* link speed that puts a port in this class.
    ///
    /// Supported, never negotiated: a mesh port is down exactly when the peer it
    /// waits for has not booted, and a down port reports no speed at all.
    pub min_speed_mbps: u32,
    /// How many ports of this class a conforming machine presents. The
    /// classifier fails the boot on any other number rather than configuring
    /// what it found (§3.1).
    pub count: u32,
    /// The MTU every port in this class takes.
    pub mtu: u32,
    /// `derived` (from the ordinal, §4.1) or `dhcp`.
    pub addressing: String,
    /// What the class is for.
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

/// The arithmetic turning an ordinal into an address (§4.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Addressing {
    /// A node's loopback is this plus its ordinal.
    pub loopback_base: String,
    /// `32`. A loopback is a host route.
    pub loopback_prefix_len: u8,
    /// A link's prefix is this plus twice its index.
    pub link_base: String,
    /// `31` per RFC 3021: exactly two hosts, no network or broadcast address.
    pub link_prefix_len: u8,
}

impl Addressing {
    /// The loopback of ordinal `n` (§4.1).
    ///
    /// Returns `None` when the base is not a well-formed address, which
    /// [`crate::Cluster::check`] turns into a model error rather than a panic.
    pub fn loopback_of(&self, ordinal: u32) -> Option<Ipv4Addr> {
        let base: Ipv4Addr = self.loopback_base.parse().ok()?;
        Some(Ipv4Addr::from(u32::from(base).checked_add(ordinal)?))
    }

    /// The link between two ordinals, in either order (§4.1).
    ///
    /// The index is the position of the unordered pair in ascending order, so
    /// both ends compute the same prefix from the same two numbers without
    /// exchanging it. The lower ordinal takes the even address.
    pub fn link_between(&self, x: u32, y: u32, fleet_size: u32) -> Option<Link> {
        if x == y || x == 0 || y == 0 || x > fleet_size || y > fleet_size {
            return None;
        }
        let (lower, higher) = if x < y { (x, y) } else { (y, x) };
        let index = Self::pair_index(lower, higher, fleet_size)?;
        let base: Ipv4Addr = self.link_base.parse().ok()?;
        let prefix = u32::from(base).checked_add(2 * index)?;
        Some(Link {
            lower,
            higher,
            lower_address: Ipv4Addr::from(prefix),
            higher_address: Ipv4Addr::from(prefix.checked_add(1)?),
            prefix_len: self.link_prefix_len,
        })
    }

    /// The position of an unordered ordinal pair among all such pairs, in
    /// ascending order: `(1,2)` is 0, `(1,3)` is 1, `(2,3)` is 2.
    fn pair_index(lower: u32, higher: u32, fleet_size: u32) -> Option<u32> {
        let mut index = 0;
        for a in 1..=fleet_size {
            for b in (a + 1)..=fleet_size {
                if a == lower && b == higher {
                    return Some(index);
                }
                index += 1;
            }
        }
        None
    }

    /// Every link in the fleet, in ascending pair order.
    pub fn links(&self, fleet_size: u32) -> Vec<Link> {
        let mut out = Vec::new();
        for a in 1..=fleet_size {
            for b in (a + 1)..=fleet_size {
                if let Some(link) = self.link_between(a, b, fleet_size) {
                    out.push(link);
                }
            }
        }
        out
    }

    /// Every link touching ordinal `n`.
    pub fn links_of(&self, ordinal: u32, fleet_size: u32) -> Vec<Link> {
        self.links(fleet_size)
            .into_iter()
            .filter(|l| l.touches(ordinal))
            .collect()
    }
}

/// A point-to-point link between two ordinals (§4.1).
///
/// Derived, never parsed. There is no `[[link]]` table: a link is a function of
/// the two ordinals it joins, and both ends compute it independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The lower ordinal, which takes the even address.
    pub lower: u32,
    /// The higher ordinal, which takes the odd one.
    pub higher: u32,
    /// The even address.
    pub lower_address: Ipv4Addr,
    /// The odd address.
    pub higher_address: Ipv4Addr,
    /// `31`.
    pub prefix_len: u8,
}

impl Link {
    /// A stable identifier, e.g. `l12`.
    pub fn id(&self) -> String {
        format!("l{}{}", self.lower, self.higher)
    }

    /// The network prefix, e.g. `10.10.0.2/31`.
    pub fn prefix(&self) -> String {
        format!("{}/{}", self.lower_address, self.prefix_len)
    }

    /// Does this link touch `ordinal`?
    pub fn touches(&self, ordinal: u32) -> bool {
        self.lower == ordinal || self.higher == ordinal
    }

    /// The ordinal on the other end of this link.
    pub fn peer_of(&self, ordinal: u32) -> Option<u32> {
        if self.lower == ordinal {
            Some(self.higher)
        } else if self.higher == ordinal {
            Some(self.lower)
        } else {
            None
        }
    }

    /// `ordinal`'s own address on this link.
    pub fn address_of(&self, ordinal: u32) -> Option<Ipv4Addr> {
        if self.lower == ordinal {
            Some(self.lower_address)
        } else if self.higher == ordinal {
            Some(self.higher_address)
        } else {
            None
        }
    }

    /// Both addresses this link carries.
    pub fn addresses(&self) -> [Ipv4Addr; 2] {
        [self.lower_address, self.higher_address]
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

/// How a node learns which peer is on the far end of a cable (§3.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    /// The IPv6 link-local multicast group announcements go to. Scoped to one
    /// interface, and a direct-attached segment has exactly two endpoints, so
    /// the only listener is the peer.
    pub group: String,
    /// The UDP port.
    pub port: u16,
    /// How often an unpeered link re-announces.
    pub interval_ms: u32,
    /// How long a node waits before reporting a link unpeered. Longer than a
    /// cold boot, because the peer may not have been powered on yet (§12.1).
    pub timeout_s: u32,
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
    /// The interface class the rule applies on, or the pseudo-classes
    /// `tailscale` and `lo`.
    pub plane: String,
    /// `tcp`, `udp`, `icmp`, or `any`.
    pub protocol: String,
    /// Destination port; `0` when the protocol has none.
    pub port: u16,
    /// `lan`, `tailnet`, `mesh`, `link-local`, or `any`.
    pub source: String,
    /// Restrict the rule to named roles. Empty means every role.
    ///
    /// Roles rather than nodes: one image means one `nftables.conf`, so a rule
    /// true of one role only is rendered into its own include and linked into
    /// place once the role is known (§8.4).
    #[serde(default)]
    pub roles: Vec<String>,
    /// Why the flow is accepted.
    pub comment: String,
}

impl FirewallRule {
    /// Does this rule hold for every role, or only for some?
    pub fn is_universal(&self) -> bool {
        self.roles.is_empty()
    }

    /// Does this rule render for `role`?
    pub fn applies_to(&self, role: &str) -> bool {
        self.is_universal() || self.roles.iter().any(|r| r == role)
    }
}

/// Where the runtime-written hosts file goes (§4.3).
///
/// It cannot be rendered into the image: every entry depends on an ordinal the
/// image does not know. What the model declares is the path and the staging
/// path, so the write is atomic and the location is not hard-coded in a binary.
#[derive(Debug, Clone, Deserialize)]
pub struct Hosts {
    /// Where the file lands.
    pub path: String,
    /// Written here and renamed over `path`, so a node that loses power
    /// mid-write finds either the old file or the new one and never half of one.
    pub staging_path: String,
}

impl NetworkFile {
    /// An interface class by name.
    pub fn class(&self, id: &str) -> Option<&Class> {
        self.class.iter().find(|c| c.id == id)
    }

    /// The mesh class, which the model check requires to exist.
    pub fn mesh_class(&self) -> Option<&Class> {
        self.class("mesh")
    }

    /// The LAN class, which the model check requires to exist.
    pub fn lan_class(&self) -> Option<&Class> {
        self.class("lan")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressing() -> Addressing {
        Addressing {
            loopback_base: "10.10.255.0".to_string(),
            loopback_prefix_len: 32,
            link_base: "10.10.0.0".to_string(),
            link_prefix_len: 31,
        }
    }

    #[test]
    fn loopbacks_follow_the_ordinal() {
        let a = addressing();
        assert_eq!(a.loopback_of(1).unwrap().to_string(), "10.10.255.1");
        assert_eq!(a.loopback_of(3).unwrap().to_string(), "10.10.255.3");
    }

    /// The property the whole scheme rests on: both ends of a cable compute the
    /// same link from the same two ordinals, in either order, with nothing
    /// exchanged but the ordinals themselves (§4.1).
    #[test]
    fn both_ends_derive_the_same_link() {
        let a = addressing();
        for (x, y) in [(1, 2), (1, 3), (2, 3)] {
            let forward = a.link_between(x, y, 3).expect("a link exists");
            let backward = a.link_between(y, x, 3).expect("a link exists");
            assert_eq!(forward, backward, "the pair is unordered");
            assert_eq!(forward.address_of(x), backward.address_of(x));
        }
    }

    /// The lower ordinal takes the even address. This is the whole of the
    /// agreement: without it both ends would have to negotiate which is which.
    #[test]
    fn the_lower_ordinal_takes_the_even_address() {
        let a = addressing();
        for link in a.links(3) {
            let lower = u32::from(link.address_of(link.lower).unwrap());
            let higher = u32::from(link.address_of(link.higher).unwrap());
            assert!(lower.is_multiple_of(2), "{}: lower is even", link.id());
            assert_eq!(higher, lower + 1, "{}: the peer is its odd twin", link.id());
        }
    }

    #[test]
    fn a_triangle_has_three_links_and_no_address_repeats() {
        let a = addressing();
        let links = a.links(3);
        assert_eq!(links.len(), 3);
        let mut seen = std::collections::BTreeSet::new();
        for link in &links {
            for address in link.addresses() {
                assert!(seen.insert(address), "{address} is on two links");
            }
        }
        assert_eq!(
            links.iter().map(|l| l.prefix()).collect::<Vec<_>>(),
            ["10.10.0.0/31", "10.10.0.2/31", "10.10.0.4/31"],
            "§4.1's table, derived rather than declared"
        );
    }

    #[test]
    fn an_ordinal_outside_the_fleet_has_no_link() {
        let a = addressing();
        assert!(a.link_between(1, 4, 3).is_none());
        assert!(a.link_between(0, 1, 3).is_none());
        assert!(a.link_between(2, 2, 3).is_none(), "no link to itself");
    }
}
