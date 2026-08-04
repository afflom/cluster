//! `ssh_config`, including the devcontainer alias (`SPEC.md` §11.1, §16.5).
//!
//! The `dc-<id>` alias resolves the session's *current* host from the control
//! plane, so a session that migrated during `n2`'s update (§14.3) is reachable
//! at the same alias without the client knowing it moved. Attached editor
//! sessions still drop --- there is no way around that short of CRIU, which is
//! not reliable for VS Code server, open TTYs and live SSH sockets --- but
//! reconnection is one command rather than a lookup.
//!
//! One file for every client, rendered once. It is deliberately usable without
//! the control plane: §16.5 makes the UI a management surface and not a
//! dependency, and `ssh dc-<id>` keeps working against the last known host when
//! `n1` is rebooting. Only migration-aware resolution degrades.

use crate::render::Rendered;
use crate::Cluster;

pub(crate) fn render(c: &Cluster) -> Rendered {
    let control = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared role");

    let mut body = String::new();
    body.push_str(
        "# Client SSH configuration. The primary client is a Chromebook, which\n\
         # cannot build images and should not be assumed to run anything but a\n\
         # browser and an SSH client: the developer's working environment is a\n\
         # devcontainer on n2, not the Chromebook (§11).\n\n",
    );

    body.push_str("Host *\n");
    body.push_str("  ServerAliveInterval 30\n");
    body.push_str("  ServerAliveCountMax 4\n");
    // Key-only, matching what the image's sshd accepts (§8.1). Offering a
    // password to a host that will not take one is a prompt the operator learns
    // to ignore.
    body.push_str("  PasswordAuthentication no\n");
    body.push_str("  KbdInteractiveAuthentication no\n\n");

    body.push_str("# The nodes themselves, by their tailnet names.\n");
    body.push_str("#\n");
    body.push_str("# Not by address: management addresses come from DHCP (§3.2), so there is\n");
    body.push_str("# no per-machine address left to write down. MagicDNS is what makes a node\n");
    body.push_str("# reachable at a stable name, and it works identically on the LAN and off\n");
    body.push_str("# it (§4.5), which the two entries this replaced did not.\n");
    for node in c.nodes() {
        body.push_str(&format!("\nHost {}\n", node.name));
        body.push_str(&format!("  HostName {}\n", tailnet_name(c, &node.name)));
        body.push_str("  User root\n");
    }

    body.push_str(&format!(
        "\n# Devcontainer sessions. The ProxyCommand asks the control plane which node\n\
         # currently hosts the session, so an alias survives a migration (§14.3).\n\
         #\n\
         # When the control plane is unreachable --- n1 rebooting, or the client off\n\
         # the tailnet --- the fallback resolves to the last known host recorded in\n\
         # ~/.ssh/cluster-sessions. The alias keeps working; only migration-aware\n\
         # resolution degrades (§16.5).\n\
         Host dc-*\n  \
           User vscode\n  \
           ProxyCommand sh -c 'host=$(curl --silent --max-time 3 \\\n    \
             \"http://{}:8080/api/sessions/$(echo %h | cut -c4-)/connect\" \\\n    \
             | jq -r .host 2>/dev/null); \\\n    \
             [ -n \"$host\" ] && [ \"$host\" != null ] || \\\n    \
             host=$(grep \"^%h \" ~/.ssh/cluster-sessions 2>/dev/null | cut -d\" \" -f2); \\\n    \
             exec nc \"$host\" 22'\n",
        tailnet_name(c, &control.name)
    ));

    Rendered::new("ssh_config", vec!["CD-10"], body)
}

/// A node's name on the tailnet, which is the only stable way a client reaches
/// one (§3.2, §4.5).
fn tailnet_name(c: &Cluster, name: &str) -> String {
    format!(
        "{name}.{}.{}",
        c.cluster.tailnet, c.cluster.magic_dns_suffix
    )
}
