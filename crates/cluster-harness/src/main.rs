//! The tier driver (`SPEC.md` §10.2).
//!
//! `cluster-harness <t1|t2|t3>` runs one validation tier. What each tier may
//! discharge comes from the register, not from this file: [`cluster_harness::collect`]
//! filters by the `tier` column, and refuses to hand a hardware claim to a
//! simulated run whatever the register says (§19.2).
//!
//! When the fixture is incomplete --- no `/dev/kvm`, no firmware, no backing
//! image, a missing tool --- the tier reports an **explicit skip** naming the
//! part that was missing, and exits non-zero. It never falls back to TCG. A tier
//! that quietly emulated would be slower, differently timed, and --- worst ---
//! would look green (§9.4).

use std::process::ExitCode;

use cluster_harness::guest::Fixture;
use cluster_harness::{collect, Acceleration};
use cluster_model::Cluster;
use repo_model::{Model, Tier};

/// Exit code when a tier could not run and therefore tested nothing.
const NOT_RUN: u8 = 3;

fn main() -> ExitCode {
    let tier = match std::env::args().nth(1).as_deref() {
        Some("t1") => Tier::T1,
        Some("t2") => Tier::T2,
        Some("t3") => Tier::T3,
        other => {
            eprintln!(
                "cluster-harness <t1|t2|t3>\n\
                 \n\
                 t1  one node boots under OVMF and the health predicate passes\n\
                 t2  three nodes, mesh wired, failover, and a full simulated rollout\n\
                 t3  the same suite against real hardware, plus firmware verification\n\
                 \n\
                 unknown tier `{}`",
                other.unwrap_or("(none)")
            );
            return ExitCode::from(2);
        }
    };

    let root = cluster_model::repo_root();
    let model = match Model::load(&root.join("model")) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cluster-harness: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cluster = match Cluster::load(&root.join("model")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cluster-harness: {e}");
            return ExitCode::FAILURE;
        }
    };

    let claims = collect(&model, tier);
    println!(
        "{}: {} claim(s) registered at this tier",
        tier.as_str(),
        claims.len()
    );
    for row in &claims {
        println!("  {} {}", row.id, row.statement.replace('\n', " ").trim());
    }

    // T3 runs on the machines themselves; the rest need a guest, and a guest
    // needs KVM, a backing image, OVMF and the tools to drive them.
    //
    // This is the *only* place that decides whether a tier runs. The tier's
    // tests never skip themselves: a test that reported `ok` having booted
    // nothing is a gate that cannot fail wearing a different hat, and five of
    // them reporting `ok` is worse than one honest non-zero exit.
    let fixture = Fixture::from_environment();
    if tier != Tier::T3 {
        if let Some(reason) = fixture.missing() {
            eprintln!("{}: {} {reason}", tier.as_str(), Acceleration::SKIP_NOTICE);
            return ExitCode::from(NOT_RUN);
        }
    }

    for node in &cluster.cluster.node {
        let args = cluster_harness::qemu_args(
            &cluster,
            node,
            &format!("{}.qcow2", node.name),
            &fixture.firmware_code.display().to_string(),
            &fixture.firmware_vars.display().to_string(),
        );
        println!("{}: qemu-system-x86_64 {}", node.name, args.join(" "));
        if tier == Tier::T1 {
            // T1 boots one node only: it establishes that an image boots and is
            // healthy, which needs no peers. Everything that needs a mesh is T2.
            break;
        }
    }

    println!(
        "{}: the fixture is present; the tier's assertions follow",
        tier.as_str()
    );
    ExitCode::SUCCESS
}
