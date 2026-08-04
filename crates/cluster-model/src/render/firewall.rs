//! `nftables.conf`, rendered from the declared rule set (`SPEC.md` §4.4).
//!
//! Default `drop` on input, and every accept below it is a row in
//! `model/network.toml`. The mesh is accepted in full because it is a
//! physically isolated L2 with exactly two endpoints per segment --- a property
//! of §4.1's topology, and the reason §1.1's three-node ceiling is load-bearing
//! rather than incidental. A switched mesh would make this file wrong.
//!
//! The mesh address set is enumerated rather than expressed as a subnet. The
//! link prefixes and the loopbacks are not contiguous, and a rule written as
//! `10.10.0.0/16` would accept from addresses the model never assigned.

use std::net::Ipv4Addr;

use crate::render::Rendered;
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Rendered {
    let fw = &c.network.firewall;
    let mut body = String::new();

    body.push_str(&format!(
        "# {}'s packet filter. Default {} on input; every accept is a declared\n\
         # rule in model/network.toml.\n\n",
        node.name, fw.input_policy
    ));
    body.push_str("flush ruleset\n\ntable inet cluster {\n");

    // Every address the mesh may speak from: the six link addresses and the
    // three loopbacks.
    let mut mesh_addrs: Vec<Ipv4Addr> = Vec::new();
    for link in &c.network.link {
        if let Some(addrs) = link.addresses() {
            mesh_addrs.push(addrs.a);
            mesh_addrs.push(addrs.b);
        }
    }
    for n in &c.cluster.node {
        if let Ok(addr) = n.loopback.parse::<Ipv4Addr>() {
            mesh_addrs.push(addr);
        }
    }
    mesh_addrs.sort_unstable();
    mesh_addrs.dedup();

    body.push_str("  set mesh {\n");
    body.push_str("    type ipv4_addr\n");
    body.push_str("    comment \"the six link addresses and three loopbacks (§4.1)\"\n");
    body.push_str(&format!(
        "    elements = {{ {} }}\n",
        mesh_addrs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    body.push_str("  }\n\n");

    // ---- input ----
    body.push_str("  chain input {\n");
    body.push_str(&format!(
        "    type filter hook input priority filter; policy {};\n\n",
        fw.input_policy
    ));
    body.push_str("    ct state established,related accept\n");
    body.push_str("    ct state invalid drop\n\n");

    for rule in fw.rule.iter().filter(|r| r.applies_to(&node.name)) {
        body.push_str(&format!("    # {}\n", rule.comment));
        body.push_str(&format!("    {}\n", input_rule(c, rule)));
    }
    body.push_str("  }\n\n");

    // ---- forward ----
    //
    // The forward policy is drop, but §4.2's transit routes need mesh-to-mesh
    // forwarding or a node in the middle of a failover path silently discards
    // the packets its own route table asked for. Accepting only when *both*
    // ends are in the mesh set keeps the hole exactly the size of the feature.
    body.push_str("  chain forward {\n");
    body.push_str(&format!(
        "    type filter hook forward priority filter; policy {};\n\n",
        fw.forward_policy
    ));
    body.push_str("    ct state established,related accept\n");
    body.push_str("    # Transit for §4.2's failover path, and nothing else: both ends must be\n");
    body.push_str("    # mesh addresses, so this forwards no traffic that arrived from the LAN.\n");
    body.push_str("    ip saddr @mesh ip daddr @mesh accept\n");
    body.push_str("  }\n\n");

    // ---- output ----
    body.push_str("  chain output {\n");
    body.push_str(&format!(
        "    type filter hook output priority filter; policy {};\n",
        fw.output_policy
    ));
    body.push_str("  }\n");

    body.push_str("}\n");

    Rendered::new(format!("{}/nftables.conf", node.name), vec!["CD-03"], body)
}

/// One accept line, in nft's grammar.
fn input_rule(c: &Cluster, rule: &crate::FirewallRule) -> String {
    let saddr = match rule.source.as_str() {
        "lan" => format!("ip saddr {} ", c.network.lan_prefix),
        "mesh" => "ip saddr @mesh ".to_string(),
        // The tailnet's address space is assigned by Tailscale and is not a
        // model fact; the interface is what identifies it (§4.5).
        "tailnet" => String::new(),
        _ => String::new(),
    };

    let iif = match rule.plane.as_str() {
        "lo" => "iif lo ".to_string(),
        "tailscale" => "iifname \"tailscale0\" ".to_string(),
        // A plane's interfaces are matched by MAC in networkd, so by the time a
        // packet reaches nft the kernel name is whatever networkd bound. The
        // source address is what the rule keys on instead.
        _ => String::new(),
    };

    let matcher = match rule.protocol.as_str() {
        "tcp" => format!("tcp dport {} ", rule.port),
        "icmp" => "icmp type echo-request ".to_string(),
        _ => String::new(),
    };

    format!("{iif}{saddr}{matcher}accept")
}
