//! T2: three nodes, mesh wired, failover, cross-node features, and a full
//! simulated rollout with drain and rollback (`SPEC.md` §10.2).
//!
//! Not run by `cargo test`. `just t2` runs it, on `n1`, where KVM is guaranteed
//! --- which is why this tier *requires* a bootable fixture rather than skipping
//! politely. On the machine T2 is supposed to run on, a missing `/dev/kvm` means
//! the node is broken, and treating that as a skip would let a broken CI host
//! promote images (§9.4).

use std::path::PathBuf;

use cluster_harness::guest::{boot_mesh, require_bootable, Fixture, Guest};
use cluster_model::Cluster;
use cluster_updater::rollout::{admits, Observation, PeerReport};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cluster-harness is two below the root")
        .to_path_buf()
}

fn model() -> Cluster {
    let c = Cluster::load(&root().join("model")).expect("the cluster model loads");
    c.check().expect("the cluster model is consistent");
    c
}

/// Bring the whole mesh up.
fn mesh() -> (Cluster, Vec<Guest>) {
    let fixture = Fixture::from_environment();
    require_bootable("T2", &fixture);
    let c = model();
    let guests = boot_mesh(&c, &fixture).expect("the mesh boots");
    (c, guests)
}

fn guest_for<'a>(guests: &'a [Guest], node: &str) -> &'a Guest {
    guests
        .iter()
        .find(|g| g.node == node)
        .unwrap_or_else(|| panic!("no guest for {node}"))
}

/// `CN-01`: every node reaches every peer loopback at the full mesh MTU.
///
/// Reachability alone is not enough. A path that has silently dropped to 1500
/// still answers a small ping and then blackholes a registry pull, so the probe
/// sets DF and the plane's payload size --- the same probe `cluster-health`
/// runs, so a green tier and a healthy node mean the same thing.
#[test]
fn every_peer_is_reachable_at_the_full_mtu_cn_01() {
    let (c, guests) = mesh();
    let probe = c.policy.health.mesh_mtu_probe_bytes;

    for node in &c.nodes() {
        let guest = guest_for(&guests, &node.name);
        for peer in c.peers_of(&node.name) {
            guest
                .exec(&format!(
                    "ping -c 1 -W 5 -M do -s {probe} {}",
                    peer.loopback
                ))
                .unwrap_or_else(|e| {
                    panic!(
                        "{} cannot reach {} at {probe} bytes: {e}. A path that will not \
                         carry jumbo frames answers a small ping and blackholes a \
                         registry pull (§10.1)",
                        node.name, peer.name
                    )
                });
        }
    }
}

/// `CN-02`: one failed link does not partition two nodes that can reach each
/// other through the third.
///
/// The whole reason §4.2 renders a transit route at all. `systemd-networkd`
/// withdraws the direct route on carrier loss, so failover needs no daemon --- and
/// this is the test that says the rendered metrics actually produce that.
#[test]
fn a_failed_link_fails_over_to_the_transit_route_cn_02() {
    let (c, guests) = mesh();
    let probe = c.policy.health.mesh_mtu_probe_bytes;

    let link = c
        .network
        .addressing
        .links(c.cluster.fleet.size)
        .into_iter()
        .next()
        .expect("the addressing derives links");
    let a = c
        .node_at(link.lower)
        .expect("the ordinal is in the fleet")
        .name;
    let peer = c.node_at(link.higher).expect("the ordinal is in the fleet");
    let b = peer.name.clone();
    let guest_a = guest_for(&guests, &a);

    // The direct route carries it first.
    let before = guest_a
        .exec(&format!("ip route get {}", peer.loopback))
        .expect("the route is resolvable");
    assert!(
        before.contains(
            &link
                .address_of(peer.ordinal)
                .expect("a link has both ends")
                .to_string()
        ),
        "{a} should reach {b} directly before the link is cut: {before}"
    );

    guest_a
        .detach_link(&c, &c.node(&a).expect("an ordinal slot"), &link.id())
        .expect("the link detaches");
    // networkd needs a moment to notice carrier loss and withdraw the route.
    std::thread::sleep(std::time::Duration::from_secs(10));

    // Still reachable, now one hop further round the triangle.
    guest_a
        .exec(&format!(
            "ping -c 1 -W 5 -M do -s {probe} {}",
            peer.loopback
        ))
        .unwrap_or_else(|e| {
            panic!(
                "{a} lost {b} when link {} went down. A triangle with only direct \
                 routes is not resilient, which is why §4.2 renders a transit \
                 route: {e}",
                link.id()
            )
        });

    let after = guest_a
        .exec(&format!("ip route get {}", peer.loopback))
        .expect("the route is resolvable");
    assert_ne!(before, after, "the path must have changed");
}

