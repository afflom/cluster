//! Configuration for the services on `lv_data` (`SPEC.md` §5.4, §5.5, §18).
//!
//! Every one of these is mounted read-only by a Quadlet. A container that mounts
//! an empty directory *starts and stays active*, which is worse than one that
//! fails: §10.1's fifth check asks whether the declared units are active, and an
//! inert Prometheus is active. So these render from the model like everything
//! else, and `check-wiring` fails if a read-only `/etc` mount has nothing behind
//! it.
//!
//! The alert rules are the case that matters most. `model/policy.toml` declares
//! fourteen alerts with their conditions and durations, and §18 says the
//! digest-drift, rollout-stalled and split-version ones are *what make §13
//! observable* --- unattended automation whose failures are invisible is worse
//! than manual updates. Declaring them and rendering nothing would leave exactly
//! that invisibility, with a model that claimed otherwise.

use crate::render::{section, Rendered};
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    // Only the node that runs them. Rendering a Prometheus configuration onto
    // the measurement node would be a file that exists to be ignored.
    if node.name != c.policy.drain.migration_target {
        return vec![journald(c, node)];
    }
    vec![
        journald(c, node),
        zot(c, node),
        prometheus(c, node),
        alert_rules(c, node),
        alertmanager(c, node),
        garage(c, node),
        nfs_exports(c, node),
        grafana_datasource(c, node),
        tailscale_serve(c, node),
        tailscale_acl(c, node),
    ]
}

/// Grafana's datasource (§18).
///
/// Provisioned rather than clicked. A Grafana with no datasource starts, reports
/// itself active to §10.1's fifth check, and shows an operator nothing --- which
/// is the same failure mode as the empty `/etc/prometheus` mount, arriving by a
/// different route.
fn grafana_datasource(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# Grafana's datasource on {}. Provisioned rather than clicked: a Grafana\n\
         # with no datasource starts, reports itself active, and shows nothing ---\n\
         # which §10.1's fifth check cannot tell from working (§18).\n\n",
        node.name
    ));
    body.push_str("apiVersion: 1\n\n");
    body.push_str("datasources:\n");
    body.push_str("  - name: Prometheus\n");
    body.push_str("    type: prometheus\n");
    body.push_str("    access: proxy\n");
    body.push_str(&format!("    url: http://{}:9090\n", node.loopback));
    body.push_str("    isDefault: true\n");
    body.push_str(&format!(
        "    jsonData: {{ timeInterval: \"15s\", timeSeriesRetention: \"{}d\" }}\n",
        c.policy.gc.prometheus_retention_days
    ));
    Rendered::new(
        format!(
            "{}/grafana/provisioning/datasources/prometheus.yml",
            node.name
        ),
        vec!["CD-12"],
        body,
    )
}

/// Publish the control plane on the tailnet (§16.2).
///
/// The control plane binds the mesh loopback, which is reachable from the other
/// two nodes and from nowhere an operator is. `tailscale serve` is what closes
/// that: a real certificate from the tailnet's CA, no inbound port opened on the
/// management plane, and the caller's identity in a request header --- which is
/// the whole of this repository's authentication.
///
/// Without this unit the API is bound and unreachable, and the web interface
/// renders its disconnected state forever while every gate stays green.
fn tailscale_serve(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# Publishes the control plane at https://{}.{}.ts.net (§16.2).\n\
         #\n\
         # The API binds the mesh loopback, which is reachable from the other two\n\
         # nodes and from nowhere an operator is. This is what closes that gap, and\n\
         # it opens no inbound port on the management plane: Tailscale is\n\
         # outbound-initiated (§16.4).\n\n",
        node.name, c.cluster.tailnet
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Publish the control plane on the tailnet".to_string(),
            "After=tailscaled.service cluster-ctl.service".to_string(),
            "Requires=cluster-ctl.service".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=oneshot".to_string(),
            "RemainAfterExit=yes".to_string(),
            format!(
                "ExecStart=/usr/bin/tailscale serve --bg --https=443 http://{}:8080",
                node.loopback
            ),
            "ExecStop=/usr/bin/tailscale serve --https=443 off".to_string(),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=multi-user.target".to_string()],
    ));
    Rendered::new(
        format!("{}/systemd/tailscale-serve.service", node.name),
        vec!["CD-15", "CC-06"],
        body,
    )
}

