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
#[allow(dead_code)]
fn matching(files: &[Rendered], suffix: &str) -> Vec<Rendered> {
    files
        .iter()
        .filter(|f| f.path.ends_with(suffix))
        .cloned()
        .collect()
}

/// `CD-01`: ports are classified by link speed, and no MAC is rendered anywhere.
#[test]
fn ports_are_classified_by_speed_and_no_mac_is_rendered_cd_01() {
    let c = model();
    let files = rendered();

    // Nothing in the tree carries a MAC. The withdrawn §3.1 put four per node
    // into every `.network` file, which made replacing a mainboard an edit to a
    // file in a repository (§21.12).
    for file in &files {
        assert!(
            !file.body.contains("MACAddress="),
            "{}: a MAC address is a fact about a card, not about this cluster (§3.1)",
            file.path
        );
    }

    // What replaces it: the thresholds a machine sorts its own ports with.
    let policy = files
        .iter()
        .find(|f| f.path.ends_with("init.conf"))
        .expect("the network policy is rendered");
    let mesh = c.network.mesh_class().expect("a mesh class is declared");
    let lan = c.network.lan_class().expect("a lan class is declared");
    for expected in [
        format!("mesh_min_speed_mbps={}", mesh.min_speed_mbps),
        format!("mesh_count={}", mesh.count),
        format!("mesh_mtu={}", mesh.mtu),
        format!("lan_min_speed_mbps={}", lan.min_speed_mbps),
        format!("lan_addressing={}", lan.addressing),
    ] {
        assert!(
            policy.body.contains(&expected),
            "the rendered policy must carry `{expected}`, or cluster-init would need its \
             own copy of it (§3.1)"
        );
    }

    // The mesh threshold is above the LAN one, or every port classifies as mesh.
    assert!(mesh.min_speed_mbps > lan.min_speed_mbps);
    // And the LAN plane is DHCP: there is no per-machine address left to make it
    // static from (§3.2).
    assert_eq!(lan.addressing, "dhcp");
}

/// `CD-02`: the route metrics and addressing bases are rendered, not compiled in.
///
/// The `.network` files themselves cannot be rendered --- a mesh unit needs this
/// machine's ordinal and the ordinal of the peer on a particular cable, and one
/// image boots all three (§3.3, §8.4). What R1 still covers is the arithmetic
/// and the metrics both ends compute from, which is what this asserts. That the
/// units built from them carry a direct and a transit route to every peer is
/// `cluster-init`'s own test, where the units are actually produced.
#[test]
fn the_routing_policy_is_rendered_not_compiled_in_cd_02() {
    let c = model();
    let files = rendered();

    let policy = files
        .iter()
        .find(|f| f.path.ends_with("init.conf"))
        .expect("the network policy is rendered");

    for expected in [
        format!("direct_metric={}", c.network.routing.direct_metric),
        format!("transit_metric={}", c.network.routing.transit_metric),
        format!("loopback_base={}", c.network.addressing.loopback_base),
        format!("link_base={}", c.network.addressing.link_base),
        format!("fleet_size={}", c.cluster.fleet.size),
    ] {
        assert!(
            policy.body.contains(&expected),
            "the rendered policy must carry `{expected}`: a metric compiled into a binary \
             and also written in the model is two sources for one number (§4.2)"
        );
    }

    // The transit metric must lose to the direct one, or failover never happens.
    assert!(c.network.routing.direct_metric < c.network.routing.transit_metric);

    // Forwarding, or a node in the middle of a failover path drops the packets
    // its own route table told a peer to send it. Identity-free, so it is
    // rendered rather than written at boot.
    let sysctl = files
        .iter()
        .find(|f| f.path.ends_with("sysctl.d/90-cluster.conf"))
        .expect("the sysctl fragment is rendered");
    assert!(sysctl.body.contains("net.ipv4.ip_forward=1"));
}

