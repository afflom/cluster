//! The `definition` suite: the model renders the declared artifacts
//! (`SPEC.md` §7.2, `CD-01` .. `CD-10`).
//!
//! These run against the real `model/` and the real `generated/` tree, not
//! against a fixture. A renderer tested only on a model written for the test is
//! the differential test comparing the reference against itself --- it would
//! pass on a repository whose actual model rendered nothing at all.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cluster_model::render::{render_all, Rendered, ASSERTED_BY, GENERATED_MARKER, TREE_CLAIM};
use cluster_model::Cluster;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cluster-model is two below the root")
        .to_path_buf()
}

fn model() -> Cluster {
    let c = Cluster::load(&root().join("model")).expect("the cluster model loads");
    c.check().expect("the cluster model is consistent");
    c
}

fn rendered() -> Vec<Rendered> {
    let files = render_all(&model());
    assert!(!files.is_empty(), "the model must render something");
    files
}

/// Every rendered file whose path matches a suffix.
fn matching(files: &[Rendered], suffix: &str) -> Vec<Rendered> {
    files
        .iter()
        .filter(|f| f.path.ends_with(suffix))
        .cloned()
        .collect()
}

/// `CD-01`: interfaces are matched by MAC, never by kernel name.
#[test]
fn interfaces_are_matched_by_mac_cd_01() {
    let c = model();
    let files = rendered();
    let units = matching(&files, ".network");
    assert!(!units.is_empty(), "no network units rendered");

    let mut seen: Vec<String> = Vec::new();
    for unit in &units {
        // The loopback has no card, so it is the one unit matched by name. The
        // claim is worded for physical interfaces for exactly this reason, and
        // the exception is visible in the rendered file rather than inferred.
        if unit.path.ends_with("30-loopback.network") {
            assert!(
                unit.body.contains("Name=lo"),
                "{}: the loopback unit must match the loopback",
                unit.path
            );
            continue;
        }
        assert!(
            unit.body.contains("MACAddress="),
            "{}: a physical interface must be matched by MAC (§3.1)",
            unit.path
        );
        assert!(
            !unit.body.contains("Name="),
            "{}: matching on a kernel name binds whatever the kernel enumerated \
             that boot (§3.1)",
            unit.path
        );
        for line in unit.body.lines() {
            if let Some(mac) = line.strip_prefix("MACAddress=") {
                seen.push(mac.to_string());
            }
        }
    }

    // Every MAC that carries a configured interface appears exactly once. The
    // BMC's is declared but never rendered: it is the BMC's own NIC, on an
    // isolated VLAN the host OS does not configure (§3.2).
    for node in &c.cluster.node {
        for (role, mac) in node.mac.roles() {
            let count = seen.iter().filter(|m| m.as_str() == mac).count();
            let expected = usize::from(role != "bmc");
            assert_eq!(
                count, expected,
                "{}.{role} ({mac}) appears in {count} units, expected {expected}",
                node.name
            );
        }
    }
}

/// `CD-02`: a direct route and a transit route to every peer loopback.
#[test]
fn every_peer_has_a_direct_and_a_transit_route_cd_02() {
    let c = model();
    let files = rendered();
    let direct = c.network.routing.direct_metric;
    let transit = c.network.routing.transit_metric;

    for node in &c.cluster.node {
        let units: String = matching(&files, ".network")
            .iter()
            .filter(|f| f.path.starts_with(&format!("{}/", node.name)))
            .map(|f| f.body.clone())
            .collect::<Vec<_>>()
            .join("\n");

        for peer in c.peers_of(&node.name) {
            let destination = format!("Destination={}/32", peer.loopback);
            let blocks: Vec<&str> = units.split("[Route]").skip(1).collect();

            let with_metric = |metric: u32| -> Vec<&&str> {
                blocks
                    .iter()
                    .filter(|b| b.contains(&destination) && b.contains(&format!("Metric={metric}")))
                    .collect()
            };

            assert_eq!(
                with_metric(direct).len(),
                1,
                "{} needs exactly one direct route to {} at metric {direct}",
                node.name,
                peer.name
            );
            let transit_routes = with_metric(transit);
            assert_eq!(
                transit_routes.len(),
                1,
                "{} needs exactly one transit route to {} at metric {transit}. Without it \
                 one failed link partitions two nodes that can reach each other through \
                 the third (§4.2)",
                node.name,
                peer.name
            );

            // The transit gateway is the *remaining* peer's address on the link
            // this node shares with it --- never the destination's own address,
            // which is what a route copied from the direct one would say.
            let other = c
                .peers_of(&node.name)
                .into_iter()
                .find(|n| n.name != peer.name)
                .expect("a triangle leaves exactly one remaining peer");
            let link = c
                .network
                .link_between(&node.name, &other.name)
                .expect("the model check requires every pair to be joined");
            let gateway = link
                .address_of(&other.name)
                .expect("the model check requires a well-formed /31");
            assert!(
                transit_routes[0].contains(&format!("Gateway={gateway}")),
                "{}'s transit route to {} must go via {} at {gateway}",
                node.name,
                peer.name,
                other.name
            );
        }

        // Forwarding, or a node in the middle of a failover path drops the
        // packets its own route table told a peer to send it.
        let sysctl = files
            .iter()
            .find(|f| f.path == format!("{}/sysctl.d/90-cluster.conf", node.name))
            .expect("every node renders a sysctl fragment");
        assert!(sysctl.body.contains("net.ipv4.ip_forward=1"));
    }
}