/// The tailnet's access policy (§4.5).
///
/// Applied to the tailnet by an operator rather than by a node --- it is
/// tailnet-side configuration, like the kickstart is installer-side. Rendered
/// here so that who may reach the cluster is a model fact under R1 and not a
/// setting somebody once typed into a web console.
///
/// The mesh is never advertised. Only the management subnet is, and only from
/// the node that §4.5 says advertises it.
fn tailscale_acl(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# The tailnet access policy (§4.5). Applied by an operator, not by a node:\n\
         # this is tailnet-side configuration in the way the kickstart is\n\
         # installer-side, and it is rendered so that who may reach the cluster is a\n\
         # model fact rather than something typed into a console once.\n\
         #\n\
         # The mesh is never advertised. Only the management subnet is, and only from\n\
         # the node §4.5 says advertises it.\n",
    );
    body.push_str("{\n");
    body.push_str("  \"tagOwners\": {\n");
    let owners = c
        .cluster
        .authorized_logins
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(", ");
    body.push_str(&format!("    \"tag:cluster\": [{owners}]\n"));
    body.push_str("  },\n");
    body.push_str("  \"acls\": [\n");
    // Only the operator's devices reach the cluster, and only on the ports §4.4
    // opens on the Tailscale plane.
    body.push_str("    {\n");
    body.push_str(&format!(
        "      \"action\": \"accept\",\n      \"src\": [{owners}],\n"
    ));
    body.push_str("      \"dst\": [\"tag:cluster:22\", \"tag:cluster:443\"]\n");
    body.push_str("    }\n");
    body.push_str("  ],\n");
    body.push_str(&format!(
        "  \"autoApprovers\": {{ \"routes\": {{ \"{}\": [\"tag:cluster\"] }} }}\n",
        c.network.lan_prefix
    ));
    body.push_str("}\n");
    let _ = node;
    Rendered::new("tailscale/policy.hujson", vec!["CD-15"], body)
}

/// `SystemMaxUse` (§5.5).
///
/// Rendered on every node: a journal that fills `/var` takes container graph
/// storage down with it, and `/var` is the one writable filesystem the bootc
/// contract gives us (§5.2).
fn journald(c: &Cluster, node: &Node) -> Rendered {
    let body = format!(
        "# Journal retention (§5.5). Rendered on every node because a journal that\n\
         # fills /var takes container graph storage with it, and /var is the one\n\
         # writable filesystem the bootc contract provides (§5.2).\n\n\
         [Journal]\n\
         SystemMaxUse={}\n\
         Storage=persistent\n",
        c.policy.gc.journald_max_use
    );
    Rendered::new(
        format!("{}/journald.conf.d/10-cluster.conf", node.name),
        vec!["CD-12"],
        body,
    )
}

