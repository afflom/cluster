//! The files a node writes about itself at boot (`SPEC.md` §3.1, §4.1, §4.3).
//!
//! These are the artifacts that could not be rendered: each one needs the
//! machine's ordinal, or the identity of the peer on the far end of a particular
//! cable, and one image boots all three ordinals (§8.4).
//!
//! They go under `/run/systemd/network/`, which `systemd-networkd` searches
//! before `/usr/lib/systemd/network/`. Nothing persists: a role and an ordinal
//! are re-derived on every boot from the machine's own hardware and the
//! registrar's answer, and a file that survived could outvote the machine it
//! describes.
//!
//! Every function here returns a `String`. Writing is the caller's job, so the
//! content of a unit is testable without a filesystem --- which matters, because
//! the content is where a wrong address would go.

use crate::addressing::{Addressing, Link};
use crate::links::{Classified, Port};

/// A mesh port and the peer discovered on the far end of it (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeeredPort {
    /// The port.
    pub port: Port,
    /// The ordinal of the machine at the other end of this cable.
    pub peer_ordinal: u32,
}

/// Route metrics, read from the rendered policy (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// The direct route to a peer loopback.
    pub direct: u32,
    /// The route via the remaining peer, taken over when networkd withdraws the
    /// direct one on carrier loss.
    pub transit: u32,
}

/// The `.network` unit for one mesh port.
///
/// Eight parameters, which clippy objects to and which is the right shape here:
/// every one of them is a fact this unit needs and none is derivable from the
/// others. Bundling them into a struct would name the bundle after this one
/// caller and hide that the peer's ordinal is discovered while the rest are
/// rendered --- which is the distinction the whole module is about.
///
/// Matched by **name**, and the name is the one this machine just enumerated ---
/// not a model fact and not persisted anywhere. That is the whole difference
/// from the withdrawn §3.1: the file is written by the machine that read the
/// name, so the two cannot disagree.
#[allow(clippy::too_many_arguments)]
pub fn mesh_unit(
    index: usize,
    peered: &PeeredPort,
    ordinal: u32,
    link: &Link,
    all_links: &[Link],
    addressing: &Addressing,
    mtu: u32,
    metrics: Metrics,
) -> String {
    let own = link
        .address_of(ordinal)
        .expect("the caller derived this link for this ordinal");
    let peer_address = link
        .address_of(peered.peer_ordinal)
        .expect("a link has both its ends");

    let mut body = format!(
        "# node{ordinal} -> node{} over {}, link l{}{}.\n\
         #\n\
         # Written at boot, not rendered: the interface name is whatever this machine\n\
         # enumerated, and the peer on the far end of this cable was discovered rather\n\
         # than declared (§3.1, §3.3). Direct-attached, so the segment has exactly two\n\
         # endpoints and §4.4 trusts it in full.\n\n",
        peered.peer_ordinal, peered.port.name, link.lower, link.higher
    );
    body.push_str(&format!("[Match]\nName={}\n\n", peered.port.name));
    body.push_str(&format!("[Link]\nMTUBytes={mtu}\n\n"));
    // A /31 carries no network or broadcast address, so both ends take a host
    // address on the same prefix (RFC 3021, §4.1).
    body.push_str("[Network]\n");
    body.push_str(&format!("Address={own}/{}\n", link.prefix_len));
    body.push_str("LinkLocalAddressing=ipv6\n");
    body.push_str("IPv6AcceptRA=no\n\n");

    body.push_str(&format!(
        "# Direct route to node{}. Withdrawn by networkd on carrier loss, at which\n\
         # point the metric-{} route on the other mesh port takes over (§4.2).\n",
        peered.peer_ordinal, metrics.transit
    ));
    body.push_str("[Route]\n");
    body.push_str(&format!(
        "Destination={}/{}\n",
        addressing
            .loopback_of(peered.peer_ordinal)
            .expect("the peer is in the fleet"),
        addressing.loopback_prefix_len
    ));
    body.push_str(&format!("Gateway={peer_address}\n"));
    body.push_str(&format!("Metric={}\n\n", metrics.direct));

    // The transit route: every ordinal that is not this link's peer and not this
    // node is reachable through this link's peer, one hop further out.
    for other in 1..=addressing.fleet_size {
        if other == ordinal || other == peered.peer_ordinal {
            continue;
        }
        // Only if the peer actually has a link to it, which in a triangle it
        // always does --- written as a lookup so the rule follows the topology
        // rather than the count.
        if !all_links
            .iter()
            .any(|l| l.peer_of(peered.peer_ordinal) == Some(other))
        {
            continue;
        }
        body.push_str(&format!(
            "# Transit to node{other} via node{}. One hop more than the direct link,\n\
             # and only selected when the direct route is gone.\n",
            peered.peer_ordinal
        ));
        body.push_str("[Route]\n");
        body.push_str(&format!(
            "Destination={}/{}\n",
            addressing
                .loopback_of(other)
                .expect("the ordinal is in the fleet"),
            addressing.loopback_prefix_len
        ));
        body.push_str(&format!("Gateway={peer_address}\n"));
        body.push_str(&format!("Metric={}\n\n", metrics.transit));
    }

    let _ = index;
    body
}

