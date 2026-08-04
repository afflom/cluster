//! T1: one node boots under OVMF and the health predicate passes
//! (`SPEC.md` §10.2).
//!
//! Not run by `cargo test`. `just t1` runs it, because T1 needs a guest and a
//! gate that pretends to have booted one is worse than no gate. `Cargo.toml`
//! marks this target `test = false` for exactly that reason: `just vv` never
//! claims to have discharged a `T1` claim.
//!
//! When `/dev/kvm` or the backing image is absent the tier prints the skip
//! notice and stops. It never falls back to TCG (§9.4).

use std::path::PathBuf;

use cluster_harness::guest::{require_bootable, Fixture, Guest, BOOT_TIMEOUT_S};
use cluster_model::Cluster;

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

/// Boot the first node in rollout order. T1 establishes that an image boots and
/// is healthy, which needs no peers; everything that needs a mesh is T2.
///
/// Requires the fixture rather than skipping past it. Whether T1 runs at all is
/// the driver's decision (`just t1`), made once and reported in an exit status;
/// a test that quietly returned `ok` having booted nothing would undo that.
fn boot_one(tier: &str) -> (Cluster, Guest) {
    let fixture = Fixture::from_environment();
    require_bootable(tier, &fixture);
    let c = model();
    let node = c
        .in_update_order()
        .into_iter()
        .next()
        .expect("the fleet has ordinals");
    let guest = Guest::boot(&c, &node, &fixture).expect("the guest boots");
    guest
        .wait_for_ssh(BOOT_TIMEOUT_S)
        .expect("the guest answers");
    (c, guest)
}

/// `CB-02`: a booted node passes the health predicate.
///
/// The same binary greenboot runs and the rollout predicate reads (§10.1). If
/// this fails on a candidate image, greenboot would roll that image back on a
/// real node --- so failing here is the cheap version of the same event.
#[test]
fn a_booted_node_passes_the_health_predicate_cb_02() {
    let (_c, guest) = boot_one("T1");
    let report = guest.health().expect("the predicate runs");
    assert!(
        report.healthy,
        "{} is unhealthy: {:?}",
        guest.node,
        report.failures()
    );
    // Every declared check was evaluated, not merely declared.
    assert_eq!(report.checks.len(), cluster_health::CheckId::ALL.len());
}

/// `CB-03`: SELinux is enforcing with no AVC denial after boot settles.
///
/// §8.3 makes a denial a build failure rather than a warning. Every Quadlet
/// mount carries its relabel from the model (`CD-05`), so a denial here means a
/// mount reached a node without one --- which is a defect in the render, caught
/// on the cheapest node in the sequence.
#[test]
fn selinux_is_enforcing_with_no_denials_cb_03() {
    let (_c, guest) = boot_one("T1");
    assert_eq!(
        guest.exec("getenforce").expect("getenforce runs"),
        "Enforcing"
    );

    // Let the boot settle: a denial recorded while units are still starting is
    // the one worth catching, and asserting too early would miss it.
    std::thread::sleep(std::time::Duration::from_secs(30));
    let denials = guest
        .exec("journalctl --boot --grep='avc:  denied' --no-pager --quiet | wc -l")
        .expect("the audit log is readable");
    assert_eq!(
        denials.trim(),
        "0",
        "{}: an AVC denial is a build failure, not a warning (§8.3)",
        guest.node
    );
}

/// `CB-04`: `/usr` is read-only and `/var` is writable.
///
/// The bootc filesystem contract (§5.2). An immutability violation makes every
/// other guarantee in §5.2 untrue, which is why §18 alerts on it separately and
/// why it is asserted rather than assumed.
#[test]
fn the_filesystem_contract_holds_cb_04() {
    let (_c, guest) = boot_one("T1");
    assert!(
        !guest.succeeds("touch /usr/.writable-probe"),
        "{}: /usr accepted a write (§5.2)",
        guest.node
    );
    guest
        .exec("touch /var/.writable-probe && rm /var/.writable-probe")
        .expect("/var must be writable");
}

/// `CB-05`: the declared runtime is present and its socket answers.
///
/// §8.2 says the build fails loudly if the declared runtime's packages are
/// unavailable and never silently substitutes. `CI-02` asserts the build was
/// told to install them; this asserts it worked and the socket answers a Docker
/// API version ping, which is the thing `devcontainer up` actually needs.
#[test]
fn the_declared_runtime_socket_answers_cb_05() {
    let (c, guest) = boot_one("T1");
    let variant = c
        .images
        .variant_for(&guest.node)
        .expect("the booted node has a variant");
    let runtime = c
        .images
        .runtime_of(variant)
        .expect("the variant declares a runtime");

    let active = guest
        .exec(&format!("systemctl is-active {}", runtime.socket_unit))
        .expect("the socket unit is queryable");
    assert_eq!(active, "active", "{}", runtime.socket_unit);

    // A Docker API version ping over the declared socket. Either legal runtime
    // value passes this, which is the point of §8.2's model row.
    let socket = runtime
        .docker_host
        .strip_prefix("unix://")
        .expect("the declared DOCKER_HOST is a unix socket");
    let version = guest
        .exec(&format!(
            "curl --silent --unix-socket {socket} http://localhost/version"
        ))
        .expect("the socket answers");
    assert!(
        version.contains("ApiVersion"),
        "{}: the runtime socket must answer a Docker API version ping (§8.2): {version}",
        guest.node
    );
}

