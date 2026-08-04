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
//! `10.10.0.0/16` would accept from addresses the model never assigned. It is
//! enumerable without knowing which machine is which, because every one of those
//! addresses is a function of an ordinal (§4.1).
//!
//! **Rules that hold for one role only are rendered into their own include.**
//! One image boots all three roles (§8.4), so there is one `nftables.conf`, and
//! putting the control plane's 443 accept in it would open that port on every
//! machine. `cluster-init` links the include for the role it discovered into
//! place before `nftables.service` starts. Every include is rendered and
//! diff-gated, so a role's ruleset is as much a model fact as the common one.

use std::net::Ipv4Addr;

use crate::render::{node_path, Rendered};
use crate::Cluster;

/// Where the role's include is expected, and what the common ruleset includes.
///
/// Under `/run` because a role is re-derived on every boot (§8.4). nft's
/// `include` of a missing path is an error, so `cluster-init` writes an empty
/// file for a role with no rules of its own rather than leaving nothing there.
pub const ROLE_INCLUDE: &str = "/run/cluster/nftables-role.conf";

pub(crate) fn render(c: &Cluster) -> Vec<Rendered> {
    let mut out = vec![common(c)];
    for role in &c.cluster.role {
        out.push(role_include(c, &role.id));
    }
    out
}

/// The ruleset every machine carries, whatever role it turns out to hold.
fn common(c: &Cluster) -> Rendered {
    let fw = &c.network.firewall;
    let mut body = String::new();

    body.push_str(&format!(
        "# The packet filter every node carries. Default {} on input; every accept\n\
         # is a declared rule in model/network.toml.\n\
         #\n\
         # One image boots all three roles, so this file is identical on all three\n\
         # machines. Rules true of one role only are in {ROLE_INCLUDE},\n\
         # which cluster-init links into place once the role is known (§8.4).\n\n",
        fw.input_policy
    ));
    body.push_str("flush ruleset\n\ntable inet cluster {\n");

    body.push_str("  set mesh {\n");
    body.push_str("    type ipv4_addr\n");
    body.push_str("    comment \"every link address and loopback the ordinals derive (§4.1)\"\n");
    body.push_str(&format!(
        "    elements = {{ {} }}\n",
        mesh_addresses(c)
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

    for rule in fw.rule.iter().filter(|r| r.is_universal()) {
        body.push_str(&format!("    # {}\n", rule.comment));
        body.push_str(&format!("    {}\n", input_rule(c, rule)));
    }

    body.push_str("\n    # Whatever this machine's role adds, and nothing if it adds none.\n");
    body.push_str(&format!("    include \"{ROLE_INCLUDE}\"\n"));
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

    Rendered::new(node_path("nftables.conf"), vec!["CD-03"], body)
}

/// The accepts that hold for one role only.
///
/// Rendered even when empty. `cluster-init` copies one of these to
/// [`ROLE_INCLUDE`], and nft treats an `include` of a missing file as an error
/// --- so a role with no rules of its own needs a file saying exactly that,
/// rather than an absence the ruleset would fail to load over.
fn role_include(c: &Cluster, role: &str) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# What the `{role}` role adds to the input chain (§4.4, §8.4).\n\
         #\n\
         # Copied to {ROLE_INCLUDE} by cluster-init once the role is\n\
         # known, and included from nftables.conf. Rendered even when it is empty:\n\
         # nft fails to load a ruleset that includes a file which is not there, so\n\
         # `no rules` has to be a file that says so.\n\n"
    ));

    let rules: Vec<&crate::FirewallRule> = c
        .network
        .firewall
        .rule
        .iter()
        .filter(|r| !r.is_universal() && r.applies_to(role))
        .collect();

    if rules.is_empty() {
        body.push_str("# This role adds nothing to what every node already accepts.\n");
    }
    for rule in rules {
        body.push_str(&format!("# {}\n", rule.comment));
        body.push_str(&format!("{}\n", input_rule(c, rule)));
    }

    Rendered::new(
        node_path(format!("nftables-role-{role}.conf")),
        vec!["CD-03", "CD-18"],
        body,
    )
}

/// Every address the mesh may speak from: each link's two addresses and each
/// ordinal's loopback, all of them derived (§4.1).
fn mesh_addresses(c: &Cluster) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for link in c.network.addressing.links(c.cluster.fleet.size) {
        out.extend(link.addresses());
    }
    for node in c.nodes() {
        if let Ok(addr) = node.loopback.parse::<Ipv4Addr>() {
            out.push(addr);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// One accept line, in nft's grammar.
fn input_rule(c: &Cluster, rule: &crate::FirewallRule) -> String {
    let saddr = match rule.source.as_str() {
        "lan" => format!("ip saddr {} ", c.network.lan_prefix),
        "mesh" => "ip saddr @mesh ".to_string(),
        // Discovery runs before any node has an address to be recognised by, so
        // it is keyed on the scope it is confined to rather than on a source set
        // that does not exist yet (§3.3).
        "link-local" => "ip6 saddr fe80::/10 ".to_string(),
        // The tailnet's address space is assigned by Tailscale and is not a
        // model fact; the interface is what identifies it (§4.5).
        "tailnet" => String::new(),
        _ => String::new(),
    };

    let iif = match rule.plane.as_str() {
        "lo" => "iif lo ".to_string(),
        "tailscale" => "iifname \"tailscale0\" ".to_string(),
        // A class is a property of the card, not a name nft can match: by the
        // time a packet reaches this chain the kernel name is whatever the
        // machine happened to enumerate. The source address is what the rule
        // keys on instead (§3.1).
        _ => String::new(),
    };

    let matcher = match rule.protocol.as_str() {
        "tcp" => format!("tcp dport {} ", rule.port),
        "udp" => format!("udp dport {} ", rule.port),
        "icmp" => "icmp type echo-request ".to_string(),
        _ => String::new(),
    };

    format!("{iif}{saddr}{matcher}accept")
}
