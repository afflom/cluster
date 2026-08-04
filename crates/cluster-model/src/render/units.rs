//! The units and environment files that carry `model/policy.toml` onto a node
//! (`SPEC.md` §7.2, §13, §15.3).
//!
//! Every tunable that governs unattended behaviour renders here. The point of
//! putting them in the model rather than in a unit file is that they are the
//! numbers most likely to be tuned in anger at 2am: a poll interval, a drain
//! budget, a greenboot deadline. Scattered through units on three nodes they
//! would be three places to remember and one place to get wrong.
//!
//! The updater's environment file is also how a node learns its own rollout
//! position and its peers' health endpoints. Nothing on a node parses the model
//! at runtime (§7.2), so the ordering predicate's inputs arrive the same way
//! everything else does: rendered into the image.

use crate::render::{section, Rendered};
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    let mut out = vec![
        updater_env(c, node),
        updater_service(c, node),
        updater_timer(c, node),
        health_service(c, node),
        greenboot_check(c, node),
        gc_service(c, node),
        gc_timer(c, node),
    ];
    // Reclamation runs on the node that holds the session database and the
    // snapshots, and nowhere else (§15.3).
    if node.name == c.policy.drain.migration_target {
        out.push(reclaim_service(c, node));
        out.push(reclaim_timer(c, node));
    }
    // The control plane and the devcontainer agent are native units, not
    // Quadlets: both drive this node's own podman and filesystem, and both are
    // binaries the image already ships (§16.1, §15.1).
    if node.name == c.policy.drain.migration_target {
        out.push(control_plane_service(c, node));
    }
    if c.images
        .variant_for(&node.name)
        .is_some_and(|v| v.services.iter().any(|s| s == "devcontainer-agent.service"))
    {
        out.push(agent_service(c, node));
    }
    // The governor and IRQ affinity are neither kernel arguments nor anything
    // bootc sets, so the one variant that isolates CPUs carries a unit that
    // establishes them before the first measurement runs (§8.5).
    if let Some(isolation) = c
        .images
        .variant_for(&node.name)
        .and_then(|v| v.isolation.as_ref())
    {
        out.push(isolation_service(node, isolation));
    }
    out
}