/// `CN-03`: the firewall accepts only what the model declares.
///
/// Default drop on input is the property every other rule depends on, and a
/// rule set that opened a port nothing declared would be invisible until
/// somebody found it from outside.
#[test]
fn the_firewall_accepts_only_declared_flows_cn_03() {
    let (c, guests) = mesh();

    for node in &c.nodes() {
        let guest = guest_for(&guests, &node.name);
        let ruleset = guest
            .exec("nft list ruleset")
            .expect("the ruleset is readable");
        assert!(
            ruleset.contains("policy drop"),
            "{}: the input chain must default to drop (§4.4)",
            node.name
        );
        for rule in c
            .network
            .firewall
            .rule
            .iter()
            .filter(|r| r.applies_to(&node.role))
        {
            assert!(
                ruleset.contains(&rule.comment),
                "{}: `{}` is declared and not loaded",
                node.name,
                rule.comment
            );
        }
    }
}

/// `CS-01`: a node that is not the registry host pulls across the mesh.
#[test]
fn a_cross_mesh_registry_pull_succeeds_cs_01() {
    let (c, guests) = mesh();
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");

    for node in &c.nodes() {
        if node.name == storage.name {
            continue;
        }
        let guest = guest_for(&guests, &node.name);
        guest
            .exec(&format!(
                "podman pull --tls-verify=false {}:{}/{}/{}:stable",
                storage.loopback,
                c.images.registries.port,
                c.images.signing.repository,
                cluster_model::render::NODE_DIR
            ))
            .unwrap_or_else(|e| panic!("{} cannot pull across the mesh: {e}", node.name));
    }
}

/// `CS-02`: NFS is exported to the compute node's loopback alone.
///
/// `sec=sys` is acceptable only because §4.4 makes the mesh a closed segment, so
/// an export any wider than one address would be relying on trust the topology
/// does not extend.
#[test]
fn nfs_is_exported_to_one_loopback_cs_02() {
    let (c, guests) = mesh();
    // By the ordinal the role holds, not by the role's name: `guest_for` looks
    // guests up by node name, and `migration_target` is a role now (§2.3).
    let storage_slot = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");
    let storage = guest_for(&guests, &storage_slot.name);
    let exports = storage.exec("exportfs -s").expect("exports are readable");

    let consumers: Vec<&str> = c
        .images
        .variant
        .iter()
        .filter(|v| v.mount.iter().any(|m| m.fstype.starts_with("nfs")))
        .map(|v| v.role.as_str())
        .collect();
    assert_eq!(consumers.len(), 1, "one node mounts NFS (§5.4)");

    let consumer = c.node(consumers[0]).expect("a declared node");
    assert!(
        exports.contains(&consumer.loopback),
        "the export must name {}'s loopback: {exports}",
        consumer.name
    );
    assert!(
        !exports.contains('*') && !exports.contains("0.0.0.0"),
        "an export wider than one address relies on trust the topology does not \
         extend (§5.4): {exports}"
    );
    for other in c.peers_of(&consumer.name) {
        assert!(
            !exports.contains(&other.loopback),
            "{} must not be exported to: {exports}",
            other.name
        );
    }
}