/// `CD-03`: default drop, declared accepts, and mesh-only forwarding.
#[test]
fn the_firewall_drops_by_default_cd_03() {
    let c = model();
    let files = rendered();

    for node in &c.cluster.node {
        let nft = files
            .iter()
            .find(|f| f.path == format!("{}/nftables.conf", node.name))
            .expect("every node renders a packet filter");

        assert!(
            nft.body
                .contains("hook input priority filter; policy drop;"),
            "{}: the input chain must default to drop (§4.4)",
            node.name
        );

        // Every declared rule that applies to this node became an accept.
        let accepts = nft.body.matches("accept").count();
        let declared = c
            .network
            .firewall
            .rule
            .iter()
            .filter(|r| r.applies_to(&node.name))
            .count();
        assert!(
            accepts >= declared,
            "{}: {declared} rules declared, {accepts} accepts rendered",
            node.name
        );
        for rule in c
            .network
            .firewall
            .rule
            .iter()
            .filter(|r| r.applies_to(&node.name))
        {
            assert!(
                nft.body.contains(&rule.comment),
                "{}: rule `{}` is declared but not rendered",
                node.name,
                rule.comment
            );
        }
        // And a rule restricted to other nodes did not leak onto this one.
        for rule in c
            .network
            .firewall
            .rule
            .iter()
            .filter(|r| !r.applies_to(&node.name))
        {
            assert!(
                !nft.body.contains(&rule.comment),
                "{}: rule `{}` is restricted to {:?} and must not render here",
                node.name,
                rule.comment,
                rule.nodes
            );
        }

        // Transit forwarding is exactly as wide as §4.2's feature and no wider:
        // both ends must be mesh addresses, so nothing that arrived from the LAN
        // is forwarded.
        assert!(
            nft.body.contains("ip saddr @mesh ip daddr @mesh accept"),
            "{}: forwarding must require both ends to be mesh addresses",
            node.name
        );
    }
}

/// `CD-04`: names resolve from the node table, with no resolver.
#[test]
fn names_resolve_from_the_node_table_cd_04() {
    let c = model();
    let files = rendered();
    let suffixes = &c.network.hosts;

    for node in &c.cluster.node {
        let hosts = files
            .iter()
            .find(|f| f.path == format!("{}/hosts", node.name))
            .expect("every node renders a hosts file");

        for other in &c.cluster.node {
            let mesh = format!(
                "{}\t{}.{} {}",
                other.loopback, other.name, suffixes.mesh_suffix, other.name
            );
            assert!(
                hosts.body.contains(&mesh),
                "{}: missing `{mesh}`",
                node.name
            );
            let mgmt_addr = other
                .mgmt_address
                .split_once('/')
                .map_or(other.mgmt_address.as_str(), |(a, _)| a);
            let mgmt = format!("{mgmt_addr}\t{}.{}", other.name, suffixes.mgmt_suffix);
            assert!(
                hosts.body.contains(&mgmt),
                "{}: missing `{mgmt}`",
                node.name
            );
        }

        // No name the model does not declare. A hosts file that resolved a name
        // nothing assigned would be a resolver by another route, and §4.3
        // declares there is none.
        for line in hosts.body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            for name in line.split_whitespace().skip(1) {
                let known = name.starts_with("localhost")
                    || c.cluster.node.iter().any(|n| {
                        name == n.name
                            || name == format!("{}.{}", n.name, suffixes.mesh_suffix)
                            || name == format!("{}.{}", n.name, suffixes.mgmt_suffix)
                    });
                assert!(known, "{}: `{name}` is not a declared name", node.name);
            }
        }
    }
}