/// `cluster-ctl`, the control plane (§16.1).
///
/// Published by `tailscale serve`, so it binds the mesh loopback and no port is
/// opened on the management plane. Authorization is the login list from
/// `model/cluster.toml`, rendered here --- who may drive the cluster is a model
/// fact under R1 like everything else (§16.2).
fn control_plane_service(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# The control plane on {}. Session registry, rollout state, and the API the\n\
         # web interface speaks to (§16.1).\n\
         #\n\
         # It binds the mesh loopback. `tailscale serve` publishes it with a real\n\
         # certificate from the tailnet's CA, so nothing is opened on the management\n\
         # plane and there is no password, session cookie, or auth code anywhere in\n\
         # this repository (§16.2).\n\n",
        node.name
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Cluster control plane".to_string(),
            "After=network-online.target var-lib-cluster\\x2dctl.mount".to_string(),
            "Wants=network-online.target".to_string(),
        ],
    ));

    let peers: Vec<String> = c
        .cluster
        .in_update_order()
        .into_iter()
        .map(|n| {
            format!(
                "{}:{}:http://{}:{}/health",
                n.name, n.update_position, n.loopback, c.policy.rollout.peer_health_port
            )
        })
        .collect();
    let r = &c.policy.reclaim;

    body.push_str(&section(
        "Service",
        &[
            "Type=simple".to_string(),
            format!("Environment=CLUSTER_CTL_BIND={}:8080", node.loopback),
            "Environment=CLUSTER_CTL_DB=/var/lib/cluster-ctl/sessions.db".to_string(),
            format!(
                "Environment=CLUSTER_AUTHORIZED_LOGINS={}",
                c.cluster.authorized_logins.join(",")
            ),
            format!("Environment=CLUSTER_PEERS={}", peers.join(",")),
            format!(
                "Environment=CLUSTER_RECLAIM_NOTIFY_DAYS={}",
                r.notify_after_days
            ),
            format!(
                "Environment=CLUSTER_RECLAIM_ARCHIVE_DAYS={}",
                r.archive_after_days
            ),
            format!(
                "Environment=CLUSTER_RECLAIM_PURGE_DAYS={}",
                r.purge_after_days
            ),
            format!(
                "Environment=CLUSTER_SNAPSHOT_REPOSITORY={}",
                r.snapshot_repository
            ),
            format!("Environment=CLUSTER_SNAPSHOT_TOOL={}", r.snapshot_tool),
            // Authorization (§16.2). The client ID is public by design and the
            // allowlist is the model's; nothing here is a secret, which is why
            // none of it is in §12.2's table.
            format!(
                "Environment=CLUSTER_GITHUB_USER_URL={}",
                c.cluster.github_app.user_url
            ),
            format!(
                "Environment=CLUSTER_GITHUB_CLIENT_ID={}",
                c.cluster.github_app.client_id
            ),
            format!(
                "Environment=CLUSTER_GITHUB_SCOPES={}",
                c.cluster.github_app.scopes.join(",")
            ),
            format!(
                "Environment=CLUSTER_GITHUB_DEVICE_CODE_URL={}",
                c.cluster.github_app.device_code_url
            ),
            format!(
                "Environment=CLUSTER_GITHUB_TOKEN_URL={}",
                c.cluster.github_app.token_url
            ),
            format!(
                "Environment=CLUSTER_AUTH_TOKEN_CACHE_TTL_S={}",
                c.policy.auth.token_cache_ttl_s
            ),
            format!(
                "Environment=CLUSTER_AUTH_VALIDATION_TIMEOUT_S={}",
                c.policy.auth.validation_timeout_s
            ),
            format!(
                "Environment=CLUSTER_ALLOWED_ORIGIN={}",
                c.policy.auth.allowed_origin
            ),
            "Environment=CLUSTER_WEB_ROOT=/var/lib/cluster-ctl/web".to_string(),
            format!(
                "Environment=CLUSTER_RECLAIM_SUSPEND_DURING_ROLLOUT={}",
                r.suspend_during_rollout
            ),
            format!(
                "Environment=CLUSTER_MIGRATION_TARGET={}",
                c.policy.drain.migration_target
            ),
            format!(
                "Environment=CLUSTER_MIGRATION_MEMORY_CAP_GIB={}",
                c.policy.drain.migration_memory_cap_gib
            ),
            format!(
                "Environment=CLUSTER_NEVER_RECEIVES={}",
                c.policy.drain.never_receives.join(",")
            ),
            format!(
                "Environment=CLUSTER_STOP_GRACE_S={}",
                c.policy.drain.container_stop_grace_s
            ),
            "ExecStart=/usr/bin/cluster-ctl serve".to_string(),
            "Restart=always".to_string(),
            "RestartSec=5".to_string(),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=multi-user.target".to_string()],
    ));
    Rendered::new(
        format!("{}/systemd/cluster-ctl.service", node.name),
        vec!["CD-08"],
        body,
    )
}