/// Zot: this repository's images, mirrored from GHCR, with pull-through caches
/// (§5.4).
fn zot(c: &Cluster, node: &Node) -> Rendered {
    let r = &c.images.registries;
    let repository = &c.images.signing.repository;

    let mut body = String::new();
    body.push_str(&format!(
        "# The registry on {}. It hosts this repository's images, mirrors them from\n\
         # GHCR every five minutes, and pull-through caches the declared upstreams\n\
         # so a node's pull does not leave the mesh when it does not have to (§5.4).\n\
         #\n\
         # Bound to the mesh loopback: the registry is a mesh service and the\n\
         # management plane's firewall does not open its port (§4.4).\n",
        node.name
    ));
    body.push_str("{\n");
    body.push_str("  \"distSpecVersion\": \"1.1.0\",\n");
    body.push_str("  \"storage\": { \"rootDirectory\": \"/var/lib/registry\", \"gc\": true,\n");
    body.push_str(&format!(
        "    \"gcDelay\": \"{}h\", \"gcInterval\": \"24h\" }},\n",
        c.policy.gc.registry_untagged_max_age_days * 24
    ));
    body.push_str(&format!(
        "  \"http\": {{ \"address\": \"{}\", \"port\": \"{}\" }},\n",
        node.loopback, r.port
    ));
    body.push_str("  \"log\": { \"level\": \"info\" },\n");
    body.push_str("  \"extensions\": {\n");
    body.push_str("    \"sync\": {\n");
    body.push_str("      \"enable\": true,\n");
    body.push_str("      \"registries\": [\n");
    body.push_str("        {\n");
    body.push_str("          \"urls\": [\"https://ghcr.io\"],\n");
    body.push_str("          \"onDemand\": false,\n");
    body.push_str("          \"pollInterval\": \"5m\",\n");
    body.push_str(&format!(
        "          \"content\": [{{ \"prefix\": \"/{repository}/**\" }}]\n"
    ));
    body.push_str("        },\n");
    // Pull-through, on demand: an upstream is cached when something asks for it
    // rather than mirrored wholesale, which is the difference between a cache
    // and a second copy of Docker Hub.
    for (index, fallback) in r.fallbacks.iter().enumerate() {
        let comma = if index + 1 == r.fallbacks.len() {
            ""
        } else {
            ","
        };
        body.push_str("        {\n");
        body.push_str(&format!("          \"urls\": [\"https://{fallback}\"],\n"));
        body.push_str("          \"onDemand\": true,\n");
        body.push_str("          \"tlsVerify\": true\n");
        body.push_str(&format!("        }}{comma}\n"));
    }
    body.push_str("      ]\n");
    body.push_str("    }\n");
    body.push_str("  }\n");
    body.push_str("}\n");

    Rendered::new(
        format!("{}/zot/config.json", node.name),
        vec!["CD-12", "CS-04"],
        body,
    )
}

/// Prometheus, scraping the mesh (§18).
fn prometheus(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# Scrape configuration for {}. Every target is a mesh loopback: the metrics\n\
         # path crosses the mesh, not the management plane, so a scrape does not\n\
         # depend on the LAN being up (§18).\n\n",
        node.name
    ));
    body.push_str("global:\n");
    body.push_str("  scrape_interval: 15s\n");
    body.push_str("  evaluation_interval: 15s\n\n");
    body.push_str("rule_files:\n  - /etc/prometheus/alerts.yml\n\n");
    body.push_str("alerting:\n");
    body.push_str("  alertmanagers:\n");
    body.push_str(&format!(
        "    - static_configs:\n        - targets: [\"{}:9093\"]\n\n",
        node.loopback
    ));
    body.push_str("scrape_configs:\n");

    body.push_str("  - job_name: node\n");
    body.push_str("    static_configs:\n");
    for n in &c.cluster.node {
        body.push_str(&format!(
            "      - targets: [\"{}:9100\"]\n        labels: {{ node: \"{}\", role: \"{}\" }}\n",
            n.loopback, n.name, n.role
        ));
    }

    // The health predicate is scraped as well as polled. §18's digest-drift and
    // split-version alerts are computed from what nodes report about themselves,
    // and that is the same document §13.2 reads.
    body.push_str("\n  - job_name: cluster-health\n");
    body.push_str("    metrics_path: /health\n");
    body.push_str("    static_configs:\n");
    for n in &c.cluster.node {
        body.push_str(&format!(
            "      - targets: [\"{}:{}\"]\n        labels: {{ node: \"{}\" }}\n",
            n.loopback, c.policy.health.port, n.name
        ));
    }

    Rendered::new(
        format!("{}/prometheus/prometheus.yml", node.name),
        vec!["CD-12"],
        body,
    )
}