/// `CD-05`: every volume mount carries its relabel flag.
#[test]
fn every_volume_mount_carries_its_relabel_cd_05() {
    let c = model();
    let files = rendered();
    let mut checked = 0usize;

    for node in &c.cluster.node {
        let variant = c
            .images
            .variant_for(&node.name)
            .expect("the model check requires a variant per node");
        for quadlet in variant.all_quadlets(&c.images.base) {
            let unit = files
                .iter()
                .find(|f| {
                    f.path
                        == format!(
                            "{}/containers-systemd/{}.{}",
                            node.name, quadlet.name, quadlet.kind
                        )
                })
                .unwrap_or_else(|| panic!("{}: {} was not rendered", node.name, quadlet.name));
            for mount in &quadlet.mount {
                let expected = format!("Volume={}", mount.volume_line());
                assert!(
                    unit.body.contains(&expected),
                    "{}/{}: expected `{expected}`",
                    node.name,
                    quadlet.name
                );
                // The relabel is the last thing on the line: a missing one is an
                // AVC denial at boot, and §8.3 makes a denial a build failure.
                assert!(
                    expected.ends_with(",Z") || expected.ends_with(",z"),
                    "{}/{}: `{expected}` carries no SELinux relabel (§8.3)",
                    node.name,
                    quadlet.name
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no volume mounts were checked");
}

/// `CD-06`: the isolation set renders on the measurement node alone.
#[test]
fn isolation_renders_on_one_node_alone_cd_06() {
    let c = model();
    let files = rendered();
    let base = &c.images.base;
    let mut isolated_nodes = Vec::new();

    for node in &c.cluster.node {
        let kargs = files
            .iter()
            .find(|f| f.path == format!("{}/kargs.d/10-cluster.toml", node.name))
            .expect("every node renders kernel arguments");

        for karg in &base.content.kargs {
            assert!(
                kargs.body.contains(&format!("\"{karg}\"")),
                "{}: missing base kernel argument `{karg}`",
                node.name
            );
        }

        let variant = c
            .images
            .variant_for(&node.name)
            .expect("the model check requires a variant per node");
        if let Some(isolation) = &variant.isolation {
            isolated_nodes.push(node.name.clone());
            assert!(
                kargs
                    .body
                    .contains(&format!("\"isolcpus={}\"", isolation.isolated_cpus)),
                "{}: isolcpus must name the CPUs the variant declares",
                node.name
            );
            assert!(kargs.body.contains("\"nosmt\""), "{}: nosmt", node.name);
        } else {
            assert!(
                !kargs.body.contains("isolcpus="),
                "{}: declares no isolation but renders isolcpus (§2.3)",
                node.name
            );
        }
    }

    assert_eq!(
        isolated_nodes.len(),
        1,
        "measurement is one node's job; {isolated_nodes:?} are isolated (§2.3)"
    );
}

/// `CD-07`: the declared layout, and no secret value.
#[test]
fn the_kickstart_carries_no_secret_cd_07() {
    let c = model();
    let files = rendered();

    for node in &c.cluster.node {
        let ks = files
            .iter()
            .find(|f| f.path == format!("bootstrap/{}.ks", node.name))
            .expect("every node renders a kickstart");

        for partition in &c.cluster.partition {
            assert!(
                ks.body.contains(&format!("part {} ", partition.mount)),
                "{}: partition {} is declared but not rendered",
                node.name,
                partition.mount
            );
        }

        for placeholder in cluster_model::render::SECRET_PLACEHOLDERS {
            assert!(
                ks.body.contains(placeholder),
                "{}: {placeholder} must appear as a placeholder (§12.2)",
                node.name
            );
        }

        // A kickstart is a plain-text file in a repository, and a secret in one
        // is a secret published. Anything that looks like a real credential is a
        // failure whether or not anyone meant it.
        for (n, line) in ks.body.lines().enumerate() {
            for marker in [
                "-----BEGIN",
                "ssh-rsa ",
                "ssh-ed25519 ",
                "ghp_",
                "github_pat_",
                "tskey-",
            ] {
                assert!(
                    !line.contains(marker),
                    "{}:{}: `{marker}` --- a secret value, not a placeholder (§12.2)",
                    node.name,
                    n + 1
                );
            }
        }
    }
}

/// `CD-08`: unattended behaviour is carried by rendered units.
#[test]
fn unattended_behaviour_is_rendered_from_policy_cd_08() {
    let c = model();
    let files = rendered();
    let p = &c.policy;

    for node in &c.cluster.node {
        let unit = |name: &str| -> String {
            files
                .iter()
                .find(|f| f.path == format!("{}/systemd/{name}", node.name))
                .unwrap_or_else(|| panic!("{}: {name} was not rendered", node.name))
                .body
                .clone()
        };

        let timer = unit("cluster-updater.timer");
        assert!(timer.contains(&format!("OnUnitActiveSec={}", p.rollout.poll_interval_s)));
        assert!(timer.contains(&format!(
            "RandomizedDelaySec={}",
            p.rollout.poll_jitter_max_s
        )));

        let env = unit("cluster-updater.env");
        assert!(env.contains(&format!("CLUSTER_UPDATE_POSITION={}", node.update_position)));
        // Every peer's endpoint, because §13.2's ordering is a pure function of
        // what the peers report and a node that cannot read one of them cannot
        // evaluate it.
        for peer in c.peers_of(&node.name) {
            assert!(
                env.contains(&format!(
                    "{}:{}:http://{}:{}/health",
                    peer.name, peer.update_position, peer.loopback, p.rollout.peer_health_port
                )),
                "{}: no health endpoint for {}",
                node.name,
                peer.name
            );
        }
        for budget in &p.drain_budget {
            let key = budget.class.to_uppercase().replace('-', "_");
            assert!(
                env.contains(&format!("CLUSTER_BUDGET_{key}_S={}", budget.budget_s)),
                "{}: budget {} not rendered",
                node.name,
                budget.class
            );
            assert!(env.contains(&format!(
                "CLUSTER_BUDGET_{key}_ON_EXCEED={}",
                budget.on_exceed
            )));
        }

        let check = files
            .iter()
            .find(|f| f.path.starts_with(&format!("{}/greenboot/", node.name)))
            .expect("every node renders a greenboot check");
        assert!(
            check
                .body
                .contains(&format!("timeout {}s", p.greenboot.deadline_s)),
            "{}: the greenboot check must carry the declared deadline (§13.3)",
            node.name
        );
        // A rollback that does not quarantine leaves the next node free to try
        // the same bad digest (§13.4).
        assert!(check.body.contains("/api/rollout/quarantine"));

        let health = unit("cluster-health.service");
        assert!(health.contains(&format!("{}:{}", node.loopback, p.health.port)));
    }

    // Reclamation runs where the session database and the snapshots are, and
    // nowhere else (§15.3).
    let reclaim: Vec<&Rendered> = files
        .iter()
        .filter(|f| f.path.ends_with("cluster-reclaim.timer"))
        .collect();
    assert_eq!(reclaim.len(), 1, "reclamation runs on one node");
    assert!(reclaim[0]
        .path
        .starts_with(&format!("{}/", p.drain.migration_target)));
}

/// `CD-09`: the committed tree equals the render and is fully asserted about.
#[test]
fn the_committed_tree_equals_the_render_cd_09() {
    let files = rendered();
    let dir = root().join(cluster_model::GENERATED_DIR);

    let mut expected = BTreeSet::new();
    for file in &files {
        let path = dir.join(&file.path);
        expected.insert(path.clone());
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}. Run `just render`.", file.path));
        assert_eq!(
            committed,
            file.contents(),
            "{}: the committed file disagrees with the model (R1)",
            file.path
        );

        assert!(
            file.ids.contains(&TREE_CLAIM),
            "{}: every file is covered by the tree-level claim",
            file.path
        );
        assert!(
            committed.contains(ASSERTED_BY),
            "{}: the header must name the claims that assert over it",
            file.path
        );
        assert!(
            !file.ids.is_empty(),
            "{}: rendering an artifact nothing asserts about is a gap (§7.2)",
            file.path
        );
    }

    // A file under the tree that the model does not render is a stale artifact:
    // it would keep shipping inside an image with nothing regenerating it and
    // nothing asserting over it (§17.2).
    let mut stack = vec![dir.clone()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                assert!(
                    expected.contains(&path),
                    "{}: present but rendered by nothing (§17.2)",
                    path.strip_prefix(&dir).unwrap_or(&path).display()
                );
            }
        }
    }
}

/// `CD-10`: a devcontainer alias survives a migration.
#[test]
fn a_devcontainer_alias_survives_migration_cd_10() {
    let c = model();
    let files = rendered();
    let ssh = files
        .iter()
        .find(|f| f.path == "ssh_config")
        .expect("the client SSH configuration is rendered");

    assert!(ssh.body.contains("Host dc-*"), "no devcontainer alias");

    let control = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");
    assert!(
        ssh.body
            .contains(&format!("http://{}:8080/api/sessions/", control.loopback)),
        "the alias must resolve the session's current host from the control plane (§11.1)"
    );

    // §16.5: the UI is a management surface, not a dependency. `ssh dc-<id>`
    // keeps working against the last known host when n1 is rebooting, and only
    // migration-aware resolution degrades.
    assert!(
        ssh.body.contains("cluster-sessions"),
        "the alias must fall back to the last known host (§16.5)"
    );

    // Key-only, matching what the image's sshd accepts (§8.1).
    assert!(ssh.body.contains("PasswordAuthentication no"));
}

/// `CD-11`: trust and pull order render from the model, and no address in the
/// rendered tree was written down twice.
#[test]
fn trust_and_pull_order_render_from_the_model_cd_11() {
    let c = model();
    let files = rendered();
    let signing = &c.images.signing;

    for node in &c.cluster.node {
        let policy = files
            .iter()
            .find(|f| f.path == format!("{}/containers/policy.json", node.name))
            .expect("every node renders a signature policy");

        // Default reject. A default of `insecureAcceptAnything` would make every
        // rule below it decoration --- the failure §4.4's default-drop avoids.
        assert!(
            policy
                .body
                .contains("\"default\": [{ \"type\": \"reject\" }]"),
            "{}: the policy must reject by default (§12.3)",
            node.name
        );
        assert!(policy.body.contains(&signing.issuer));
        // A *workflow* identity, not merely a repository: §12.3 is explicit that
        // an image signed by another workflow in the same repository must not
        // stage either.
        let identity = signing.certificate_identity();
        assert!(
            policy.body.contains(&identity),
            "{}: the policy must bind the promote workflow, not just the repository",
            node.name
        );
        assert!(identity.contains(&signing.workflow));
        assert!(policy.body.contains("sigstoreSigned"));

        let registries = files
            .iter()
            .find(|f| f.path == format!("{}/containers/registries.conf", node.name))
            .expect("every node renders a registry configuration");

        let storage = c
            .cluster
            .node(&c.policy.drain.migration_target)
            .expect("the migration target is a declared node");
        let local = format!("{}:{}", storage.loopback, c.images.registries.port);
        assert!(registries.body.contains(&local));

        // Every prefix carries the local mirror. `containers-registries` tries
        // mirrors before the primary location, so this is what puts the mesh
        // copy first and leaves the WAN as the fallback --- which is what keeps
        // §14.2 a window rather than an outage.
        for block in registries.body.split("[[registry]]").skip(1) {
            assert!(
                block.contains(&local),
                "{}: a prefix with no local mirror pulls over WAN even when the \
                 mesh has a copy",
                node.name
            );
        }
        for fallback in &c.images.registries.fallbacks {
            assert!(
                registries
                    .body
                    .contains(&format!("prefix = \"{fallback}\"")),
                "{}: {fallback} is declared in the model and not rendered",
                node.name
            );
        }
    }

    // No address survives unsubstituted. A `{n1.loopback}` rendered literally
    // into a unit file is exactly the failure the placeholder exists to prevent,
    // and it would be invisible until a service failed to bind.
    for file in &files {
        assert!(
            !file.body.contains("{n1.loopback}")
                && !file.body.contains("{n2.loopback}")
                && !file.body.contains("{n3.loopback}"),
            "{}: an unsubstituted placeholder reached the rendered tree",
            file.path
        );
    }

    // And substitution is not a no-op: the addresses really are in there.
    let zot = files
        .iter()
        .find(|f| f.path.ends_with("containers-systemd/zot.container"))
        .expect("the registry Quadlet is rendered");
    let storage = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");
    assert!(zot
        .body
        .contains(&format!("PublishPort={}:5000", storage.loopback)));
}

/// `CD-13`: every declared alert renders as a rule.
///
/// §18's digest-drift, rollout-stalled and split-version alerts are what make
/// §13 observable. An alert declared in the model and rendered nowhere would be
/// exactly the invisibility the model claims to have removed --- and it would
/// look, from the model, like coverage.
#[test]
fn every_declared_alert_renders_as_a_rule_cd_13() {
    let c = model();
    let files = rendered();
    let storage = &c.policy.drain.migration_target;

    let rules = files
        .iter()
        .find(|f| f.path == format!("{storage}/prometheus/alerts.yml"))
        .expect("the alert rules are rendered");

    assert!(
        !c.policy.alert.is_empty(),
        "the model declares alerts, or this test checks nothing"
    );
    for alert in &c.policy.alert {
        // The name is the id in camel case, so a rule is findable from the model
        // row that declares it.
        let name: String = alert
            .id
            .split('-')
            .map(|p| {
                let mut chars = p.chars();
                chars
                    .next()
                    .map(|f| f.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect();
        assert!(
            rules.body.contains(&format!("- alert: {name}")),
            "{} is declared and renders no rule (§18)",
            alert.id
        );
        assert!(
            rules
                .body
                .contains(&format!("severity: {}", alert.severity)),
            "{}: severity",
            alert.id
        );
        if alert.for_s > 0 {
            assert!(
                rules.body.contains(&format!("for: {}s", alert.for_s)),
                "{}: duration",
                alert.id
            );
        }
        // And the condition the register states, so a reader of the rule sees
        // the same sentence the model does.
        assert!(
            rules.body.contains(&alert.condition.replace('"', "'")),
            "{}: condition",
            alert.id
        );
    }

    // Every rule traces back to a declared alert: a rule with no row would be a
    // page nobody agreed to.
    let rendered_rules = rules.body.matches("- alert:").count();
    assert_eq!(rendered_rules, c.policy.alert.len());
}

/// `CS-04`: the registry mirrors and caches what the model declares.
#[test]
fn the_registry_mirrors_and_caches_as_declared_cs_04() {
    let c = model();
    let files = rendered();
    let storage = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");

    let config = files
        .iter()
        .find(|f| f.path == format!("{}/zot/config.json", storage.name))
        .expect("the registry configuration is rendered");

    // Bound to the mesh loopback: the registry is a mesh service and §4.4 opens
    // no port for it on the management plane.
    assert!(config.body.contains(&format!(
        "\"address\": \"{}\", \"port\": \"{}\"",
        storage.loopback, c.images.registries.port
    )));

    // This repository's namespace, mirrored on the declared interval (§5.4).
    assert!(config.body.contains("\"pollInterval\": \"5m\""));
    assert!(config
        .body
        .contains(&format!("/{}/**", c.images.signing.repository)));

    // Each fallback, cached on demand rather than mirrored wholesale --- which
    // is the difference between a cache and a second copy of Docker Hub.
    for fallback in &c.images.registries.fallbacks {
        assert!(
            config.body.contains(&format!("https://{fallback}")),
            "{fallback} is declared and not cached"
        );
    }
    assert_eq!(
        config.body.matches("\"onDemand\": true").count(),
        c.images.registries.fallbacks.len()
    );

    // Collection at the age the model declares, in the units Zot takes.
    assert!(config.body.contains(&format!(
        "\"gcDelay\": \"{}h\"",
        c.policy.gc.registry_untagged_max_age_days * 24
    )));
}

/// `CG-07`: an SSH session records an attachment.
#[test]
fn an_ssh_session_records_an_attachment_cg_07() {
    let c = model();
    let files = rendered();
    let control = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");

    for node in &c.cluster.node {
        let hook = files
            .iter()
            .find(|f| f.path == format!("{}/sshrc", node.name))
            .expect("every node renders the attachment hook");

        // `sshrc`, not a profile script: scp, rsync and a VS Code server
        // starting are all attachments and none of them sources a profile.
        assert!(hook.body.contains("/attached"));
        assert!(hook
            .body
            .contains(&format!("http://{}:8080", control.loopback)));

        // Failure is swallowed on purpose. A control plane that is rebooting
        // must not stop somebody logging in, and the cost is one attachment not
        // recorded against thresholds measured in days (§16.5).
        assert!(
            hook.body.contains("|| true"),
            "{}: a rebooting control plane must not block a login (§16.5)",
            node.name
        );
    }
}

/// `CD-14`: host policy is rendered, not declared twice.
///
/// These were hard-coded in the base Containerfile, which gave one decision two
/// sources --- and quietly: an `sshd` that accepted passwords would have
/// satisfied a model saying it did not.
#[test]
fn host_policy_is_rendered_not_declared_twice_cd_14() {
    let c = model();
    let files = rendered();
    let sshd = &c.images.base.sshd;
    let selinux = &c.images.base.selinux;
    let greenboot = &c.policy.greenboot;

    for node in &c.cluster.node {
        let read = |suffix: &str| -> String {
            files
                .iter()
                .find(|f| f.path == format!("{}/{suffix}", node.name))
                .unwrap_or_else(|| panic!("{}: {suffix} is not rendered", node.name))
                .body
                .clone()
        };

        let ssh = read("sshd_config.d/10-cluster.conf");
        assert!(ssh.contains(&format!(
            "PasswordAuthentication {}",
            if sshd.password_authentication {
                "yes"
            } else {
                "no"
            }
        )));
        assert!(ssh.contains(&format!(
            "KbdInteractiveAuthentication {}",
            if sshd.kbd_interactive_authentication {
                "yes"
            } else {
                "no"
            }
        )));
        assert!(ssh.contains(&format!("PermitRootLogin {}", sshd.permit_root_login)));

        let se = read("selinux/config");
        assert!(se.contains(&format!("SELINUX={}", selinux.mode)));
        assert!(se.contains(&format!("SELINUXTYPE={}", selinux.policy_type)));

        // Both halves. A deadline without an attempt count is a node that rolls
        // back and, if the previous deployment is also unhealthy, keeps trying.
        let gb = read("greenboot.conf");
        assert!(gb.contains(&format!(
            "GREENBOOT_MAX_BOOT_ATTEMPTS={}",
            greenboot.max_boot_attempts
        )));
        assert!(gb.contains(&format!(
            "GREENBOOT_HEALTHCHECK_TIMEOUT={}",
            greenboot.deadline_s
        )));
    }

    // And no image build declares any of them a second time.
    let root = root();
    for variant in ["base", "n1", "n2", "n3"] {
        let Ok(text) =
            std::fs::read_to_string(root.join("images").join(variant).join("Containerfile"))
        else {
            continue;
        };
        for declared in [
            "PasswordAuthentication",
            "SELINUXTYPE",
            "GREENBOOT_MAX_BOOT_ATTEMPTS",
        ] {
            assert!(
                !text.contains(declared),
                "images/{variant}/Containerfile declares `{declared}`, which the \
                 model already declares. One decision, one source (R1)"
            );
        }
    }
}

/// `CD-15` and `CC-06`: the control plane is published where the operator is,
/// and the tailnet policy is rendered from the model.
///
/// Without the publish unit the API is bound to a mesh loopback, reachable from
/// the other two nodes and from nowhere else --- and the web interface would
/// render its disconnected state forever while every gate stayed green.
#[test]
fn the_control_plane_is_published_on_the_tailnet_cd_15() {
    let c = model();
    let files = rendered();
    let node = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");

    let unit = files
        .iter()
        .find(|f| f.path == format!("{}/systemd/tailscale-serve.service", node.name))
        .expect("the control plane is published");

    // The same address the control plane binds. A publish unit pointing
    // somewhere else would succeed and serve nothing.
    let control = files
        .iter()
        .find(|f| f.path == format!("{}/systemd/cluster-ctl.service", node.name))
        .expect("the control plane unit is rendered");
    assert!(control.body.contains(&format!("{}:8080", node.loopback)));
    assert!(unit
        .body
        .contains(&format!("http://{}:8080", node.loopback)));
    assert!(unit.body.contains("--https=443"));
    assert!(
        unit.body.contains("Requires=cluster-ctl.service"),
        "publishing without the service behind it would serve a closed port"
    );
    assert!(
        unit.body.contains("ExecStop"),
        "stopping the unit must withdraw the publication"
    );

    // Exactly one node publishes it: §16.1 puts the control plane on one node.
    let publishers: Vec<&Rendered> = files
        .iter()
        .filter(|f| f.path.ends_with("tailscale-serve.service"))
        .collect();
    assert_eq!(publishers.len(), 1);

    let acl = files
        .iter()
        .find(|f| f.path == "tailscale/policy.hujson")
        .expect("the tailnet policy is rendered");
    for login in &c.cluster.authorized_logins {
        assert!(
            acl.body.contains(login),
            "{login} is authorized and not admitted"
        );
    }
    // Only the management prefix is advertised. §4.5: the mesh is never
    // advertised, and a policy that auto-approved a mesh prefix would make the
    // closed segment §4.4 relies on reachable from a laptop.
    assert!(acl.body.contains(&c.network.lan_prefix));
    for n in &c.cluster.node {
        assert!(
            !acl.body.contains(&n.loopback),
            "{}: the mesh is never advertised (§4.5)",
            n.name
        );
    }
}

/// `CC-06` names the same rendered unit; the assertion above covers both
/// directions, and this is the test the register binds the control-plane claim
/// to.
#[test]
fn the_publish_unit_serves_the_bound_address_cc_06() {
    the_control_plane_is_published_on_the_tailnet_cd_15();
}

/// `CD-16`: every rendered artifact is valid in its own syntax.
///
/// The header used to be `#` unconditionally. That made every rendered JSON
/// document invalid --- `bootc install` refused the signature policy with
/// `invalid character '#'`, and only at deployment --- and it pushed the shebang
/// off line one of the greenboot check, which is the script deciding whether an
/// unattended reboot stands. Neither failed anything until an image was actually
/// installed.
#[test]
fn every_rendered_artifact_is_valid_in_its_syntax_cd_16() {
    let files = rendered();
    let mut json = 0usize;
    let mut scripts = 0usize;

    for file in &files {
        let contents = file.contents();

        // Provenance is present whatever the syntax, because `check-render`
        // reads it and because a reader holding only the file should be able to
        // tell where it came from.
        assert!(
            contents.contains(GENERATED_MARKER) && contents.contains(ASSERTED_BY),
            "{}: no provenance",
            file.path
        );

        if file.path.ends_with(".json") {
            json += 1;
            // Parsed, not merely inspected: a document that starts with `{` and
            // is malformed later would pass a first-byte check and fail at
            // install.
            let parsed: serde_json::Value = serde_json::from_str(&contents)
                .unwrap_or_else(|e| panic!("{}: not valid JSON: {e}", file.path));
            assert!(
                parsed.get("_generated").is_some() && parsed.get("_assertedBy").is_some(),
                "{}: provenance must be fields, because JSON has no comments",
                file.path
            );
            assert!(
                !contents.lines().next().unwrap_or_default().starts_with('#'),
                "{}: a JSON document cannot open with a comment",
                file.path
            );
        }

        // Anything with an interpreter line must keep it first: the kernel reads
        // the first two bytes, and a displaced shebang means the script runs
        // under whatever the caller happened to use.
        if file.body.trim_start().starts_with("#!") {
            scripts += 1;
            let first = contents.lines().next().unwrap_or_default();
            assert!(
                first.starts_with("#!"),
                "{}: the interpreter line must be first, not `{first}`",
                file.path
            );
            assert!(
                contents.lines().nth(1).unwrap_or_default().starts_with('#'),
                "{}: the provenance follows the shebang",
                file.path
            );
        }
    }

    assert!(json > 0, "no JSON is rendered, so this checks nothing");
    assert!(
        scripts > 0,
        "no scripts are rendered, so this checks nothing"
    );
}