/// The devcontainer agent (§14.3, §15.1, §15.2).
///
/// It is the only thing that can answer whether a workspace is dirty, because it
/// is the only thing on the node that can see the worktree --- and §15.2 requires
/// that answer immediately before any destructive step, never from cache.
fn agent_service(c: &Cluster, node: &Node) -> Rendered {
    let storage = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");
    let runtime = c
        .images
        .variant_for(&node.name)
        .and_then(|v| c.images.runtime_of(v))
        .expect("the model check requires a declared runtime");

    let mut body = String::new();
    body.push_str(&format!(
        "# The devcontainer agent on {}. It starts and stops devcontainers, reports\n\
         # whether a workspace is dirty, and records an attachment (§15.1, §15.2).\n\
         #\n\
         # It is the only thing that can answer the dirty question, because it is the\n\
         # only thing that can see the worktree --- and §15.2 requires that answer\n\
         # immediately before any destructive step rather than from a cache.\n\n",
        node.name
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Devcontainer agent".to_string(),
            "After=network-online.target remote-fs.target".to_string(),
            "Wants=network-online.target".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=simple".to_string(),
            format!("Environment=AGENT_BIND={}:8081", node.loopback),
            format!("Environment=AGENT_NODE={}", node.name),
            "Environment=AGENT_WORKSPACES=/var/lib/devcontainers".to_string(),
            "Environment=AGENT_HOME=/var/lib/devcontainer-home".to_string(),
            format!("Environment=DOCKER_HOST={}", runtime.docker_host),
            format!(
                "Environment=AGENT_CONTROL_PLANE=http://{}:8080",
                storage.loopback
            ),
            format!(
                "Environment=AGENT_STOP_GRACE_S={}",
                c.policy.drain.container_stop_grace_s
            ),
            // The tunnel (§11.1). Added with `--additional-features`, so no
            // `devcontainer.json` is ever modified --- §1 puts that file out of
            // scope, and the tunnel belongs to this cluster, not to a project.
            format!(
                "Environment=AGENT_FEATURES={}",
                c.images
                    .variant_for(&node.name)
                    .map(|v| v.features.join(","))
                    .unwrap_or_default()
            ),
            format!(
                "Environment=AGENT_TUNNEL_PREFIX={}",
                c.policy.tunnel.name_prefix
            ),
            format!(
                "Environment=AGENT_TUNNEL_URL_TEMPLATE={}",
                c.policy.tunnel.url_template
            ),
            format!(
                "Environment=AGENT_TUNNEL_BACKOFF_INITIAL_S={}",
                c.policy.tunnel.restart_backoff_initial_s
            ),
            format!(
                "Environment=AGENT_TUNNEL_BACKOFF_MAX_S={}",
                c.policy.tunnel.restart_backoff_max_s
            ),
            format!(
                "Environment=AGENT_PORT_VISIBILITY={}",
                c.policy.tunnel.default_port_visibility
            ),
            "ExecStart=/usr/bin/devcontainer-agent serve".to_string(),
            "Restart=always".to_string(),
            "RestartSec=5".to_string(),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=multi-user.target".to_string()],
    ));
    Rendered::new(
        format!("{}/systemd/devcontainer-agent.service", node.name),
        vec!["CD-08"],
        body,
    )
}

/// Pin the governor and steer interrupts away from the isolated set (§8.5).
fn isolation_service(node: &Node, isolation: &crate::Isolation) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# CPUs {} are isolated by kernel argument; the governor and IRQ affinity
         # are not kernel arguments and are established here, before any workload
         # starts (§8.5).
         #
         # What this unit makes true is constructible and CB- carries it. That the
         # result yields stable measurements is neither constructible nor testable,
         # and §21.1 records why.

",
        isolation.isolated_cpus
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Pin the governor and steer interrupts off the isolated CPUs".to_string(),
            // Before anything a measurement could contend with.
            "Before=multi-user.target".to_string(),
            "DefaultDependencies=no".to_string(),
            "After=sysinit.target".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=oneshot".to_string(),
            "RemainAfterExit=yes".to_string(),
            format!(
                "ExecStart=/usr/bin/bash -c 'for g in \
                 /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo {} > $g; done'",
                isolation.governor
            ),
            format!(
                "ExecStart=/usr/bin/bash -c 'echo {} > /proc/irq/default_smp_affinity'",
                isolation.irq_affinity
            ),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=multi-user.target".to_string()],
    ));
    Rendered::new(
        format!("{}/systemd/cluster-isolation.service", node.name),
        vec!["CD-06"],
        body,
    )
}