/// `CS-03`: the data volume is a writethrough cache over its origin.
///
/// Writeback would be faster and would make a single non-redundant SSD a
/// data-loss mode for the whole 2 TB origin. Writethrough is what makes §2.5's
/// tolerance of hard power loss true --- which unattended reboots depend on, so
/// this is not a performance preference being asserted, it is a precondition.
#[test]
fn the_data_volume_is_writethrough_cs_03() {
    let (c, guests) = mesh();
    let node = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");
    let guest = guest_for(&guests, &node.name);

    // The devices belong to the role, not to a machine: which chassis holds it
    // is discovered, and what a machine holding it carries is declared (§2.3).
    let devices = &c
        .cluster
        .role(&node.role)
        .expect("the migration target is a declared role")
        .devices;
    let vg = devices.volume_group.as_ref().expect("a volume group");
    let lv = devices.origin_lv.as_ref().expect("an origin LV");
    let status = guest
        .exec(&format!("dmsetup status {vg}-{lv}"))
        .expect("the device is readable");
    assert!(
        status.contains("writethrough"),
        "{}: {vg}/{lv} must be writethrough. Writeback makes a single \
         non-redundant SSD a data-loss mode for the whole origin, and it is \
         writethrough that makes §2.5's tolerance of hard power loss true: {status}",
        node.name
    );
}

/// `CW-01`: `devcontainer up` succeeds against the declared runtime, and an
/// exec runs inside the result.
#[test]
fn a_devcontainer_starts_and_execs_cw_01() {
    let (c, guests) = mesh();
    let compute = c
        .images
        .variant
        .iter()
        .find(|v| v.quadlet.iter().any(|q| q.name == "devcontainer-agent"))
        .expect("one variant runs the devcontainer agent");
    let node = c
        .node_with_role(&compute.role)
        .expect("every role holds an ordinal");
    let guest = guest_for(&guests, &node.name);

    guest
        .exec(
            "mkdir -p /var/tmp/probe/.devcontainer && \
             printf '{\"image\":\"quay.io/fedora/fedora:41\"}' \
               > /var/tmp/probe/.devcontainer/devcontainer.json",
        )
        .expect("a workspace can be written");

    guest
        .exec("devcontainer up --workspace-folder /var/tmp/probe")
        .expect("devcontainer up succeeds against the declared runtime (§8.2)");

    // Up is not enough: a container that starts and cannot be entered is not a
    // development environment.
    let echoed = guest
        .exec(
            "devcontainer exec --workspace-folder /var/tmp/probe \
             sh -c 'echo inside'",
        )
        .expect("an exec runs inside it");
    assert!(echoed.contains("inside"), "{echoed}");
}

/// `CW-02`: an ephemeral runner registers, takes one job, and exits.
///
/// `--ephemeral` is what makes drain a matter of not re-registering rather than
/// of killing work (§14.1), so a runner that stayed alive after a job would
/// silently turn every drain into a wait for something that never ends.
#[test]
fn an_ephemeral_runner_exits_after_one_job_cw_02() {
    let (c, guests) = mesh();
    for variant in &c.images.variant {
        for runner in &variant.runner {
            assert!(
                runner.ephemeral,
                "{}: every runner is ephemeral, or drain never terminates (§14.1)",
                runner.name
            );
            let slot = c
                .node_with_role(&variant.role)
                .expect("every role holds an ordinal");
            let guest = guest_for(&guests, &slot.name);
            let unit = format!("cluster-runner-{}.service", runner.name);
            let active = guest
                .exec(&format!("systemctl is-active {unit}"))
                .unwrap_or_else(|e| panic!("{unit}: {e}"));
            assert_eq!(active, "active", "{unit}");

            // The unit re-registers after each job rather than the runner
            // looping internally, which is what lets a drain stop it by not
            // restarting it.
            let restart = guest
                .exec(&format!("systemctl show -p Restart --value {unit}"))
                .expect("the unit is queryable");
            assert_eq!(restart, "always", "{unit}");
        }
    }
}

