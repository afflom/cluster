//! `systemd-networkd` units, and the route table derived from the topology
//! (`SPEC.md` §3.1, §4.1, §4.2).
//!
//! Two facts are load-bearing here and neither is written by hand.
//!
//! **Interfaces are matched by MAC.** A `.network` file that matched on a name
//! would bind whatever the kernel happened to enumerate as `enp3s0f0` that
//! boot. Matching on `MACAddress=` makes interface identity a model fact, so a
//! swapped cable or a replaced mainboard fails `CB-01` at boot instead of
//! producing a silently mis-wired mesh.
//!
//! **The transit route is computed, not declared.** In a triangle each node has
//! exactly one route to a peer that does not use the direct link: via the third
//! node. Deriving it from the topology means the failover path cannot disagree
//! with the wiring, which is what would happen if both were written down.

use crate::render::{section, Rendered};
use crate::{Cluster, Node};

/// The loopback file matches `Name=lo` rather than a MAC because the loopback
/// has no card. `CD-01` is worded to cover physical interfaces for exactly this
/// reason, and the exception is stated in the rendered file so a reader does
/// not have to infer it.
const LOOPBACK_UNIT: &str = "30-loopback.network";

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    let mut out = Vec::new();
    out.push(mgmt(c, node));
    out.extend(mesh(c, node));
    out.push(loopback(c, node));
    out.push(sysctl(c, node));
    out
}

fn mgmt(c: &Cluster, node: &Node) -> Rendered {
    let plane = c
        .network
        .plane
        .iter()
        .find(|p| p.id == "mgmt")
        .expect("the model check requires a mgmt plane");

    let mut body = String::new();
    body.push_str(&format!(
        "# {}'s management interface. {}\n\n",
        node.name, plane.description
    ));
    body.push_str(&section(
        "Match",
        &[format!("MACAddress={}", node.mac.mgmt)],
    ));
    body.push_str(&section("Link", &[format!("MTUBytes={}", plane.mtu)]));

    let mut network = vec![format!("Address={}", node.mgmt_address)];
    if let Some(gateway) = &plane.gateway {
        network.push(format!("Gateway={gateway}"));
    }
    for dns in &plane.dns {
        network.push(format!("DNS={dns}"));
    }
    // The hosts file answers first for every name this repository owns, so the
    // resolver above is only ever asked about the world outside (§4.3).
    network.push("IPv6AcceptRA=no".to_string());
    body.push_str(&section("Network", &network));

    Rendered::new(
        format!("{}/systemd-network/10-mgmt.network", node.name),
        vec!["CD-01"],
        body,
    )
}

fn mesh(c: &Cluster, node: &Node) -> Vec<Rendered> {
    let plane = c
        .network
        .plane
        .iter()
        .find(|p| p.id == "mesh")
        .expect("the model check requires a mesh plane");
    let routing = &c.network.routing;

    let mut out = Vec::new();
    // Ordered by interface role rather than by link declaration order, so that
    // the rendered filenames are stable when a link is reordered in the model.
    for (index, role) in ["mesh_a", "mesh_b"].into_iter().enumerate() {
        let Some(link) = c
            .network
            .links_of(&node.name)
            .into_iter()
            .find(|l| l.interface_of(&node.name) == Some(role))
        else {
            continue;
        };
        let mac = node
            .mac
            .get(role)
            .expect("the model check requires a MAC for every link interface");
        let own = link
            .address_of(&node.name)
            .expect("the model check requires a well-formed /31");
        let peer_name = link
            .peer_of(&node.name)
            .expect("a link touching a node names its peer");
        let peer = c
            .cluster
            .node(peer_name)
            .expect("the model check requires every link end to be a declared node");
        let peer_address = link
            .address_of(peer_name)
            .expect("the model check requires a well-formed /31");

        let mut body = String::new();
        body.push_str(&format!(
            "# {} -> {} over {role}, link {}. Direct-attached: no switch, so the\n\
             # segment has exactly two endpoints and §4.4 trusts it in full.\n\n",
            node.name, peer.name, link.id
        ));
        body.push_str(&section("Match", &[format!("MACAddress={mac}")]));
        body.push_str(&section("Link", &[format!("MTUBytes={}", plane.mtu)]));
        // A /31 carries no network or broadcast address, so both ends take a
        // host address on the same prefix (RFC 3021, §4.1).
        body.push_str(&section(
            "Network",
            &[
                format!("Address={own}/31"),
                "LinkLocalAddressing=no".to_string(),
                "IPv6AcceptRA=no".to_string(),
            ],
        ));

        // The direct route to this link's peer. `systemd-networkd` withdraws it
        // on carrier loss, which is the whole of the failover mechanism (§4.2).
        body.push_str(&format!(
            "# Direct route to {}. Withdrawn by networkd on carrier loss, at which\n\
             # point the metric-{} route on the other mesh interface takes over.\n",
            peer.name, routing.transit_metric
        ));
        body.push_str(&section(
            "Route",
            &[
                format!("Destination={}/32", peer.loopback),
                format!("Gateway={peer_address}"),
                format!("Metric={}", routing.direct_metric),
            ],
        ));

        // The transit route: every peer that is *not* this link's peer is
        // reachable through this link's peer, at the higher metric. In a
        // triangle that is exactly one destination, but the loop is written for
        // the topology rather than for the count.
        for other in c.peers_of(&node.name) {
            if other.name == peer.name {
                continue;
            }
            body.push_str(&format!(
                "# Transit to {} via {}. One hop more than the direct link, and only\n\
                 # selected when the direct route is gone.\n",
                other.name, peer.name
            ));
            body.push_str(&section(
                "Route",
                &[
                    format!("Destination={}/32", other.loopback),
                    format!("Gateway={peer_address}"),
                    format!("Metric={}", routing.transit_metric),
                ],
            ));
        }

        let filename = format!("{}-mesh-{}.network", 20 + index, &role[5..]);
        out.push(Rendered::new(
            format!("{}/systemd-network/{filename}", node.name),
            vec!["CD-01", "CD-02"],
            body,
        ));
    }
    out
}

fn loopback(_c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# {}'s mesh loopback. Every mesh service binds this address, which\n\
         # decouples service addressing from which link carries a flow and makes\n\
         # reachability assertions meaningful (§4.1).\n\
         #\n\
         # Matched by name and not by MAC: the loopback has no card. CD-01 is\n\
         # worded to cover physical interfaces for exactly this reason.\n\n",
        node.name
    ));
    body.push_str(&section("Match", &["Name=lo".to_string()]));
    body.push_str(&section(
        "Network",
        &[format!("Address={}/32", node.loopback)],
    ));
    Rendered::new(
        format!("{}/systemd-network/{LOOPBACK_UNIT}", node.name),
        vec!["CD-01"],
        body,
    )
}

fn sysctl(c: &Cluster, node: &Node) -> Rendered {
    let forward = u8::from(c.network.routing.ip_forward);
    let body = format!(
        "# Forwarding, so that a transit route has something to transit (§4.2).\n\
         # Without this a node in the middle of a failover path drops the packets\n\
         # its own route table told a peer to send it.\n\
         net.ipv4.ip_forward={forward}\n"
    );
    Rendered::new(
        format!("{}/sysctl.d/90-cluster.conf", node.name),
        vec!["CD-02"],
        body,
    )
}