/// Everything the updater needs to evaluate §13.2's predicate without reading
/// the model.
fn updater_env(c: &Cluster, node: &Node) -> Rendered {
    let r = &c.policy.rollout;
    let mut body = String::new();
    body.push_str(&format!(
        "# The rollout predicate's inputs for {}, rendered from model/policy.toml\n\
         # and model/cluster.toml. A node at position {} applies an update only when\n\
         # every earlier peer is already on the target and healthy, and every later\n\
         # peer is healthy (§13.2).\n\n",
        node.name, node.update_position
    ));

    body.push_str(&format!("CLUSTER_NODE={}\n", node.name));
    body.push_str(&format!("CLUSTER_ROLE={}\n", node.role));
    body.push_str(&format!(
        "CLUSTER_UPDATE_POSITION={}\n",
        node.update_position
    ));

    // Peers, with their position and health endpoint, in rollout order. The
    // predicate needs the ordering, so the ordering is what is rendered rather
    // than a set the updater would have to sort.
    let peers: Vec<String> = c
        .cluster
        .in_update_order()
        .into_iter()
        .filter(|n| n.name != node.name)
        .map(|n| {
            format!(
                "{}:{}:http://{}:{}/health",
                n.name, n.update_position, n.loopback, r.peer_health_port
            )
        })
        .collect();
    body.push_str(&format!("CLUSTER_PEERS={}\n", peers.join(",")));

    body.push_str(&format!("CLUSTER_POLL_INTERVAL_S={}\n", r.poll_interval_s));
    body.push_str(&format!("CLUSTER_POLL_JITTER_S={}\n", r.poll_jitter_max_s));
    body.push_str(&format!(
        "CLUSTER_RECHECK_BEFORE_APPLY_S={}\n",
        r.recheck_before_apply_s
    ));
    body.push_str(&format!(
        "CLUSTER_PEER_HEALTH_TIMEOUT_S={}\n",
        r.peer_health_timeout_s
    ));
    body.push_str(&format!("CLUSTER_REGISTRIES={}\n", r.registries.join(",")));
    body.push_str(&format!(
        "CLUSTER_IMAGE={}/{}\n",
        r.image_repository, node.name
    ));
    body.push_str(&format!("CLUSTER_STABLE_TAG={}\n", r.stable_tag));
    body.push_str(&format!(
        "CLUSTER_CONTROL_PLANE=http://{}:8080\n",
        c.cluster
            .node(&c.policy.drain.migration_target)
            .expect("the model check requires the migration target to be a declared node")
            .loopback
    ));

    body.push_str("\n# Drain budgets (§14.4). A budget is never met by force: exceeding one\n");
    body.push_str("# halts the rollout and asks for a human.\n");
    for budget in &c.policy.drain_budget {
        let key = budget.class.to_uppercase().replace('-', "_");
        body.push_str(&format!("CLUSTER_BUDGET_{key}_S={}\n", budget.budget_s));
        if let Some(total) = budget.total_budget_s {
            body.push_str(&format!("CLUSTER_BUDGET_{key}_TOTAL_S={total}\n"));
        }
        body.push_str(&format!(
            "CLUSTER_BUDGET_{key}_ON_EXCEED={}\n",
            budget.on_exceed
        ));
    }

    body.push_str("\n# Migration (§14.3).\n");
    body.push_str(&format!(
        "CLUSTER_MIGRATION_TARGET={}\n",
        c.policy.drain.migration_target
    ));
    body.push_str(&format!(
        "CLUSTER_MIGRATION_MEMORY_CAP_GIB={}\n",
        c.policy.drain.migration_memory_cap_gib
    ));
    body.push_str(&format!(
        "CLUSTER_NEVER_RECEIVES={}\n",
        c.policy.drain.never_receives.join(",")
    ));
    body.push_str(&format!(
        "CLUSTER_STOP_GRACE_S={}\n",
        c.policy.drain.container_stop_grace_s
    ));

    Rendered::new(
        format!("{}/systemd/cluster-updater.env", node.name),
        vec!["CD-08"],
        body,
    )
}

fn updater_service(_c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# One shot of the rollout predicate. The timer below decides how often;\n\
         # this unit decides nothing except what to run (§13.1).\n\n",
    );
    body.push_str(&section(
        "Unit",
        &[
            "Description=Evaluate the rollout predicate and apply if admitted".to_string(),
            "After=network-online.target".to_string(),
            "Wants=network-online.target".to_string(),
            // Applying an update while reclamation is snapshotting would put a
            // restic run and a reboot in the same window (§15.4).
            "Conflicts=cluster-reclaim.service".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=oneshot".to_string(),
            "EnvironmentFile=/etc/cluster/cluster-updater.env".to_string(),
            "ExecStart=/usr/bin/cluster-updater run".to_string(),
        ],
    ));
    Rendered::new(
        format!("{}/systemd/cluster-updater.service", node.name),
        vec!["CD-08"],
        body,
    )
}