/// `CU-07`: exactly one guest updates at a time in a full simulated rollout.
///
/// §13.2's guarantee, observed rather than reasoned about. `CU-01` enumerates
/// the predicate over every consistent state; this watches three real nodes move
/// through it and confirms the sequence the model declares.
#[test]
fn exactly_one_guest_updates_at_a_time_cu_07() {
    let (c, guests) = mesh();
    let target = std::env::var("CLUSTER_TARGET_DIGEST")
        .expect("T2 is given the candidate digest by the workflow");

    let mut order = Vec::new();
    for _ in 0..c.cluster.fleet.size as usize {
        // Ask every node what it would do, from what its peers actually report.
        let mut admitted = Vec::new();
        for node in &c.nodes() {
            let guest = guest_for(&guests, &node.name);
            let booted = guest.health().expect("the predicate runs").booted;
            let peers: Vec<PeerReport> = c
                .peers_of(&node.name)
                .into_iter()
                .map(|peer| {
                    let report = guest_for(&guests, &peer.name)
                        .health()
                        .expect("the peer answers");
                    PeerReport {
                        name: peer.name.clone(),
                        position: peer.update_position,
                        healthy: Some(report.healthy),
                        booted: Some(report.booted),
                        state: Some(report.state),
                    }
                })
                .collect();

            let decision = admits(&Observation {
                node: node.name.clone(),
                position: node.update_position,
                booted,
                target: target.clone(),
                quarantined: Vec::new(),
                peers,
            });
            if decision.applies() {
                admitted.push(node.name.clone());
            }
            assert!(!decision.halts(), "{}: {decision}", node.name);
        }

        assert_eq!(
            admitted.len(),
            1,
            "{admitted:?} were admitted at once; two nodes rebooting together is \
             what §13.2 exists to prevent"
        );
        let name = admitted.remove(0);
        let guest = guest_for(&guests, &name);
        guest.upgrade_to(&target).expect("the upgrade applies");
        assert!(
            guest.health().expect("the predicate runs").healthy,
            "{name} came back unhealthy"
        );
        order.push(name);
    }

    let expected: Vec<String> = c
        .in_update_order()
        .into_iter()
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(order, expected, "§2.3's update positions");
}

/// `CU-08`: a boot that fails the predicate rolls back and quarantines.
///
/// The single reason unattended update is acceptable (§13.3). An untested
/// rollback is not a recovery path, and this is the path taken with no operator
/// present.
#[test]
fn a_failed_boot_rolls_back_and_quarantines_cu_08() {
    let (c, guests) = mesh();
    let node = c
        .in_update_order()
        .into_iter()
        .next()
        .expect("the fleet has ordinals");
    let guest = guest_for(&guests, &node.name);

    let before = guest.health().expect("the predicate runs").booted;

    // Break the predicate the way a bad image would: a unit that will not start.
    // greenboot's required check is `cluster-health`, so this is the same signal
    // a genuinely broken image produces, without needing to build one.
    guest
        .exec(
            "systemctl mask --now cluster-health.service && \
             systemd-run --unit=cluster-break --service-type=oneshot /bin/false || true",
        )
        .expect("a failing unit can be created");
    guest.reboot().expect("the guest reboots");

    // greenboot counts the boot as failed and restores the previous deployment.
    let after = guest.health().expect("the predicate runs after rollback");
    assert_eq!(
        after.booted, before,
        "{}: greenboot must restore the previous deployment (§13.3)",
        node.name
    );

    // And the failed digest is recorded, so no other node attempts it (§13.4).
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");
    let rollout = guest_for(&guests, &storage.name)
        .exec(&format!(
            "curl --silent http://{}:8080/api/rollout",
            storage.loopback
        ))
        .expect("the control plane answers");
    assert!(
        rollout.contains("quarantined"),
        "a rollback must post a quarantine, or the next node tries the same \
         digest (§13.4): {rollout}"
    );
}