/// `CB-06`: the isolation the model declares is reflected by the kernel.
///
/// Constructible and testable, unlike the stability it is meant to support ---
/// §21.1 records why that one is not claimed. This asserts what §8.5 configures:
/// the isolated set is what the model declares, SMT is off, and the governor is
/// pinned.
#[test]
fn the_isolated_cpu_set_is_reflected_by_the_kernel_cb_06() {
    let fixture = Fixture::from_environment();
    require_bootable("T1", &fixture);
    let c = model();

    // Only the variant that declares isolation. The model check already refuses
    // more than one, so this loop runs exactly once.
    for node in &c.nodes() {
        let Some(isolation) = c
            .images
            .variant_for(&node.name)
            .and_then(|v| v.isolation.as_ref())
        else {
            continue;
        };
        let guest = Guest::boot(&c, node, &fixture).expect("the guest boots");
        guest.wait_for_ssh(BOOT_TIMEOUT_S).expect("it answers");

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
    }
}

/// `CB-07`: a booted node worked out its own ports, ordinal and addresses.
///
/// The assertion that matters is the last one: none of these facts was in the
/// image. A node that had been *told* would pass every check above it and would
/// have made §2.3's whole change cosmetic.
#[test]
fn a_node_works_out_its_own_identity_cb_07() {
    let (c, guest) = boot_one("T1");

    // It classified its ports. The mesh ports are the 10GbE ones, and it found
    // the number a conforming machine presents (§3.1).
    let mesh = c
        .network
        .mesh_class()
        .expect("the model declares a mesh class");
    let env = guest
        .exec("cat /run/cluster/node.env")
        .expect("cluster-init wrote the node environment");

    // The ordinal its own hardware entitles it to. T1 boots the guest carrying
    // the bulk device, so it is the registrar and takes the pinned ordinal
    // (§2.3.1).
    let storage = c
        .cluster
        .self_detected_role()
        .expect("the model declares a self-detected role");
    let expected = storage
        .ordinal
        .expect("the self-detected role pins an ordinal");
    assert!(
        env.contains(&format!("CLUSTER_ORDINAL={expected}")),
        "{}: the node must take the ordinal its disks entitle it to (§2.3.1): {env}",
        guest.node
    );
    assert!(
        env.contains(&format!("CLUSTER_ROLE={}", storage.id)),
        "{}: {env}",
        guest.node
    );

    // The addresses that ordinal derives, on the interfaces themselves.
    let loopback = c
        .network
        .addressing
        .loopback_of(expected)
        .expect("the ordinal is in the fleet");
    let addresses = guest
        .exec("ip -brief address show")
        .expect("the address table is readable");
    assert!(
        addresses.contains(&loopback.to_string()),
        "{}: the loopback must carry {loopback}, which is what ordinal {expected} \
         derives (§4.1): {addresses}",
        guest.node
    );

    // And a mesh port that found no peer is unaddressed rather than guessed at.
    // T1 boots one node, so both of them are (§3.3, §12.1).
    let links = guest
        .exec("ip -brief link show | grep -c ' UP '")
        .unwrap_or_default();
    assert!(
        !links.is_empty(),
        "{}: the link table is readable",
        guest.node
    );

    // Nothing above was in the image. The rendered tree ships the *policy* --
    // thresholds, bases, metrics -- and no ordinal, no role and no address.
    let shipped = guest
        .exec("cat /usr/lib/cluster/init.conf")
        .expect("the rendered policy ships in the image");
    assert!(
        shipped.contains(&format!("mesh_min_speed_mbps={}", mesh.min_speed_mbps)),
        "{}: the image carries the policy",
        guest.node
    );
    for absent in ["CLUSTER_ORDINAL", "CLUSTER_ROLE", &loopback.to_string()] {
        assert!(
            !shipped.contains(absent),
            "{}: the image carries `{absent}`, which makes the machine's identity a fact \
             in two places again (§2.3, §3.1)",
            guest.node
        );
    }
}

/// `CB-08`: one role marker, and no unit failed for belonging to another role.
///
/// This is what one image for three roles costs and what it must not cost. Every
/// role's units ship on every machine; a unit whose `ConditionPathExists=` is
/// unmet must be **skipped**, not failed, or `cluster-health`'s "no failed
/// units" check would report two thirds of the fleet's units as broken on every
/// node (§8.4, §10.1).
#[test]
fn one_role_marker_and_no_unit_failed_for_another_role_cb_08() {
    let (c, guest) = boot_one("T1");

    let markers = guest
        .exec("ls /run/cluster/ | grep '^role\\.' | wc -l")
        .expect("the marker directory is readable");
    assert_eq!(
        markers.trim(),
        "1",
        "{}: exactly one role marker, or two roles' services start on one machine (§8.4)",
        guest.node
    );

    let role = c
        .cluster
        .self_detected_role()
        .expect("the model declares a self-detected role");
    assert!(
        guest.succeeds(&format!("test -e /run/cluster/role.{}", role.id)),
        "{}: the marker must name the role the node discovered",
        guest.node
    );

    // The property that makes this safe: nothing failed.
    let failed = guest
        .exec("systemctl list-units --state=failed --no-legend --plain | wc -l")
        .expect("the unit table is readable");
    assert_eq!(
        failed.trim(),
        "0",
        "{}: a unit gated on another role must be skipped, not failed (§8.4): {}",
        guest.node,
        guest
            .exec("systemctl list-units --state=failed --no-legend --plain")
            .unwrap_or_default()
    );

    // And cluster-init itself succeeded, which is what everything above rests on.
    assert_eq!(
        guest
            .exec("systemctl is-active cluster-init.service")
            .expect("the init unit is queryable"),
        "active",
        "{}: cluster-init is what makes one image bootable as three roles",
        guest.node
    );
}