fn updater_timer(c: &Cluster, node: &Node) -> Rendered {
    let r = &c.policy.rollout;
    let mut body = String::new();
    body.push_str(&format!(
        "# Poll every {}s with up to {}s of jitter. There is no webhook: nodes have\n\
         # no inbound reachability from GitHub, and polling adds no attack surface\n\
         # (§13.1). The jitter is what makes a simultaneous stale read unlikely\n\
         # rather than merely improbable in theory (§13.2).\n\n",
        r.poll_interval_s, r.poll_jitter_max_s
    ));
    body.push_str(&section(
        "Unit",
        &["Description=Poll for a promoted image".to_string()],
    ));
    body.push_str(&section(
        "Timer",
        &[
            format!("OnBootSec={}", r.poll_interval_s),
            format!("OnUnitActiveSec={}", r.poll_interval_s),
            format!("RandomizedDelaySec={}", r.poll_jitter_max_s),
            "Persistent=true".to_string(),
        ],
    ));
    body.push_str(&section("Install", &["WantedBy=timers.target".to_string()]));
    Rendered::new(
        format!("{}/systemd/cluster-updater.timer", node.name),
        vec!["CD-08"],
        body,
    )
}

fn health_service(c: &Cluster, node: &Node) -> Rendered {
    let h = &c.policy.health;
    let mut body = String::new();
    body.push_str(&format!(
        "# The health predicate, served on {}'s mesh loopback. This is how nodes\n\
         # observe each other without a lock: §13.2's ordering is a pure function\n\
         # of what every peer reports here (§10.1).\n\n",
        node.name
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Serve the cluster health predicate".to_string(),
            "After=network-online.target".to_string(),
            "Wants=network-online.target".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=simple".to_string(),
            format!(
                "ExecStart=/usr/bin/cluster-health serve --bind {}:{}",
                node.loopback, h.port
            ),
            "Restart=always".to_string(),
            "RestartSec=5".to_string(),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=multi-user.target".to_string()],
    ));
    Rendered::new(
        format!("{}/systemd/cluster-health.service", node.name),
        vec!["CD-08"],
        body,
    )
}

/// greenboot's required check. This is the single reason unattended update is
/// acceptable: a bad image costs one reboot cycle on one node rather than a
/// cluster (§13.3).
fn greenboot_check(c: &Cluster, node: &Node) -> Rendered {
    let g = &c.policy.greenboot;
    let control = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");

    let mut body = String::new();
    body.push_str(&format!(
        "#!/usr/bin/env bash\n\
         # greenboot's required check on {}. The boot is declared successful only if\n\
         # the health predicate passes within {}s; on failure greenboot rolls back to\n\
         # the previous ostree deployment and the node reboots into it (§13.3).\n\
         #\n\
         # This is not a mechanism this repository invents. It is cited (§20.1),\n\
         # configured here, and tested in T2.\n\
         set -euo pipefail\n\n",
        node.name, g.deadline_s
    ));
    body.push_str(&format!(
        "if timeout {}s /usr/bin/cluster-health check; then\n  exit 0\nfi\n\n",
        g.deadline_s
    ));
    body.push_str(&format!(
        "# The predicate failed. Record the digest as quarantined before the rollback\n\
         # reboots us, so that no other node attempts it (§13.4). Quarantine is a\n\
         # precondition in §13.2, which is what stops the rollout rather than merely\n\
         # reporting on it.\n\
         digest=\"$(bootc status --json | jq -r '.status.booted.image.imageDigest')\"\n\
         curl --silent --show-error --max-time 10 \\\n  \
           --request POST \"http://{}:8080/api/rollout/quarantine\" \\\n  \
           --header 'content-type: application/json' \\\n  \
           --data \"{{\\\"digest\\\":\\\"${{digest}}\\\",\\\"node\\\":\\\"{}\\\"}}\" || true\n\n\
         exit 1\n",
        control.loopback, node.name
    ));

    Rendered::new(
        format!(
            "{}/greenboot/{}",
            node.name,
            g.check_path
                .rsplit('/')
                .next()
                .expect("a check path names a file")
        ),
        vec!["CD-08"],
        body,
    )
}