/// `CU-09`: a drain migrates a devcontainer and preserves its worktree.
///
/// A devcontainer's durable state is the git worktree, its declared volumes, and
/// the `devcontainer.json` that built it --- not its process state (§14.3).
/// Attached editor sessions drop, by design; what must not drop is the work.
#[test]
fn a_drain_migrates_a_container_and_preserves_its_worktree_cu_09() {
    let (c, guests) = mesh();
    let compute = c
        .images
        .variant
        .iter()
        .find(|v| v.quadlet.iter().any(|q| q.name == "devcontainer-agent"))
        .expect("one variant runs the devcontainer agent");
    let compute_slot = c
        .node_with_role(&compute.role)
        .expect("every role holds an ordinal");
    let source = guest_for(&guests, &compute_slot.name);
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");

    let marker = "the-work-that-must-survive";
    source
        .exec(&format!(
            "mkdir -p /var/lib/devcontainers/probe && \
             printf '%s' '{marker}' > /var/lib/devcontainers/probe/witness"
        ))
        .expect("a worktree can be written");

    source
        .exec(&format!(
            "curl --silent --show-error --fail --request POST \
             http://{}:8080/api/nodes/{}/drain",
            storage.loopback, compute_slot.name
        ))
        .expect("the drain completes");

    let target = guest_for(&guests, &storage.name);
    let survived = target
        .exec("cat /export/devcontainers/probe/witness")
        .expect("the worktree reached the target");
    assert_eq!(
        survived, marker,
        "the worktree is the durable state and must survive the move (§14.3)"
    );

    // Nothing migrated to a node that never receives work (§2.3).
    for reserved in &c.policy.drain.never_receives {
        let guest = guest_for(&guests, reserved);
        assert!(
            !guest.succeeds("test -d /export/devcontainers/probe"),
            "{reserved} receives no migrated workload under any circumstance (§2.3)"
        );
    }
}

/// `CU-10`: the rollout works across one version boundary.
///
/// A rollout leaves the cluster on mixed digests for tens of minutes, so
/// everything that crosses the mesh must work across one version boundary. §13.6
/// requires the simulation to run *from the previous `:stable`* rather than from
/// the candidate to itself --- a candidate-to-itself run would pass on precisely
/// the change that partitions the cluster mid-rollout.
#[test]
fn the_rollout_works_across_one_version_boundary_cu_10() {
    let (c, guests) = mesh();
    let previous = std::env::var("CLUSTER_PREVIOUS_STABLE")
        .expect("T2 is given the previously promoted digest (§13.6)");
    let target = std::env::var("CLUSTER_TARGET_DIGEST").expect("and the candidate");
    assert_ne!(
        previous, target,
        "§13.6 requires the simulation to run from the previous :stable, not from \
         the candidate to itself"
    );

    // Start every node on the previous release.
    for node in &c.nodes() {
        guest_for(&guests, &node.name)
            .upgrade_to(&previous)
            .expect("the previous release boots");
    }

    // Move the first node only, then exercise every interface that crosses the
    // mesh while the fleet is split.
    let first = c
        .in_update_order()
        .into_iter()
        .next()
        .expect("the fleet has ordinals");
    guest_for(&guests, &first.name)
        .upgrade_to(&target)
        .expect("the candidate boots");

    for node in &c.nodes() {
        let guest = guest_for(&guests, &node.name);
        // The health schema is what §13.2 reads across the boundary; a change to
        // it in one release partitions the rollout.
        let report = guest.health().expect("the predicate parses on both sides");
        assert!(report.healthy, "{}: {:?}", node.name, report.failures());

        for peer in c.peers_of(&node.name) {
            let peer_report = guest
                .exec(&format!(
                    "curl --silent --fail http://{}:{}/health",
                    peer.loopback, c.policy.health.port
                ))
                .unwrap_or_else(|e| {
                    panic!(
                        "{} cannot read {}'s health across the version boundary: {e}. \
                         A change that breaks interop between adjacent versions must \
                         ship as two releases (§13.6)",
                        node.name, peer.name
                    )
                });
            serde_json::from_str::<cluster_health::Report>(&peer_report).unwrap_or_else(|e| {
                panic!(
                    "{} cannot parse {}'s health report across the boundary: {e}",
                    node.name, peer.name
                )
            });
        }
    }
}

