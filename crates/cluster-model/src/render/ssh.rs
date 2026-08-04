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
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");

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

    body.push_str("# The nodes themselves, on the management plane. Reachable over Tailscale\n");
    body.push_str("# off-LAN with no change to these entries (§4.5).\n");
    for node in &c.cluster.node {
        let addr = node
            .mgmt_address
            .split_once('/')
            .map_or(node.mgmt_address.as_str(), |(a, _)| a);
        body.push_str(&format!("\nHost {}\n", node.name));
        body.push_str(&format!("  HostName {addr}\n"));
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
        control.loopback
    ));

    Rendered::new("ssh_config", vec!["CD-10"], body)
}