/// The alerts §18 declares, rendered from `model/policy.toml`.
///
/// The digest-drift, rollout-stalled and split-version rules are what make §13
/// observable. Unattended automation whose failures are invisible is worse than
/// manual updates, so an alert declared in the model and rendered nowhere would
/// be precisely the invisibility the model claims to have removed.
fn alert_rules(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# The alerts §18 declares. Each carries the condition and the duration from\n\
         # model/policy.toml, so tuning one is a model change and a re-render rather\n\
         # than an edit on a node.\n\
         #\n\
         # There is no paging integration: this is a three-node lab cluster, and an\n\
         # alert that wakes someone is worse than a dashboard they check (§18).\n\n",
    );
    body.push_str("groups:\n  - name: cluster\n    rules:\n");

    for alert in &c.policy.alert {
        let name = alert
            .id
            .split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<String>();
        body.push_str(&format!("      - alert: {name}\n"));
        body.push_str(&format!("        expr: {}\n", expression(c, &alert.id)));
        if alert.for_s > 0 {
            body.push_str(&format!("        for: {}s\n", alert.for_s));
        }
        body.push_str(&format!(
            "        labels: {{ severity: {} }}\n",
            alert.severity
        ));
        body.push_str(&format!(
            "        annotations: {{ summary: \"{}\" }}\n",
            alert.condition.replace('"', "'")
        ));
    }

    Rendered::new(
        format!("{}/prometheus/alerts.yml", node.name),
        vec!["CD-12", "CD-13"],
        body,
    )
}

/// The PromQL an alert's declared condition becomes.
///
/// The model states the condition in the register's words; this is the one place
/// that turns each into an expression. Keeping the mapping here rather than in
/// `policy.toml` keeps the model readable by whoever is deciding *whether* to
/// alert, and keeps PromQL out of a file that is not about Prometheus.
fn expression(c: &Cluster, id: &str) -> String {
    let nodes = c.cluster.node.len();
    match id {
        "node-down" => "up{job=\"node\"} == 0".to_string(),
        "failed-units" => "node_systemd_unit_state{state=\"failed\"} > 0".to_string(),
        "digest-drift" => "cluster_booted_is_target == 0".to_string(),
        "rollout-stalled" => {
            "cluster_rollout_target_pending == 1 and changes(cluster_rollout_stage[6h]) == 0"
                .to_string()
        }
        "drain-budget-exceeded" => "cluster_drain_budget_exceeded > 0".to_string(),
        "digest-quarantined" => "cluster_quarantined_digests > 0".to_string(),
        // Distinct booted digests across the fleet. One is healthy; more than
        // one for two hours is the split-version state §13.4 refuses to
        // reconcile silently.
        "split-version" => {
            format!("count(count by (booted) (cluster_booted_digest)) > 1 and {nodes} > 1")
        }
        "root-writable" => "cluster_usr_read_only == 0".to_string(),
        "cache-pressure" => "cluster_dmcache_occupancy_ratio > 0.9".to_string(),
        "disk-health" => "node_smart_healthy == 0".to_string(),
        "clock" => format!(
            "node_timex_sync_status == 0 or abs(node_timex_offset_seconds) > {}",
            c.policy.health.chrony_max_offset_ms as f64 / 1000.0
        ),
        "bench-contention" => "cluster_isolated_cpu_foreign_tasks > 0".to_string(),
        "reclaim-volume" => "cluster_reclaim_archived_total > 5".to_string(),
        "dirty-archives-held" => "cluster_dirty_archives_held".to_string(),
        // An alert the model declares and this function does not know is a
        // rendering that would silently emit nothing. Failing loudly here is
        // the only honest option, and CD-13 asserts the set is exhaustive.
        other => panic!(
            "alert `{other}` is declared in model/policy.toml and has no expression. \
             §18's alerts are what make §13 observable; one that renders to nothing \
             is the invisibility the model claims to have removed."
        ),
    }
}

