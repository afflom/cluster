//! Quadlet units, rendered from the variant's declared containers
//! (`SPEC.md` §8.2, §8.3, §8.4).
//!
//! Quadlet files ship inside the image under `/usr/share/containers/systemd/`
//! and materialize as service units at boot --- the bootc grain exactly. Nothing
//! writes a unit onto a running node, so a node's service set is a property of
//! the image it booted rather than of whatever last ran on it.
//!
//! Every volume mount carries `:Z` or `:z`. The relabel is declared in
//! `model/images.toml` and rendered here, never hand-written, because a missing
//! relabel is an AVC denial at boot and §8.3 makes a denial a build failure
//! rather than a warning.

use crate::render::{section, Rendered};
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    let Some(variant) = c.images.variant_for(&node.name) else {
        return Vec::new();
    };
    let base = &c.images.base;

    let mut out = Vec::new();
    for quadlet in variant.all_quadlets(base) {
        let mut body = String::new();
        body.push_str(&format!("# {}\n\n", quadlet.description));

        let mut unit = vec![format!("Description={}", quadlet.description)];
        // Every one of these binds a mesh loopback or reaches the network, and
        // a unit that starts before networkd has configured the interface it
        // publishes on fails and stays failed --- which `cluster-health`'s
        // second check would then report for the life of the boot (§10.1).
        unit.push("After=network-online.target".to_string());
        unit.push("Wants=network-online.target".to_string());
        body.push_str(&section("Unit", &unit));

        let mut container = vec![format!("Image={}", quadlet.image)];
        container.push(format!("ContainerName={}", quadlet.name));
        for publish in &quadlet.publish {
            container.push(format!("PublishPort={}", c.expand(publish)));
        }
        if let Some(network) = &quadlet.network {
            container.push(format!("Network={network}"));
        }
        for mount in &quadlet.mount {
            container.push(format!("Volume={}", mount.volume_line()));
        }
        if !quadlet.exec.is_empty() {
            let args: Vec<String> = quadlet.exec.iter().map(|a| c.expand_policy(a)).collect();
            container.push(format!("Exec={}", args.join(" ")));
        }
        body.push_str(&section("Container", &container));

        body.push_str(&section(
            "Service",
            &[
                "Restart=always".to_string(),
                // A cold pull of a multi-hundred-megabyte image over the mesh
                // is slower than the default start timeout, and a unit that
                // times out on first boot looks identical to one that is broken.
                "TimeoutStartSec=300".to_string(),
            ],
        ));

        body.push_str(&section(
            "Install",
            &["WantedBy=multi-user.target default.target".to_string()],
        ));

        out.push(Rendered::new(
            format!(
                "{}/containers-systemd/{}.{}",
                node.name, quadlet.name, quadlet.kind
            ),
            vec!["CD-05"],
            body,
        ));
    }

    // Runner units are Quadlet-managed too, but they are not containers this
    // repository declares an image for: they are the Actions runner, registered
    // ephemerally and re-registered by the unit after each job. Drain is a
    // matter of not re-registering, which is why the unit and not a script owns
    // the loop (§9.5, §14.1).
    for runner in &variant.runner {
        let mut body = String::new();
        body.push_str(&format!(
            "# Actions runner {} on {}. --ephemeral: the runner exits after one\n\
             # job, so draining this node is a matter of not re-registering rather\n\
             # than of killing work in flight (§14.1).\n\n",
            runner.name, node.name
        ));

        let mut unit = vec![format!("Description=Actions runner {}", runner.name)];
        unit.push("After=network-online.target".to_string());
        unit.push("Wants=network-online.target".to_string());
        body.push_str(&section("Unit", &unit));

        let mut service = vec![
            "Type=simple".to_string(),
            format!("Environment=RUNNER_NAME={}", runner.name),
            format!("Environment=RUNNER_LABELS={}", runner.labels.join(",")),
            format!("Environment=RUNNER_EPHEMERAL={}", runner.ephemeral),
            "EnvironmentFile=/etc/cluster/runner.env".to_string(),
            "ExecStart=/usr/libexec/cluster/runner-loop".to_string(),
            "Restart=always".to_string(),
            "RestartSec=15".to_string(),
        ];
        if let Some(limit) = runner.concurrency {
            // One measurement at a time, nothing else. A concurrency lock in
            // the unit rather than a convention in a workflow, because a
            // convention is not observable from the node (§9.5).
            service.push(format!("Environment=RUNNER_CONCURRENCY={limit}"));
        }
        body.push_str(&section("Service", &service));
        body.push_str(&section(
            "Install",
            &["WantedBy=multi-user.target".to_string()],
        ));

        out.push(Rendered::new(
            format!(
                "{}/systemd/cluster-runner-{}.service",
                node.name, runner.name
            ),
            vec!["CD-05"],
            body,
        ));
    }

    // Network filesystem mounts, as `.mount` units named for their mount point.
    // Hard mounts stall and recover cleanly across `n1`'s reboot, which is what
    // §14.2 relies on; a soft mount would return errors to a devcontainer
    // mid-write instead.
    for mount in &variant.mount {
        // systemd's path escaping, which is not a plain slash-to-dash swap: a
        // literal `-` in a path component escapes to `\x2d` first, or
        // `/var/lib/devcontainer-home` and `/var/lib/devcontainer/home` would
        // name the same unit.
        let unit_name = format!(
            "{}.mount",
            mount
                .where_
                .trim_start_matches('/')
                .replace('-', "\\x2d")
                .replace('/', "-")
        );
        let mut body = String::new();
        body.push_str(&format!(
            "# {} on {}. Hard mount: it stalls and recovers cleanly across n1's\n\
             # reboot, where a soft mount would return errors to a devcontainer\n\
             # mid-write (§14.2).\n\n",
            c.expand(&mount.what),
            node.name
        ));
        body.push_str(&section(
            "Unit",
            &[
                format!("Description={} at {}", c.expand(&mount.what), mount.where_),
                "After=network-online.target".to_string(),
                "Wants=network-online.target".to_string(),
            ],
        ));
        body.push_str(&section(
            "Mount",
            &[
                format!("What={}", c.expand(&mount.what)),
                format!("Where={}", c.expand(&mount.where_)),
                format!("Type={}", mount.fstype),
                format!("Options={}", mount.options),
            ],
        ));
        body.push_str(&section(
            "Install",
            &["WantedBy=remote-fs.target".to_string()],
        ));

        out.push(Rendered::new(
            format!("{}/systemd/{unit_name}", node.name),
            vec!["CD-05"],
            body,
        ));
    }

    out
}
