//! `/usr/bin/cluster-updater` (`SPEC.md` §13).
//!
//! One shot of the rollout, run by a timer every ten minutes with up to two
//! minutes of jitter. There is no daemon and no webhook: nodes have no inbound
//! reachability from GitHub, and a timer that wakes, decides, and exits has no
//! state to get wrong between runs.
//!
//! The sequence is exactly §13.3's: observe, decide, publish `draining`, drain,
//! publish `updating`, `bootc upgrade`, reboot. The predicate is re-evaluated
//! before committing, because the first evaluation read peers that may have
//! moved while this node was draining.

use std::process::ExitCode;

use cluster_health::State;
use cluster_updater::rollout::{admits, Decision, Observation, PeerReport};
use cluster_updater::{Applier, RolloutError, Stage, SystemApplier};

/// Exit code when the rollout halted (§13.5). Distinct from a decision to wait,
/// which is the normal middle of a rollout and exits zero.
const HALTED: u8 = 1;

/// Exit code when a stage could not be completed at all.
const FAILED: u8 = 2;

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "run".to_string());
    if mode != "run" {
        eprintln!(
            "cluster-updater run\n\
             \n\
             Evaluates SPEC.md §13.2's ordering predicate and, if this node is admitted,\n\
             drains it and applies the update. Configuration comes from the environment\n\
             the image renders (§7.2)."
        );
        return ExitCode::from(FAILED);
    }

    match run() {
        Ok(decision) => {
            println!("cluster-updater: {decision}");
            if decision.halts() {
                // A halted rollout is a recoverable state; a cluster updated on
                // top of an unnoticed fault is not. Non-zero so the unit records
                // a failure and §18's alert has something to fire on.
                ExitCode::from(HALTED)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("cluster-updater: {e}");
            ExitCode::from(FAILED)
        }
    }
}

fn run() -> Result<Decision, RolloutError> {
    let applier = applier_from_environment()?;
    let target = applier.resolve_target()?;

    let observation = observe(&target, &applier)?;
    let decision = admits(&observation);
    if !decision.applies() {
        return Ok(decision);
    }

    // §12.3: verify before staging. The node applies whatever `:stable` points
    // at, unattended, so this is the check that has to happen before anything
    // is fetched --- not after.
    applier.verify(&target)?;

    applier.publish_state(State::Draining)?;

    // Drain is delegated to the control plane, which is the only thing that
    // knows what sessions exist and where they are (§15.1). The plan it returns
    // is executed by the agent on the node being drained; this binary waits for
    // the outcome and measures it against §14.4's budgets.
    let drained = drain(&applier)?;
    if let Decision::Halt(why) = drained {
        applier.publish_state(State::Idle)?;
        return Ok(Decision::Halt(why));
    }

    // §13.2: re-evaluate within thirty seconds of committing. The first
    // evaluation read peers that may have moved while this node drained, and
    // the whole at-most-one guarantee is over states that were actually
    // observed --- not over states that were observed some minutes ago.
    let recheck = admits(&observe(&target, &applier)?);
    if !recheck.applies() {
        applier.publish_state(State::Idle)?;
        return Ok(recheck);
    }

    applier.publish_state(State::Updating)?;
    applier.upgrade_and_reboot()?;
    Ok(Decision::Apply)
}

/// Read every peer's health, and this node's own state.
fn observe(target: &str, applier: &SystemApplier) -> Result<Observation, RolloutError> {
    let booted = read_command(
        Stage::Observe,
        "bootc",
        &["status", "--json"],
        cluster_health::probe::booted_digest_from,
        "the status document names no booted image digest",
    )?;

    let mut peers = Vec::new();
    for spec in env_list("CLUSTER_PEERS") {
        // `name:position:url`, rendered by the image (§7.2).
        let mut fields = spec.splitn(3, ':');
        let (Some(name), Some(position), Some(url)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(RolloutError {
                stage: Stage::Observe,
                attempted: format!("parse peer `{spec}`"),
                because: "expected name:position:url".to_string(),
            });
        };
        let position: u32 = position.parse().map_err(|_| RolloutError {
            stage: Stage::Observe,
            attempted: format!("parse peer `{spec}`"),
            because: format!("`{position}` is not a rollout position"),
        })?;
        peers.push(read_peer(name, position, url));
    }

    Ok(Observation {
        node: env_or_default("CLUSTER_NODE", "unknown"),
        position: env_or_default("CLUSTER_UPDATE_POSITION", "0")
            .parse()
            .unwrap_or(0),
        booted,
        target: target.to_string(),
        quarantined: quarantined(applier),
        peers,
    })
}