fn gc_service(c: &Cluster, node: &Node) -> Rendered {
    let gc = &c.policy.gc;
    let mut body = String::new();
    body.push_str(&format!(
        "# Retention (§5.5). Container images older than {}h, and on the registry\n\
         # node an untagged manifest older than {} days.\n\n",
        gc.container_image_max_age_h, gc.registry_untagged_max_age_days
    ));
    body.push_str(&section(
        "Unit",
        &["Description=Reclaim container and registry storage".to_string()],
    ));

    let mut service = vec![
        "Type=oneshot".to_string(),
        format!(
            "ExecStart=/usr/bin/podman system prune --force --filter until={}h",
            gc.container_image_max_age_h
        ),
        // Two deployments: current and rollback (§5.5). bootc keeps that by
        // default, and this makes the number the model's rather than the
        // default's --- a third deployment would be one nothing has ever booted
        // and nothing would ever roll back to.
        format!(
            "ExecStart=/usr/bin/ostree admin prune --retain-only={}",
            gc.ostree_deployments_retained
        ),
    ];
    if !gc.measurement_output_pruned {
        // Measurement output is the only irreplaceable artifact the cluster
        // produces (§5.6), and nothing above touches it. Saying so in the unit
        // is what keeps a future ExecStart from being added by somebody who did
        // not read §5.5.
        service.push("Environment=CLUSTER_MEASUREMENT_OUTPUT_PRUNED=false".to_string());
    }
    if node.name == c.policy.drain.migration_target {
        service.push("ExecStart=/usr/libexec/cluster/zot-gc".to_string());
    }
    body.push_str(&section("Service", &service));

    Rendered::new(
        format!("{}/systemd/cluster-gc.service", node.name),
        vec!["CD-08"],
        body,
    )
}

fn gc_timer(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!("# {} (§5.5).\n\n", c.policy.gc.schedule));
    body.push_str(&section(
        "Unit",
        &["Description=Storage reclamation schedule".to_string()],
    ));
    body.push_str(&section(
        "Timer",
        &[
            format!("OnCalendar={}", c.policy.gc.schedule),
            // Three nodes waking at the same instant to prune would contend for
            // the same registry; the spread costs nothing.
            "RandomizedDelaySec=3600".to_string(),
            "Persistent=true".to_string(),
        ],
    ));
    body.push_str(&section("Install", &["WantedBy=timers.target".to_string()]));
    Rendered::new(
        format!("{}/systemd/cluster-gc.timer", node.name),
        vec!["CD-08"],
        body,
    )
}

fn reclaim_service(c: &Cluster, node: &Node) -> Rendered {
    let r = &c.policy.reclaim;
    let mut body = String::new();
    body.push_str(&format!(
        "# Devcontainer reclamation (§15.3). Notify at {} days, archive at {}, purge\n\
         # at {} --- and never purge a dirty workspace. Deleting someone's uncommitted\n\
         # work because a timer expired is a betrayal, and a system that does it once\n\
         # is never trusted again (§15.2).\n\n",
        r.notify_after_days, r.archive_after_days, r.purge_after_days
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Reclaim idle devcontainer sessions".to_string(),
            // Reclamation and drain are separate mechanisms with separate
            // triggers, and reclamation never runs during a rollout (§15.4).
            "Conflicts=cluster-updater.service".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=oneshot".to_string(),
            "ExecStart=/usr/bin/cluster-ctl reclaim run".to_string(),
        ],
    ));
    Rendered::new(
        format!("{}/systemd/cluster-reclaim.service", node.name),
        vec!["CD-08"],
        body,
    )
}

fn reclaim_timer(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!("# {} (§15.3).\n\n", c.policy.reclaim.schedule));
    body.push_str(&section(
        "Unit",
        &["Description=Devcontainer reclamation schedule".to_string()],
    ));
    body.push_str(&section(
        "Timer",
        &[
            format!("OnCalendar={}", c.policy.reclaim.schedule),
            "Persistent=true".to_string(),
        ],
    ));
    body.push_str(&section("Install", &["WantedBy=timers.target".to_string()]));
    Rendered::new(
        format!("{}/systemd/cluster-reclaim.timer", node.name),
        vec!["CD-08"],
        body,
    )
}