/// The file name a mesh unit takes, ordered before the LAN catch-all.
pub fn mesh_unit_name(index: usize) -> String {
    format!("2{index}-mesh.network")
}

/// The `.network` unit for the LAN port (§3.2).
///
/// DHCP, because there is no per-machine fact left to make it static from.
/// Nothing in the cluster reaches a node by its management address --- §4.3's
/// names are all on mesh loopbacks --- so it needs to be reachable, not
/// predictable.
pub fn lan_unit(port: &Port, mtu: u32) -> String {
    let mut body = String::from(
        "# The management port. DHCP: management addresses are not model facts and\n\
         # nothing in the cluster reaches a node by one (§3.2, §4.3).\n\n",
    );
    body.push_str(&format!("[Match]\nName={}\n\n", port.name));
    body.push_str(&format!("[Link]\nMTUBytes={mtu}\n\n"));
    body.push_str("[Network]\nDHCP=yes\nIPv6AcceptRA=no\n\n");
    // /etc/hosts answers first for every name this repository owns, so the
    // resolver DHCP hands out is only ever asked about the world outside (§4.3).
    body.push_str("[DHCPv4]\nUseDomains=no\n");
    body
}

/// Every LAN-class port beyond the first, left deliberately without an address.
///
/// The board has two 1GbE ports and §3.2 keeps the second as a spare for
/// physical fault diagnosis. Without this unit the catch-all would not exist and
/// networkd would leave it unmanaged, which looks the same until something else
/// starts managing it.
pub fn spare_unit(port: &Port) -> String {
    let mut body = String::from(
        "# A spare LAN port, deliberately without an address (§3.2). Kept for\n\
         # physical fault diagnosis. Configured as unmanaged rather than left\n\
         # undeclared, so that `no address here` is a decision in a file rather\n\
         # than an absence.\n\n",
    );
    body.push_str(&format!("[Match]\nName={}\n\n", port.name));
    body.push_str("[Link]\nUnmanaged=yes\n");
    body
}

/// The loopback every mesh service binds (§4.1).
pub fn loopback_unit(ordinal: u32, addressing: &Addressing) -> String {
    let address = addressing
        .loopback_of(ordinal)
        .expect("the caller holds an ordinal in the fleet");
    format!(
        "# This node's mesh loopback. Every mesh service binds it, which decouples\n\
         # service addressing from which link carries a flow and makes reachability\n\
         # assertions meaningful (§4.1).\n\
         #\n\
         # Matched by name and not by class: the loopback has no card.\n\n\
         [Match]\n\
         Name=lo\n\n\
         [Network]\n\
         Address={address}/{}\n",
        addressing.loopback_prefix_len
    )
}

/// The environment file every unit that needs this machine's identity reads.
///
/// The split this file embodies is the whole design in one place: fleet facts
/// are rendered and diff-gated, machine facts are discovered and written here,
/// and neither file contains the other's.
pub fn node_env(
    ordinal: u32,
    name: &str,
    role: &str,
    update_position: u32,
    loopback: &str,
) -> String {
    format!(
        "# What this machine worked out about itself at boot (§2.3, §4.1).\n\
         #\n\
         # Read alongside the rendered cluster-updater.env, which carries the fleet\n\
         # facts. Nothing here is in the image: one image boots all three ordinals.\n\
         CLUSTER_ORDINAL={ordinal}\n\
         CLUSTER_NODE={name}\n\
         CLUSTER_ROLE={role}\n\
         CLUSTER_UPDATE_POSITION={update_position}\n\
         CLUSTER_LOOPBACK={loopback}\n"
    )
}