/// Read one peer's `/health`.
///
/// A peer that will not answer becomes [`PeerReport::unknown`] rather than an
/// error: one unreachable peer is a halt (§13.2), not a failure of this
/// binary, and the difference is what lets the halt carry the peer's name.
fn read_peer(name: &str, position: u32, url: &str) -> PeerReport {
    let timeout = env_or_default("CLUSTER_PEER_HEALTH_TIMEOUT_S", "5");
    let output = std::process::Command::new("curl")
        .args(["--silent", "--fail", "--max-time", &timeout, url])
        .output();

    let Ok(output) = output else {
        return PeerReport::unknown(name, position);
    };
    if !output.status.success() {
        return PeerReport::unknown(name, position);
    }
    let Ok(report) = serde_json::from_slice::<cluster_health::Report>(&output.stdout) else {
        return PeerReport::unknown(name, position);
    };
    PeerReport {
        name: name.to_string(),
        position,
        healthy: Some(report.healthy),
        booted: Some(report.booted),
        state: Some(report.state),
    }
}

/// Digests the control plane reports as quarantined (§13.4).
///
/// An unreachable control plane yields an empty list, and that is deliberate:
/// the control plane lives on the last node to update, so during its own reboot
/// no peer can read it. Halting the whole rollout because `n1` is rebooting
/// would make §14.2's stated window into an outage. The risk this trades away
/// is small --- a quarantine is posted by a node that rolled back, and that node
/// is also unhealthy, which halts the rollout by the peer-health clause anyway.
fn quarantined(applier: &SystemApplier) -> Vec<String> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--max-time",
            "5",
            &format!("{}/api/rollout", applier.control_plane),
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    value
        .get("quarantined")
        .and_then(|q| q.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Ask the control plane to drain this node, and measure the result against
/// §14.4's budgets.
fn drain(applier: &SystemApplier) -> Result<Decision, RolloutError> {
    let total_budget = env_or_default("CLUSTER_BUDGET_TOTAL_S", "21600")
        .parse::<u64>()
        .unwrap_or(21_600);

    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            &total_budget.to_string(),
            "--request",
            "POST",
            &format!("{}/api/nodes/{}/drain", applier.control_plane, applier.node),
        ])
        .output()
        .map_err(|e| RolloutError {
            stage: Stage::Drain,
            attempted: format!("drain {}", applier.node),
            because: e.to_string(),
        })?;

    if !output.status.success() {
        // A drain that did not complete is a halt and not a failure to report:
        // the work is still running and the node is still serving, which is a
        // recoverable state that asks for a human (§13.5).
        return Ok(Decision::Halt(format!(
            "draining {} did not complete: {}. A budget is never met by force (§14.4)",
            applier.node,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(Decision::Apply)
}

/// Run a command and extract a value, or report why not.
fn read_command(
    stage: Stage,
    program: &str,
    args: &[&str],
    extract: impl Fn(&str) -> Option<String>,
    missing: &str,
) -> Result<String, RolloutError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| RolloutError {
            stage,
            attempted: format!("{program} {}", args.join(" ")),
            because: e.to_string(),
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    extract(&text).ok_or_else(|| RolloutError {
        stage,
        attempted: format!("{program} {}", args.join(" ")),
        because: missing.to_string(),
    })
}

fn applier_from_environment() -> Result<SystemApplier, RolloutError> {
    let node = env_or_default("CLUSTER_NODE", "");
    if node.is_empty() {
        return Err(RolloutError {
            stage: Stage::Observe,
            attempted: "read $CLUSTER_NODE".to_string(),
            because: "unset. The image renders it from model/cluster.toml (§7.2)".to_string(),
        });
    }
    Ok(SystemApplier {
        registries: env_list("CLUSTER_REGISTRIES"),
        image: env_or_default("CLUSTER_IMAGE", ""),
        tag: env_or_default("CLUSTER_STABLE_TAG", "stable"),
        state_path: env_or_default("CLUSTER_STATE_PATH", "/var/lib/cluster/rollout-state"),
        control_plane: env_or_default("CLUSTER_CONTROL_PLANE", ""),
        node,
    })
}

fn env_or_default(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