/// `CL-04`: an image signed by another identity does not stage.
///
/// The policy is the only thing standing between a node and an arbitrary image
/// (§12.3), and §13 applies whatever `:stable` points at with no operator
/// present. This is the assertion that the refusal is real rather than
/// configured.
#[test]
fn an_image_signed_by_another_identity_does_not_stage_cl_04() {
    let (c, guests) = mesh();
    let node = c
        .in_update_order()
        .into_iter()
        .next()
        .expect("the fleet has ordinals");
    let guest = guest_for(&guests, &node.name);

    // An unsigned image in this repository's own namespace: the reference looks
    // exactly like one the node would accept, so only the signature distinguishes
    // it. Testing with an obviously foreign reference would prove nothing.
    let unsigned = std::env::var("CLUSTER_UNSIGNED_IMAGE")
        .expect("T2 is given an unsigned image built from this repository");

    let refused = guest.exec(&format!("bootc switch --retain {unsigned}"));
    assert!(
        refused.is_err(),
        "{}: an image the policy does not admit must not stage (§12.3)",
        node.name
    );

    // And the node is still on what it booted: a refusal that left a staged
    // deployment behind would be a refusal in name only.
    assert!(
        guest.health().expect("the predicate runs").healthy,
        "{} must be unchanged after refusing an image",
        node.name
    );
}

/// `CN-04`: both ends of a cable agree without being told.
///
/// The property the whole addressing scheme rests on (§4.1). Each node learned
/// the peer on each of its mesh ports (§3.3), derived the `/31` from the two
/// ordinals, and took the address its own ordinal implies --- with nothing
/// exchanged but the ordinals themselves. If either end guessed, the two would
/// collide or the link would carry no traffic.
#[test]
fn both_ends_of_a_cable_agree_without_being_told_cn_04() {
    let (c, guests) = mesh();

    for link in c.network.addressing.links(c.cluster.fleet.size) {
        for ordinal in [link.lower, link.higher] {
            let node = c.node_at(ordinal).expect("the ordinal is in the fleet");
            let guest = guest_for(&guests, &node.name);
            let expected = link.address_of(ordinal).expect("a link has both its ends");

            let addresses = guest
                .exec("ip -brief address show")
                .expect("the address table is readable");
            assert!(
                addresses.contains(&expected.to_string()),
                "{}: link {} gives ordinal {ordinal} the address {expected} and the node \
                 does not hold it. Both ends compute the same /31 from the same two \
                 ordinals, so a node that holds a different address derived it from a \
                 different peer (§3.3, §4.1): {addresses}",
                node.name,
                link.id()
            );

            // And the peer's half is on the same wire, reachable at one hop.
            let peer = link
                .peer_of(ordinal)
                .and_then(|p| link.address_of(p))
                .expect("a link has both its ends");
            guest
                .exec(&format!("ping -c 1 -W 5 {peer}"))
                .unwrap_or_else(|e| {
                    panic!(
                        "{} cannot reach {peer} across link {}: {e}. The two ends took \
                         addresses on different prefixes (§4.1)",
                        node.name,
                        link.id()
                    )
                });
        }
    }
}

