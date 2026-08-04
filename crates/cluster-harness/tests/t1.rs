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
        .cluster
        .in_update_order()
        .first()
        .copied()
        .expect("the model declares nodes")
        .clone();
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
    for node in &c.cluster.node {
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