/// Alertmanager: a Tailscale-reachable webhook, and no paging (§18).
fn alertmanager(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# Alertmanager on the storage node. Delivery is a Tailscale-reachable\n\
         # webhook: this is a three-node lab cluster, and an alert that wakes someone\n\
         # is worse than a dashboard they check (§18).\n\n",
    );
    body.push_str("route:\n");
    body.push_str("  receiver: webhook\n");
    body.push_str("  group_by: [alertname, node]\n");
    body.push_str("  group_wait: 30s\n");
    body.push_str("  group_interval: 5m\n");
    body.push_str("  repeat_interval: 12h\n\n");
    body.push_str("receivers:\n");
    body.push_str("  - name: webhook\n");
    body.push_str("    webhook_configs:\n");
    body.push_str(&format!(
        "      - url: \"https://{}.{}.ts.net/alerts\"\n",
        node.name, c.cluster.tailnet
    ));
    body.push_str("        send_resolved: true\n");
    Rendered::new(
        format!("{}/alertmanager/alertmanager.yml", node.name),
        vec!["CD-12"],
        body,
    )
}

/// Garage: the S3-compatible object store backing `sccache` (§5.4).
fn garage(_c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# The object store on {}. It replaces `actions/cache`'s WAN round-trips\n\
         # with a mesh-local S3, which is why it binds the loopback and not the\n\
         # management plane (§5.4).\n\
         #\n\
         # replication_mode = \"none\": there is one node with the disk. §5.6 records\n\
         # that lv_data is a single copy, and a replication factor that pretended\n\
         # otherwise would be a claim this cluster cannot keep.\n\n",
        node.name
    ));
    body.push_str("metadata_dir = \"/var/lib/garage/meta\"\n");
    body.push_str("data_dir = \"/var/lib/garage/data\"\n");
    body.push_str("db_engine = \"sqlite\"\n");
    body.push_str("replication_mode = \"none\"\n\n");
    body.push_str(&format!("rpc_bind_addr = \"{}:3901\"\n", node.loopback));
    body.push_str(&format!("rpc_public_addr = \"{}:3901\"\n\n", node.loopback));
    body.push_str("[s3_api]\n");
    body.push_str(&format!("api_bind_addr = \"{}:3900\"\n", node.loopback));
    body.push_str("s3_region = \"cluster\"\n\n");
    body.push_str("[admin]\n");
    body.push_str(&format!("api_bind_addr = \"{}:3903\"\n", node.loopback));
    Rendered::new(
        format!("{}/garage/garage.toml", node.name),
        vec!["CD-12"],
        body,
    )
}

/// The NFS export table (§5.4).
///
/// Exported to one loopback, with `sec=sys`, which is acceptable *only* because
/// §4.4 makes the mesh a closed segment with exactly two endpoints per link. An
/// export any wider would be relying on trust the topology does not extend, and
/// `CS-02` asserts the narrowness on a booted node.
fn nfs_exports(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# Exported to one address. sec=sys is acceptable only because §4.4 makes the\n\
         # mesh a physically isolated L2 with exactly two endpoints per segment; an\n\
         # export any wider would rely on trust the topology does not extend (§5.4).\n\n",
    );

    // The node that mounts it, derived from the variant that declares the mount
    // rather than named here: two places to write "n2" is one too many.
    for variant in &c.images.variant {
        for mount in variant.mount.iter().filter(|m| m.fstype.starts_with("nfs")) {
            let consumer = c
                .cluster
                .node(&variant.node)
                .expect("the model check requires every variant to name a node");
            let path = c
                .expand(&mount.what)
                .split_once(':')
                .map(|(_, p)| p.to_string())
                .unwrap_or_else(|| mount.what.clone());
            body.push_str(&format!(
                "{path} {}(rw,sync,no_subtree_check,sec=sys,no_root_squash)\n",
                consumer.loopback
            ));
        }
    }

    Rendered::new(
        format!("{}/exports", node.name),
        vec!["CD-12", "CS-02"],
        body,
    )
}