/// `CN-05`: ordinals are handed out in arrival order and reused after release.
///
/// Provisioning order is the only tie-break available between two identical
/// machines (§2.3.2), and it has to be *stable*: a registrar that handed out a
/// fresh ordinal on every boot would exhaust the fleet in three reboots, and a
/// machine that came back would find its own name pointing somewhere else.
#[test]
fn ordinals_are_stable_and_reused_after_release_cn_05() {
    let (c, guests) = mesh();
    let storage = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the migration target is a declared role");
    let registrar = guest_for(&guests, &storage.name);

    // Every ordinal is held exactly once, and the registry says so.
    let registry = registrar
        .exec("cat /var/lib/cluster/registry.json")
        .expect("the registrar persisted what it handed out");
    for node in c.nodes() {
        if node.role == storage.role {
            continue;
        }
        assert!(
            registry.contains(&format!("\"ordinal\": {}", node.ordinal)),
            "the registry must record ordinal {} as assigned: {registry}",
            node.ordinal
        );
        assert!(
            registry.contains(&format!("\"role\": \"{}\"", node.role)),
            "the registry must record the role that goes with it: {registry}"
        );
    }

    // Assignment is keyed on the machine ID, so it survives a reboot. Each node
    // reports the same ordinal it holds now after coming back.
    for node in c.nodes() {
        let guest = guest_for(&guests, &node.name);
        let before = guest
            .exec("cat /run/cluster/node.env")
            .expect("the node environment is readable");
        assert!(
            before.contains(&format!("CLUSTER_ORDINAL={}", node.ordinal)),
            "{}: {before}",
            node.name
        );
        guest.reboot().expect("the node comes back");
        let after = guest
            .exec("cat /run/cluster/node.env")
            .expect("the node environment is readable after a reboot");
        assert_eq!(
            before, after,
            "{}: a reboot must return the same ordinal and role. A registrar that handed \
             out a fresh one would exhaust the fleet in three reboots (§2.3.2)",
            node.name
        );
    }
}

/// `CB-06`: the isolation the model declares is reflected by the kernel.
///
/// A T2 claim, not a T1 one. The testbed holds no bulk disk, so it has no
/// ordinal until the registrar answers --- and with one guest there is nothing on
/// either cable to answer (§2.3.2, §3.3). It cannot boot alone, which is correct
/// and is why this needs the mesh.
///
/// It also moved because the isolation itself did. `isolcpus=` is applied after
/// the role is known, with `bootc loader-entries set-options-for-source`, since
/// one image boots all three roles and isolating the storage node's cores would
/// cost half its CPU to no purpose (§8.5).
///
/// Constructible and testable, unlike the stability it is meant to support ---
/// §21.1 records why that one is not claimed.
#[test]
fn the_isolated_cpu_set_is_reflected_by_the_kernel_cb_06() {
    let (c, guests) = mesh();

    // The one role that declares isolation. The model check already refuses
    // more than one, and this asserts the search found it rather than passing
    // over an empty loop --- which is exactly how this test passed while
    // testing nothing: it looked a variant up by node *name* when variants are
    // keyed by role, matched none, and reported `ok`.
    let mut checked = 0usize;
    for role in &c.cluster.role {
        let Some(isolation) = c
            .images
            .variant_for(&role.id)
            .and_then(|v| v.isolation.as_ref())
        else {
            continue;
        };
        let node = c
            .node_with_role(&role.id)
            .expect("every role holds an ordinal");
        let guest = guest_for(&guests, &node.name);

        let isolated = guest
            .exec("cat /sys/devices/system/cpu/isolated")
            .expect("the isolated set is readable");
        assert_eq!(
            isolated, isolation.isolated_cpus,
            "{}: the kernel's isolated set must be the one the model declares (§8.5)",
            node.name
        );

        // `nosmt` took effect: no sibling threads are online.
        let smt = guest
            .exec("cat /sys/devices/system/cpu/smt/control")
            .expect("SMT control is readable");
        assert!(
            smt == "off" || smt == "forceoff" || smt == "notsupported",
            "{}: SMT is `{smt}`, and §8.5 disables it",
            node.name
        );

        let governor = guest
            .exec("cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
            .expect("the governor is readable");
        assert_eq!(governor, isolation.governor, "{}", node.name);

        // Applied as a tracked source rather than shipped in the image, so an
        // upgrade re-merges it and a node that stops holding this role drops it.
        let options = guest
            .exec("cat /proc/cmdline")
            .expect("the command line is readable");
        assert!(
            options.contains(&format!("isolcpus={}", isolation.isolated_cpus)),
            "{}: the applied command line must carry the role's arguments (§8.5): {options}",
            node.name
        );
        checked += 1;
    }
    assert_eq!(
        checked, 1,
        "exactly one role declares isolation, and this must have checked it \
         rather than passing over an empty loop (§2.3)"
    );
}
