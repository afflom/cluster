//! `/usr/bin/cluster-health` (`SPEC.md` §10.1).
//!
//! Two modes, one predicate.
//!
//! `cluster-health check` evaluates once, writes JSON on stdout, and exits
//! non-zero on any failure. That is what greenboot's required check runs
//! (§13.3) and what T1, T2 and T3 assert on.
//!
//! `cluster-health serve --bind ADDR` serves the same report over HTTP on the
//! mesh loopback. That is how nodes observe each other without a lock: §13.2's
//! ordering is a pure function of what every peer reports here.
//!
//! Its configuration is read from the environment the image renders (§7.2).
//! Nothing here parses `model/`: a node's configuration is a property of the
//! image it booted.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use cluster_health::probe::{Observations, Probe, SystemProbe};
use cluster_health::{Predicate, ProbeError, Report, State};

/// Exit code when a check does not hold. Distinct from the code used when a
/// probe could not be run, because §13.2 treats the two differently: a failure
/// is an answer and an unknown halts.
const UNHEALTHY: u8 = 1;

/// Exit code when the predicate could not be evaluated at all.
const UNKNOWN: u8 = 2;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("check");

    let predicate = match predicate_from_environment() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cluster-health: {e}");
            return std::process::ExitCode::from(UNKNOWN);
        }
    };
    let probe = SystemProbe {
        state: state_from_environment(),
        target_digest: std::env::var("CLUSTER_TARGET_DIGEST").ok(),
    };

    match mode {
        "serve" => {
            let bind = flag(&args, "--bind").unwrap_or_else(|| "127.0.0.1:9101".to_string());
            match serve(&bind, &predicate, &probe) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cluster-health: {e}");
                    std::process::ExitCode::from(UNKNOWN)
                }
            }
        }
        "check" => match evaluate(&predicate, &probe) {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|_| "{\"healthy\":false}".to_string())
                );
                if report.healthy {
                    std::process::ExitCode::SUCCESS
                } else {
                    for failure in report.failures() {
                        eprintln!(
                            "cluster-health: {} --- {}",
                            failure.id.as_str(),
                            failure.detail
                        );
                    }
                    std::process::ExitCode::from(UNHEALTHY)
                }
            }
            Err(e) => {
                // An unknown is louder than a failure, not quieter: greenboot
                // must not read "we could not tell" as "it is fine" (§13.3).
                eprintln!("cluster-health: {e}");
                std::process::ExitCode::from(UNKNOWN)
            }
        },
        other => {
            eprintln!(
                "cluster-health <check|serve>\n\
                 \n\
                 check                evaluate once, JSON on stdout, non-zero if unhealthy\n\
                 serve --bind ADDR    serve the same report on the mesh loopback\n\
                 \n\
                 unknown mode `{other}`"
            );
            std::process::ExitCode::from(UNKNOWN)
        }
    }
}

/// Evaluate the predicate against the machine.
fn evaluate(predicate: &Predicate, probe: &dyn Probe) -> Result<Report, ProbeError> {
    let observations: Observations = probe.observe(&predicate.peers, predicate.mesh_probe_bytes)?;
    predicate.evaluate(&observations)
}

/// Serve the report on the mesh loopback.
///
/// A hand-written HTTP responder rather than a framework: this binary is in the
/// base image on every node, one endpoint returning one document does not
/// justify an async runtime in the base of the fleet, and every dependency here
/// is one more thing that has to be right before a node can say whether it is
/// healthy.
fn serve(bind: &str, predicate: &Predicate, probe: &dyn Probe) -> Result<(), ProbeError> {
    let listener = TcpListener::bind(bind).map_err(|e| ProbeError {
        check: "serve",
        attempted: format!("bind {bind}"),
        because: e.to_string(),
    })?;

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        // The predicate is evaluated per request rather than cached. A peer
        // reading a cached report would be reading history, and §13.2's
        // ordering is only sound if what it reads is current.
        let body = match evaluate(predicate, probe) {
            Ok(report) => serde_json::to_string(&report).unwrap_or_default(),
            Err(e) => format!(
                "{{\"healthy\":false,\"unknown\":true,\"because\":{}}}",
                serde_json::to_string(&e.to_string()).unwrap_or_default()
            ),
        };
        respond(&mut stream, &body);
    }
    Ok(())
}

/// Write one response. A failure to write to a peer that hung up is not this
/// node's problem and is deliberately not reported: the next poll will ask
/// again, and turning a closed socket into an error would make a peer's network
/// hiccup look like this node's ill health.
fn respond(stream: &mut TcpStream, body: &str) {
    let mut reader = BufReader::new(&*stream);
    let mut request = String::new();
    let _ = reader.read_line(&mut request);

    let status = if request.starts_with("GET /health") {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let payload = if status.starts_with("200") { body } else { "" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{payload}",
        payload.len()
    );
}

/// The predicate's thresholds, from the environment the image rendered.
fn predicate_from_environment() -> Result<Predicate, ProbeError> {
    let required = |key: &'static str| -> Result<String, ProbeError> {
        std::env::var(key).map_err(|_| ProbeError {
            check: "configuration",
            attempted: format!("read ${key}"),
            because: "unset. The image renders it from model/policy.toml (§7.2)".to_string(),
        })
    };

    let list = |key: &str| -> Vec<String> {
        std::env::var(key)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };

    let number = |key: &'static str, fallback: u64| -> Result<u64, ProbeError> {
        match std::env::var(key) {
            Err(_) => Ok(fallback),
            Ok(v) => v.trim().parse().map_err(|_| ProbeError {
                check: "configuration",
                attempted: format!("parse ${key}=`{v}`"),
                because: "not a number".to_string(),
            }),
        }
    };

    Ok(Predicate {
        node: required("CLUSTER_NODE")?,
        expected_digest: required("CLUSTER_EXPECTED_DIGEST")?,
        peers: list("CLUSTER_PEER_LOOPBACKS"),
        quadlets: list("CLUSTER_QUADLETS"),
        mesh_probe_bytes: number("CLUSTER_MESH_PROBE_BYTES", 8972)? as u32,
        max_clock_offset_ms: number("CLUSTER_MAX_CLOCK_OFFSET_MS", 100)?,
    })
}

/// This node's rollout state, which the updater writes.
fn state_from_environment() -> State {
    match std::env::var("CLUSTER_STATE").unwrap_or_default().as_str() {
        "draining" => State::Draining,
        "updating" => State::Updating,
        // Anything else is idle. A node with no state file has not started an
        // update, and treating an unrecognised token as `draining` would stall
        // every peer's predicate on a typo.
        _ => State::Idle,
    }
}

/// The value following a flag.
fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).cloned()
}