/// `CD-03`: default drop, declared accepts, and mesh-only forwarding.
#[test]
fn the_firewall_drops_by_default_cd_03() {
    let c = model();
    let files = rendered();

    for node in &c.nodes() {
        let nft = files
            .iter()
            .find(|f| f.path == "node/nftables.conf")
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
                rule.roles
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

/// `CD-04`: every name resolves from the ordinals, with no resolver.
#[test]
fn names_resolve_from_the_ordinals_cd_04() {
    let c = model();
    let files = rendered();

    let hosts = files
        .iter()
        .find(|f| f.path == "node/hosts")
        .expect("the hosts file is rendered");

    // One file for the whole fleet. Nothing in it depends on which chassis is
    // reading it, which is why it can be rendered when the `.network` files
    // beside it cannot (§4.3).
    for node in &c.nodes() {
        let entry = format!("{}\t{}\t{}", node.loopback, node.fqdn, node.name);
        assert!(
            hosts.body.contains(&entry),
            "missing `{entry}`; the hosts file must carry every ordinal"
        );
    }

    // The bare cluster name is the entry point, and it resolves to the ordinal
    // the storage role pins (§2.3.2, §4.3).
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("one ordinal holds the storage role");
    let entry = format!("{}\t{}", storage.loopback, c.cluster.fleet.entry_name);
    assert!(
        hosts.body.contains(&entry),
        "missing `{entry}`; a client that knows only the cluster name must find the \
         control plane from it (§4.3)"
    );

    // No name the model does not derive. A hosts file that resolved a name
    // nothing assigned would be a resolver by another route, and §4.3 declares
    // there is none.
    //
    // There are deliberately no management names: those addresses come from
    // DHCP, so there is nothing stable to name (§3.2).
    for line in hosts.body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for name in line.split_whitespace().skip(1) {
            let known = name.starts_with("localhost")
                || name == c.cluster.fleet.entry_name
                || c.nodes().iter().any(|n| name == n.name || name == n.fqdn);
            assert!(known, "`{name}` is not a name this fleet derives");
        }
    }
}

/// `CD-05`: every volume mount carries its relabel flag.
#[test]
fn every_volume_mount_carries_its_relabel_cd_05() {
    let c = model();
    let files = rendered();
    let mut checked = 0usize;

    for node in &c.nodes() {
        let variant = c
            .images
            .variant_for(&node.role)
            .expect("the model check requires a variant per role");
        for quadlet in variant.all_quadlets(&c.images.base) {
            let unit = files
                .iter()
                .find(|f| {
                    f.path == format!("node/containers-systemd/{}.{}", quadlet.name, quadlet.kind)
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

/// `CD-06`: the base kernel arguments carry no isolation, and one role's do.
///
/// This used to assert that `isolcpus=` appeared in exactly one node's rendered
/// `kargs.d`. There is one image now (§8.4), so a `kargs.d` entry reaches all
/// three machines --- and isolating two of the storage node's four cores would
/// cost half its CPU to no purpose. The isolation moved to a per-role file that
/// `cluster-init` applies once the role is known (§8.5), and the assertion moved
/// with it.
#[test]
fn isolation_is_a_role_karg_and_never_a_base_one_cd_06() {
    let c = model();
    let files = rendered();
    let base = &c.images.base;

    let kargs = files
        .iter()
        .find(|f| f.path == "node/kargs.d/10-cluster.toml")
        .expect("the base kernel arguments are rendered");

    for karg in &base.content.kargs {
        assert!(
            kargs.body.contains(&format!("\"{karg}\"")),
            "missing base kernel argument `{karg}`"
        );
    }

    // The failure this replaced the old assertion to catch: one image boots all
    // three roles, so an isolation karg here isolates every machine's cores.
    //
    // Read from the argument list and not from the file, because the file's own
    // comment explains why `isolcpus=` is absent --- and a search over the whole
    // body matches that sentence. A gate that cannot be described in the file it
    // inspects is a gate nobody can write around honestly.
    let declared: Vec<&str> = kargs
        .body
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.strip_suffix("\","))
        .collect();
    assert!(!declared.is_empty(), "no kernel arguments were parsed");
    for forbidden in ["isolcpus=", "nohz_full=", "nosmt"] {
        assert!(
            !declared
                .iter()
                .any(|k| k.starts_with(forbidden) || *k == forbidden),
            "the base kernel arguments carry `{forbidden}`, which would isolate the \
             storage node's cores too (§8.4, §8.5)"
        );
    }

    // Exactly one role declares isolation, and its rendered set carries it.
    let mut isolated = Vec::new();
    for role in &c.cluster.role {
        let file = files
            .iter()
            .find(|f| f.path == format!("node/role-kargs-{}.conf", role.id))
            .unwrap_or_else(|| panic!("`{}` renders no kernel-argument set", role.id));
        let variant = c
            .images
            .variant_for(&role.id)
            .expect("the model check requires a variant per role");
        match &variant.isolation {
            Some(isolation) => {
                isolated.push(role.id.clone());
                assert!(
                    file.body
                        .contains(&format!("isolcpus={}", isolation.isolated_cpus)),
                    "`{}`: isolcpus must name the CPUs the variant declares",
                    role.id
                );
                assert!(file.body.contains("nosmt"), "`{}`: nosmt", role.id);
            }
            None => assert!(
                !file.body.contains("isolcpus="),
                "`{}` declares no isolation but renders isolcpus (§2.3)",
                role.id
            ),
        }
    }
    assert_eq!(
        isolated.len(),
        1,
        "measurement is one role's job; {isolated:?} are isolated (§2.3)"
    );
}

/// `CD-07`: the declared layout, and no secret value.
#[test]
fn the_kickstart_carries_no_secret_cd_07() {
    let c = model();
    let files = rendered();

    for node in &c.nodes() {
        let ks = files
            .iter()
            .find(|f| f.path == "bootstrap/node.ks")
            .expect("the kickstart is rendered");

        for partition in &c.cluster.partition {
            assert!(
                ks.body.contains(&format!("part {} ", partition.mount)),
                "{}: partition {} is declared but not rendered",
                node.name,
                partition.mount
            );
        }

        // The kickstart names no secret at all, not even as a placeholder.
        //
        // It carried three, substituted at ISO build time from Actions secrets
        // that did not exist --- so a node would have installed the literal
        // `@@AUTHORIZED_KEY@@` as root's authorized key, on a headless machine,
        // and then died at `tailscale up --erroronfail`. And an ISO is a release
        // artifact: a secret put in one is published to whoever downloads it
        // (§9.1). Credentials reach a node through the browser after it boots
        // (§12.2).
        for placeholder in cluster_model::render::RETIRED_PLACEHOLDERS {
            assert!(
                !ks.body.contains(placeholder),
                "{}: {placeholder} is retired and must not come back (§12.2)",
                node.name
            );
        }
        for forbidden in ["auth.json", "authorized_keys", "--auth-key"] {
            assert!(
                !ks.body.contains(forbidden),
                "{}: the kickstart mentions `{forbidden}`; a node installs unenrolled \
                 (§12.2)",
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

    for node in &c.nodes() {
        let unit = |name: &str| -> String {
            files
                .iter()
                .find(|f| f.path == format!("node/systemd/{name}"))
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

        // Not under `systemd/`: it is rendered policy, and it lands where
        // `init.conf` does. It used to sit in the unit directory, which the
        // build copies wholesale into `/usr/lib/systemd/system/` --- while the
        // unit read `/etc/cluster/`, which nothing wrote.
        let env = files
            .iter()
            .find(|f| f.path == "node/cluster-updater.env")
            .unwrap_or_else(|| panic!("{}: cluster-updater.env was not rendered", node.name))
            .body
            .clone();

        // And the unit reads it where it actually is. `EnvironmentFile=`
        // without a leading `-` makes a missing file a unit *start failure*, so
        // these two disagreeing meant the updater never ran on any node.
        let service = unit("cluster-updater.service");
        let read = service
            .lines()
            .find_map(|l| l.trim().strip_prefix("EnvironmentFile="))
            .expect("the updater unit reads a rendered environment");
        assert!(
            read.ends_with("/cluster-updater.env") && read.starts_with("/usr/lib/cluster/"),
            "{}: the unit reads {read}, which is not where the rendered environment is \
             shipped (§7.2, §13.1)",
            node.name
        );
        // Which node this is, is deliberately absent. One image boots all three
        // ordinals (§8.4), so CLUSTER_UPDATE_POSITION is written at boot by
        // cluster-init into /run/cluster/node.env, and the unit reads both files.
        // Fleet facts are rendered and diff-gated; machine facts are discovered.
        assert!(
            !env.contains("CLUSTER_UPDATE_POSITION="),
            "the rendered environment must not claim a position: it is the one input \
             the image cannot carry (§8.4, §13.2)"
        );
        assert!(
            env.contains("/run/cluster/node.env"),
            "the rendered environment must name where the machine's own facts arrive"
        );

        // Every ordinal's endpoint, because §13.2's ordering is a pure function
        // of what the peers report and a node that cannot read one of them
        // cannot evaluate it. The whole fleet, not "the peers": which entry is
        // this node is not known until boot, and the updater drops its own by
        // matching CLUSTER_NODE.
        for peer in c.nodes() {
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
            .find(|f| f.path.starts_with("node/greenboot/"))
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
        // The port is a model fact and is rendered; the address is this
        // machine's loopback, which follows from an ordinal it does not have
        // until boot (§4.1, §8.4).
        assert!(health.contains(&format!("${{CLUSTER_LOOPBACK}}:{}", p.health.port)));
        assert!(health.contains("/run/cluster/node.env"));
    }

    // Reclamation runs where the session database and the snapshots are, and
    // nowhere else (§15.3).
    let reclaim: Vec<&Rendered> = files
        .iter()
        .filter(|f| f.path.ends_with("cluster-reclaim.timer"))
        .collect();
    // One unit, shipped on every machine and started on one. It is gated by the
    // role marker rather than by which image was installed, because there is one
    // image (§8.4) --- and a unit whose condition is unmet is skipped, not
    // failed, which is what keeps §10.1's "no failed units" check honest.
    assert_eq!(reclaim.len(), 1, "reclamation is one unit");
    assert!(
        reclaim[0].body.contains(&format!(
            "ConditionPathExists=/run/cluster/role.{}",
            p.drain.migration_target
        )),
        "reclamation must be gated on the role that holds the session database (§15.3)"
    );
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
        // Except where the format refuses it. `containers-policy.json` validates
        // strictly and rejects any key it does not define, so those files carry
        // no header --- they are still diff-gated above, and `Rendered::ids`
        // still names the claims, which is what `check-render` reads (CD-16).
        if !file.path.ends_with(".json") {
            assert!(
                committed.contains(ASSERTED_BY),
                "{}: the header must name the claims that assert over it",
                file.path
            );
        }
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
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared role");
    // By the control plane's *tailnet* name, not by an address. A client is not
    // on the mesh, and management addresses come from DHCP (§3.2), so MagicDNS
    // is the only stable way a client reaches a node (§4.5).
    let host = format!(
        "{}.{}.{}",
        control.name, c.cluster.tailnet, c.cluster.magic_dns_suffix
    );
    assert!(
        ssh.body
            .contains(&format!("http://{host}:8080/api/sessions/")),
        "the alias must resolve the session's current host from the control plane (§11.1)"
    );
    assert!(
        !ssh.body.contains(&control.loopback),
        "a mesh loopback is unreachable from a client, and putting one here would make \
         the alias work only from inside the cluster (§3.2, §4.5)"
    );

    // §16.5: the UI is a management surface, not a dependency. `ssh dc-<id>`
    // keeps working against the last known host when the storage node is rebooting, and only
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

    for node in &c.nodes() {
        let policy = files
            .iter()
            .find(|f| f.path == "node/containers/policy.json")
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
            .find(|f| f.path == "node/containers/registries.conf")
            .expect("every node renders a registry configuration");

        let storage = c
            .node_with_role(&c.policy.drain.migration_target)
            .expect("the migration target is a declared role");
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

    // No address survives unsubstituted. A `{node1.loopback}` rendered literally
    // into a unit file is exactly the failure the placeholder exists to prevent,
    // and it would be invisible until a service failed to bind.
    //
    // The names come from the model. This used to name `{n1.loopback}`,
    // `{n2.loopback}` and `{n3.loopback}` --- machine names withdrawn when roles
    // replaced them --- so it searched for three strings the renderer could not
    // emit and passed on every tree, including one carrying a real
    // unsubstituted placeholder.
    for file in &files {
        for node in &c.nodes() {
            for field in ["loopback", "name", "fqdn"] {
                let placeholder = format!("{{{}.{field}}}", node.name);
                assert!(
                    !file.body.contains(&placeholder),
                    "{}: `{placeholder}` reached the rendered tree unsubstituted",
                    file.path
                );
                let short = node
                    .name
                    .split_once('.')
                    .map_or(node.name.as_str(), |(h, _)| h);
                let short_placeholder = format!("{{{short}.{field}}}");
                assert!(
                    !file.body.contains(&short_placeholder),
                    "{}: `{short_placeholder}` reached the rendered tree unsubstituted",
                    file.path
                );
            }
        }
    }

    // And substitution is not a no-op: the addresses really are in there.
    let zot = files
        .iter()
        .find(|f| f.path.ends_with("containers-systemd/zot.container"))
        .expect("the registry Quadlet is rendered");
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");
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

    let rules = files
        .iter()
        .find(|f| f.path == "node/prometheus/alerts.yml")
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
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");

    let config = files
        .iter()
        .find(|f| f.path == "node/zot/config.json")
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
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");

    for node in &c.nodes() {
        let hook = files
            .iter()
            .find(|f| f.path == "node/sshrc")
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

    for node in &c.nodes() {
        let read = |suffix: &str| -> String {
            files
                .iter()
                .find(|f| f.path == format!("node/{suffix}"))
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
    //
    // Read off the directory rather than from a list of names. The list said
    // `base`, `n1`, `n2`, `n3` --- four directories that stopped existing when
    // three images became one (§8.4) --- so every iteration hit the `continue`
    // and this half of `CD-14` checked nothing at all.
    let root = root();
    let images = std::fs::read_dir(root.join("images")).expect("images/ exists");
    let mut checked = 0usize;
    for entry in images.flatten() {
        let path = entry.path().join("Containerfile");
        let variant = entry.file_name().to_string_lossy().to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        checked += 1;
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
    assert!(
        checked > 0,
        "no Containerfile was read, so this half of CD-14 checked nothing"
    );
    {}
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
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");

    let unit = files
        .iter()
        .find(|f| f.path == "node/systemd/tailscale-serve.service")
        .expect("the control plane is published");

    // The same address the control plane binds. A publish unit pointing
    // somewhere else would succeed and serve nothing.
    let control = files
        .iter()
        .find(|f| f.path == "node/systemd/cluster-ctl.service")
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
    for n in &c.nodes() {
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

        if file.path.ends_with(".json") {
            json += 1;
            // Parsed, not merely inspected: a document that starts with `{` and
            // is malformed later would pass a first-byte check and fail at
            // install --- which is exactly how this was found.
            let parsed: serde_json::Value = serde_json::from_str(&contents)
                .unwrap_or_else(|e| panic!("{}: not valid JSON: {e}", file.path));

            // Schema keys only. `containers-policy.json` validates strictly and
            // refuses an unknown key; a `_generated` field added for provenance
            // was rejected by `bootc install` at deployment. A format that will
            // not carry provenance does not get any --- the file is still
            // diff-gated, and the claims are in the register.
            let injected: Vec<&String> = parsed
                .as_object()
                .map(|o| o.keys().filter(|k| k.starts_with('_')).collect())
                .unwrap_or_default();
            assert!(
                injected.is_empty(),
                "{}: {injected:?} are not schema keys, and a strict validator \
                 rejects them only at install",
                file.path
            );
            assert!(
                !contents.lines().next().unwrap_or_default().starts_with('#'),
                "{}: a JSON document cannot open with a comment",
                file.path
            );
        } else {
            // Everything else carries it, because `check-render` reads it and a
            // reader holding only the file should be able to tell where it came
            // from.
            assert!(
                contents.contains(GENERATED_MARKER) && contents.contains(ASSERTED_BY),
                "{}: no provenance",
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

/// `CD-17`: the policy a node configures itself from is rendered, not compiled
/// in.
///
/// The claim is about *absence* as much as presence: `cluster-init` reads every
/// threshold from this file, so a value that lived in both the model and the
/// binary would be two sources for one fact, and the one that drifted would be
/// the one nobody read.
#[test]
fn a_node_configures_itself_from_a_rendered_policy_cd_17() {
    let c = model();
    let files = rendered();

    let policy = files
        .iter()
        .find(|f| f.path == "node/init.conf")
        .expect("the node policy is rendered");

    let mesh = c.network.mesh_class().expect("a mesh class is declared");
    let lan = c.network.lan_class().expect("a lan class is declared");
    let a = &c.network.addressing;
    let d = &c.network.discovery;
    for expected in [
        format!("mesh_min_speed_mbps={}", mesh.min_speed_mbps),
        format!("mesh_count={}", mesh.count),
        format!("mesh_mtu={}", mesh.mtu),
        format!("lan_min_speed_mbps={}", lan.min_speed_mbps),
        format!("lan_mtu={}", lan.mtu),
        format!("fleet_size={}", c.cluster.fleet.size),
        format!("loopback_base={}", a.loopback_base),
        format!("link_base={}", a.link_base),
        format!("direct_metric={}", c.network.routing.direct_metric),
        format!("transit_metric={}", c.network.routing.transit_metric),
        format!("discovery_group={}", d.group),
        format!("discovery_port={}", d.port),
        format!("discovery_timeout_s={}", d.timeout_s),
        format!("machine_id_path={}", c.cluster.identity.source),
        format!("bulk_disk_min_gb={}", c.cluster.detection.bulk_disk_min_gb),
        format!("domain={}", c.cluster.domain),
        format!("entry_name={}", c.cluster.fleet.entry_name),
    ] {
        assert!(
            policy.body.contains(&expected),
            "the rendered policy must carry `{expected}` (§3.1, §4.1)"
        );
    }

    // Every role, with how it is come by and where it sits in the rollout.
    for role in &c.cluster.role {
        let row = format!("role={}:{}:", role.id, role.detect);
        assert!(
            policy.body.contains(&row),
            "the rendered policy must carry a row for `{}` (§2.3)",
            role.id
        );
    }

    // And the binary that reads it declares none of these itself. A search over
    // the crate's source, which is coarse in one direction only: a value that
    // appears in an unrelated context passes when it should not. The expensive
    // failure is the other one --- a threshold hard-coded beside the model's ---
    // and this catches every instance of it.
    let source = read_crate_source("cluster-init");
    for literal in [
        mesh.min_speed_mbps.to_string(),
        c.cluster.detection.bulk_disk_min_gb.to_string(),
        c.network.routing.transit_metric.to_string(),
        a.loopback_base.clone(),
        a.link_base.clone(),
    ] {
        assert!(
            !source.contains(&literal),
            "cluster-init's source carries `{literal}`, which is a model fact. A value in \
             both the binary and the model is two sources for it (§7.2, CD-17)"
        );
    }
}

/// `CD-18`: every role's firewall include is rendered, empty ones included.
#[test]
fn every_role_renders_a_firewall_include_cd_18() {
    let c = model();
    let files = rendered();

    let common = files
        .iter()
        .find(|f| f.path == "node/nftables.conf")
        .expect("the common ruleset is rendered");
    let includes = common.body.matches("include \"").count();
    assert_eq!(
        includes, 1,
        "the common ruleset includes exactly one role file; one image carries one \
         ruleset (§4.4, §8.4)"
    );

    for role in &c.cluster.role {
        let file = files
            .iter()
            .find(|f| f.path == format!("node/nftables-role-{}.conf", role.id))
            .unwrap_or_else(|| {
                panic!(
                    "`{}` renders no firewall include. nft fails to load a ruleset that \
                     includes a file which is not there, so `no rules` has to be a file \
                     that says so",
                    role.id
                )
            });

        // A restricted rule appears in its own role's file and nowhere else.
        for rule in c.network.firewall.rule.iter().filter(|r| !r.is_universal()) {
            let port = format!("dport {}", rule.port);
            let expected = rule.applies_to(&role.id);
            assert_eq!(
                file.body.contains(&port),
                expected,
                "`{}`: `{}` must appear only in the roles it names (§4.4)",
                role.id,
                rule.comment
            );
            assert!(
                !common.body.contains(&port),
                "`{}` is restricted to a role and must not be in the common ruleset: \
                 putting it there opens the port on every machine (§8.4)",
                rule.comment
            );
        }
    }
}

/// `CD-19`: every role's kernel-argument set is rendered, empty ones included.
#[test]
fn every_role_renders_a_kernel_argument_set_cd_19() {
    let c = model();
    let files = rendered();

    for role in &c.cluster.role {
        let file = files
            .iter()
            .find(|f| f.path == format!("node/role-kargs-{}.conf", role.id))
            .unwrap_or_else(|| {
                panic!(
                    "`{}` renders no kernel-argument set. cluster-init reads one per role, \
                     and an absent file is indistinguishable from a role whose arguments \
                     nobody rendered",
                    role.id
                )
            });
        assert!(
            file.body.lines().any(|l| l.starts_with("options=")),
            "`{}`: the set must be an `options=` line even when it is empty",
            role.id
        );

        let variant = c
            .images
            .variant_for(&role.id)
            .expect("the model check requires a variant per role");
        for karg in &variant.kargs {
            assert!(
                file.body.contains(karg),
                "`{}`: the model declares `{karg}` and the rendered set omits it",
                role.id
            );
        }
    }
}

/// A crate's *shipped* source, concatenated. Used by `CD-17` to assert that a
/// model fact is not also a literal in the binary that reads it.
///
/// Two things are dropped, and both were caught firing wrongly before this
/// comment existed.
///
/// **Everything from `#[cfg(test)]` onwards.** A test fixture naming 10000 Mbps
/// is not a second source for the mesh threshold --- it is a fixture, and it has
/// to name a number to be one.
///
/// **Every comment line.** `links.rs` explains that `ethtool` reports
/// `10000baseT/Full`, which is documentation of a format, not a value the binary
/// uses. This is the third time in this repository an extractor has read prose as
/// code, and the tell is always the same: the thing it objected to was a sentence
/// about the thing rather than the thing.
fn read_crate_source(name: &str) -> String {
    let dir = root().join("crates").join(name).join("src");
    let mut out = String::new();
    let mut stack = vec![dir];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).expect("the crate has a source directory") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("a readable source file");
                let shipped = text
                    .split_once("#[cfg(test)]")
                    .map_or(text.as_str(), |(before, _)| before);
                for line in shipped.lines() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// `CD-21`: every rendered row is one the control plane can actually read, and
/// what it writes is the format applied to the value.
///
/// Asserted against `cluster_ctl::enrolment` rather than against this crate's
/// own idea of the format, because the failure this catches is precisely the two
/// sides disagreeing. The renderer emitting a row the parser rejects is a
/// control plane that will not start; the renderer emitting a format the parser
/// accepts and materialises differently is worse, because it starts.
fn the_rendered_policy_is_one_the_control_plane_can_read(c: &cluster_model::Cluster, body: &str) {
    use cluster_ctl::enrolment::{Enrolment, Format};

    let read = Enrolment::parse(body).expect(
        "the control plane parses this file at startup; a row it rejects is a node whose \
         control plane does not come up (§12.2)",
    );
    assert_eq!(
        read.ids(),
        c.policy
            .secret
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
        "every declared secret reaches the control plane, in declaration order"
    );

    for secret in &c.policy.secret {
        let slot = read.slot(&secret.id).expect("it parsed");
        assert_eq!(slot.mode, secret.mode_bits().expect("octal"));
        assert_eq!(slot.path, secret.path);

        match &slot.format {
            Format::Raw => assert!(
                secret.registry.is_empty(),
                "`{}` builds no document, so its registry names nothing",
                secret.id
            ),
            Format::DockerAuth { registry } => {
                assert_eq!(
                    registry, &secret.registry,
                    "`{}`: the document is keyed by the registry the model declares",
                    secret.id
                );
                // And what lands there is a document podman can parse. The bare
                // token this used to write failed every pull --- unattended, at
                // the next update, three layers from its cause.
                let written = slot
                    .format
                    .materialise("a-token", "an-operator")
                    .expect("it materialises");
                let parsed: serde_json::Value =
                    serde_json::from_str(&written).unwrap_or_else(|e| {
                        panic!(
                            "`{}` lands at {}, which podman parses as JSON, and what would be \
                         written there is not JSON: {e}",
                            secret.id, secret.path
                        )
                    });
                assert!(
                    parsed["auths"][registry]["auth"].is_string(),
                    "keyed by {registry}: {written}"
                );
                assert!(
                    !written.contains("a-token"),
                    "the credential appears only inside the encoded pair: {written}"
                );
            }
        }
    }
}

/// `CD-20`: the enrolled secrets are declared by destination, never by value.
///
/// The row says where a value goes. The values arrive once, through the browser,
/// after the cluster boots (§12.2) --- they were `@@PLACEHOLDER@@` names in the
/// kickstart, substituted at ISO build time from Actions secrets that did not
/// exist, and an ISO is a release artifact besides.
#[test]
fn the_enrolled_secrets_are_declared_by_destination_cd_20() {
    let c = model();
    let files = rendered();

    let policy = files
        .iter()
        .find(|f| f.path == "node/enrolment.conf")
        .expect("the enrolment policy is rendered");

    assert!(
        !c.policy.secret.is_empty(),
        "a cluster that enrols nothing pulls from nothing and joins no tailnet"
    );
    for secret in &c.policy.secret {
        let row = format!(
            "secret={}:{}:{}:{}:{}",
            secret.id,
            secret.path,
            secret.mode,
            secret.apply,
            secret.rendered_format()
        );
        assert!(
            policy.body.contains(&row),
            "the rendered policy must carry `{row}`, or cluster-ctl would need its own \
             copy of the destination (§12.2)"
        );
        if secret.is_stored() {
            let mode = secret.mode_bits().expect("the model check requires octal");
            assert_eq!(
                mode & 0o022,
                0,
                "`{}` lands at {} with mode {}: a credential any local user can rewrite \
                 is a credential any local user has",
                secret.id,
                secret.path,
                secret.mode
            );
        }
    }

    // The enrolled SSH key lands where sshd actually looks.
    //
    // sshd's built-in default is `.ssh/authorized_keys` and nothing else, so a
    // key enrolled to `/etc/ssh/authorized_keys.d/root` would go somewhere it
    // never reads --- the operator enters it, the page says "given", and SSH
    // still refuses. A silent, total failure of the way back in that §16.5
    // keeps for when the control plane is the thing that is wrong.
    if let Some(key) = c
        .policy
        .secret
        .iter()
        .find(|s| s.id == "ssh_authorized_key")
        .filter(|s| s.is_stored())
    {
        let sshd = files
            .iter()
            .find(|f| f.path == "node/sshd_config.d/10-cluster.conf")
            .expect("the sshd policy is rendered");
        let directory = key
            .path
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .expect("an absolute destination has a directory");
        let searched: Vec<&str> = sshd
            .body
            .lines()
            .find_map(|l| l.trim().strip_prefix("AuthorizedKeysFile "))
            .expect(
                "the sshd policy must say where to look, or an enrolled key lands \
                 somewhere sshd never reads (§12.2, §16.5)",
            )
            .split_whitespace()
            .collect();
        assert!(
            searched
                .iter()
                .any(|p| p.starts_with(directory) && p.ends_with("%u")),
            "the enrolled key lands in {directory} and sshd searches {searched:?}. A key \
             the operator entered would be ignored, and the page would say it was given \
             (§12.2, §16.5)"
        );
        // The ordinary place still works, so a key placed by hand is not broken
        // by making room for an enrolled one.
        assert!(
            searched.contains(&".ssh/authorized_keys"),
            "sshd's default must be kept alongside: {searched:?}"
        );
    }

    the_rendered_policy_is_one_the_control_plane_can_read(&c, &policy.body);

    // And no value anywhere. The whole point of enrolment is that this
    // repository is public and these do not belong in it (§9.1).
    for file in &files {
        for marker in ["ghp_", "github_pat_", "tskey-", "ssh-ed25519 ", "ssh-rsa "] {
            assert!(
                !file.body.contains(marker),
                "{}: carries `{marker}`, which is the shape of a credential. Values reach \
                 a node through the browser, never through an artifact (§12.2)",
                file.path
            );
        }
    }
}