/// The mesh units for every discovered port, with their file names.
pub fn mesh_units(
    peers: &[PeeredPort],
    ordinal: u32,
    addressing: &Addressing,
    mtu: u32,
    metrics: Metrics,
) -> Result<Vec<(String, String)>, crate::InitError> {
    let mut all_links = Vec::new();
    for a in 1..=addressing.fleet_size {
        for b in (a + 1)..=addressing.fleet_size {
            all_links.push(addressing.link_between(a, b)?);
        }
    }
    let mut out = Vec::new();
    for (index, peered) in peers.iter().enumerate() {
        let link = addressing.link_between(ordinal, peered.peer_ordinal)?;
        out.push((
            mesh_unit_name(index),
            mesh_unit(
                index, peered, ordinal, &link, &all_links, addressing, mtu, metrics,
            ),
        ));
    }
    Ok(out)
}

/// The units a machine writes for its ports, in file-name order.
pub fn all_units(
    classified: &Classified,
    peers: &[PeeredPort],
    ordinal: u32,
    addressing: &Addressing,
    mesh_mtu: u32,
    lan_mtu: u32,
    metrics: Metrics,
) -> Result<Vec<(String, String)>, crate::InitError> {
    let mut out = mesh_units(peers, ordinal, addressing, mesh_mtu, metrics)?;
    out.push((
        "30-loopback.network".to_string(),
        loopback_unit(ordinal, addressing),
    ));
    let mut lan = classified.lan.iter();
    if let Some(port) = lan.next() {
        out.push(("40-lan.network".to_string(), lan_unit(port, lan_mtu)));
    }
    for (index, port) in lan.enumerate() {
        out.push((format!("5{index}-spare.network"), spare_unit(port)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressing() -> Addressing {
        Addressing {
            loopback_base: "10.10.255.0".parse().unwrap(),
            loopback_prefix_len: 32,
            link_base: "10.10.0.0".parse().unwrap(),
            link_prefix_len: 31,
            fleet_size: 3,
        }
    }

    fn metrics() -> Metrics {
        Metrics {
            direct: 100,
            transit: 200,
        }
    }

    fn port(name: &str) -> Port {
        Port {
            name: name.to_string(),
            max_supported_mbps: 10000,
        }
    }

    fn peers() -> Vec<PeeredPort> {
        vec![
            PeeredPort {
                port: port("enp3s0f0"),
                peer_ordinal: 2,
            },
            PeeredPort {
                port: port("enp3s0f1"),
                peer_ordinal: 3,
            },
        ]
    }

    /// Ordinal 1's two mesh units, checked against §4.1's table.
    #[test]
    fn a_mesh_unit_carries_the_derived_addresses() {
        let units = mesh_units(&peers(), 1, &addressing(), 9000, metrics()).unwrap();
        assert_eq!(units.len(), 2);
        let (_, to_node2) = &units[0];
        assert!(to_node2.contains("Address=10.10.0.0/31"), "{to_node2}");
        assert!(to_node2.contains("Gateway=10.10.0.1"));
        assert!(to_node2.contains("Destination=10.10.255.2/32"));
        assert!(to_node2.contains("MTUBytes=9000"));
    }

    /// Matched by the name this machine enumerated. The withdrawn §3.1 matched
    /// on a MAC out of the model, which is what made a mainboard swap a code
    /// change.
    #[test]
    fn a_mesh_unit_matches_on_the_enumerated_name_and_no_mac() {
        let units = mesh_units(&peers(), 1, &addressing(), 9000, metrics()).unwrap();
        let (_, body) = &units[0];
        assert!(body.contains("Name=enp3s0f0"));
        assert!(
            !body.contains("MACAddress"),
            "no MAC appears anywhere: {body}"
        );
    }

    /// §4.2: two routes to every peer, and the transit one goes via the other
    /// peer at the higher metric.
    #[test]
    fn every_peer_has_a_direct_and_a_transit_route() {
        let units = mesh_units(&peers(), 1, &addressing(), 9000, metrics()).unwrap();
        let joined: String = units.iter().map(|(_, b)| b.as_str()).collect();
        for peer_loopback in ["10.10.255.2/32", "10.10.255.3/32"] {
            let direct = joined
                .matches(&format!("Destination={peer_loopback}"))
                .count();
            assert_eq!(
                direct, 2,
                "{peer_loopback} is reachable directly and in transit"
            );
        }
        assert_eq!(joined.matches("Metric=100").count(), 2);
        assert_eq!(joined.matches("Metric=200").count(), 2);
    }

    /// Discovery runs over IPv6 link-local, so the mesh port must keep it. An
    /// earlier revision rendered `LinkLocalAddressing=no`, which would leave the
    /// port with no address to discover a peer over at all.
    #[test]
    fn a_mesh_port_keeps_ipv6_link_local_for_discovery() {
        let units = mesh_units(&peers(), 1, &addressing(), 9000, metrics()).unwrap();
        let (_, body) = &units[0];
        assert!(body.contains("LinkLocalAddressing=ipv6"), "{body}");
    }

    /// The two ends of one cable produce complementary addresses. This is the
    /// derivation property expressed where it is finally used.
    #[test]
    fn the_two_ends_of_a_cable_produce_complementary_units() {
        let a = mesh_units(
            &[PeeredPort {
                port: port("eth0"),
                peer_ordinal: 3,
            }],
            2,
            &addressing(),
            9000,
            metrics(),
        )
        .unwrap();
        let b = mesh_units(
            &[PeeredPort {
                port: port("eth9"),
                peer_ordinal: 2,
            }],
            3,
            &addressing(),
            9000,
            metrics(),
        )
        .unwrap();
        assert!(a[0].1.contains("Address=10.10.0.4/31"));
        assert!(a[0].1.contains("Gateway=10.10.0.5"));
        assert!(b[0].1.contains("Address=10.10.0.5/31"));
        assert!(b[0].1.contains("Gateway=10.10.0.4"));
    }

    #[test]
    fn the_lan_port_takes_dhcp_and_the_spare_takes_nothing() {
        let classified = Classified {
            mesh: vec![port("enp3s0f0"), port("enp3s0f1")],
            lan: vec![
                Port {
                    name: "eno1".into(),
                    max_supported_mbps: 1000,
                },
                Port {
                    name: "eno2".into(),
                    max_supported_mbps: 1000,
                },
            ],
        };
        let units = all_units(
            &classified,
            &peers(),
            1,
            &addressing(),
            9000,
            1500,
            metrics(),
        )
        .unwrap();
        let names: Vec<&str> = units.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"40-lan.network"));
        assert!(names.contains(&"50-spare.network"));
        let lan = &units.iter().find(|(n, _)| n == "40-lan.network").unwrap().1;
        assert!(lan.contains("DHCP=yes"));
        let spare = &units
            .iter()
            .find(|(n, _)| n == "50-spare.network")
            .unwrap()
            .1;
        assert!(spare.contains("Unmanaged=yes"));
        assert!(!spare.contains("DHCP"), "the spare gets no address");
    }

    /// The mesh units must sort before the LAN one, so a mesh port is never
    /// caught by a broader match first.
    #[test]
    fn mesh_units_sort_before_the_lan_unit() {
        assert!(mesh_unit_name(0).as_str() < "40-lan.network");
        assert!(mesh_unit_name(1).as_str() < "40-lan.network");
    }

    #[test]
    fn the_node_env_carries_what_the_image_cannot() {
        let env = node_env(2, "node2", "compute", 2, "10.10.255.2");
        for expected in [
            "CLUSTER_ORDINAL=2",
            "CLUSTER_NODE=node2",
            "CLUSTER_ROLE=compute",
            "CLUSTER_UPDATE_POSITION=2",
            "CLUSTER_LOOPBACK=10.10.255.2",
        ] {
            assert!(env.contains(expected), "{expected} missing from {env}");
        }
    }
}
