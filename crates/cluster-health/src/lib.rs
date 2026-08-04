//! The health predicate (`SPEC.md` §10.1).
//!
//! "Assert healthy" appears throughout the specification, in the rollout
//! predicate, in greenboot, and in three test tiers. It is defined **once**,
//! here, and shipped as `/usr/bin/cluster-health` in the base image. Five
//! consumers, one predicate: T1, T2, T3, greenboot's required check (§13.3),
//! and the rollout precondition (§13.2).
//!
//! # Why the predicate is separated from the probing
//!
//! [`Predicate::evaluate`] is a pure function from [`Observations`] to a
//! [`Report`]. It runs no command and touches no filesystem, which is what
//! makes it testable against its oracle in-process rather than only inside a
//! guest. The probing --- `systemctl`, `bootc status`, `ping`, `getenforce`,
//! `chronyc` --- lives behind [`Probe`], and the real implementation is in
//! `probe.rs`.
//!
//! That split is not tidiness. The predicate is the thing greenboot uses to
//! decide whether an unattended reboot stands or rolls back, and a decision
//! procedure that can only be exercised by booting a machine is a decision
//! procedure that gets exercised rarely.
//!
//! # Unknown is not unhealthy
//!
//! A probe that cannot be *run* is different from a probe that *fails*. §13.2's
//! ordering halts on the former and treats the latter as a definite answer, so
//! the two are different variants here and never collapse. Conflating them
//! would make a node with a broken `chronyc` binary look exactly like a node
//! with a broken clock --- and the correct response to those is not the same.

#![deny(missing_docs)]

pub mod probe;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use probe::{Observations, Probe, SystemProbe};

/// The one error a caller of this crate can see (R5).
///
/// Sanctioned by `CB-01` in `model/ids.toml`. It reports that a declared probe
/// could not be **executed** --- a missing binary, a permission denial, a socket
/// that is not there. It never reports that a check *failed*: a failed check is
/// a [`Check`] with `holds == false`, which is an answer, and this is the
/// absence of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    /// Which check could not be evaluated.
    pub check: &'static str,
    /// What was attempted.
    pub attempted: String,
    /// Why it could not be done.
    pub because: String,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: could not run `{}`: {}. This is an unknown, not a failure --- \
             the rollout halts rather than proceeding (§13.2)",
            self.check, self.attempted, self.because
        )
    }
}

impl std::error::Error for ProbeError {}

/// The eight checks §10.1 declares, in the order it declares them.
///
/// Named rather than indexed so that a report is readable by whoever is looking
/// at a node that will not come up, and so that a check added here has to be
/// added to the register too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckId {
    /// `systemctl is-system-running` returns `running`, not `degraded`.
    SystemRunning,
    /// `systemctl --failed` is empty.
    NoFailedUnits,
    /// `bootc status --json` reports the expected image digest.
    BootedDigest,
    /// Every declared mesh peer loopback answers, at the full MTU.
    MeshReachable,
    /// Every Quadlet declared for this node's role is active.
    QuadletsActive,
    /// `/usr` is read-only and `/var` writable.
    FilesystemContract,
    /// `getenforce` is `Enforcing`, with no AVC denials since boot.
    SelinuxEnforcing,
    /// chrony is synchronised and within the declared offset.
    ClockSynchronised,
}

impl CheckId {
    /// Every check, in specification order.
    pub const ALL: [Self; 8] = [
        Self::SystemRunning,
        Self::NoFailedUnits,
        Self::BootedDigest,
        Self::MeshReachable,
        Self::QuadletsActive,
        Self::FilesystemContract,
        Self::SelinuxEnforcing,
        Self::ClockSynchronised,
    ];

    /// The check's name, as it appears on stdout and in an alert.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemRunning => "system-running",
            Self::NoFailedUnits => "no-failed-units",
            Self::BootedDigest => "booted-digest",
            Self::MeshReachable => "mesh-reachable",
            Self::QuadletsActive => "quadlets-active",
            Self::FilesystemContract => "filesystem-contract",
            Self::SelinuxEnforcing => "selinux-enforcing",
            Self::ClockSynchronised => "clock-synchronised",
        }
    }
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Which check.
    pub id: CheckId,
    /// Whether it holds.
    pub holds: bool,
    /// What was observed, in enough detail to act on.
    pub detail: String,
}

/// What the node reports about itself.
///
/// Served over HTTP on the mesh loopback at `:9101/health`, which is how nodes
/// observe each other without a lock (§13.2) and how §18 alerts on drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// This node's name, from the rendered environment.
    pub node: String,
    /// Whether every check holds.
    pub healthy: bool,
    /// The digest this node actually booted.
    pub booted: String,
    /// What `:stable` currently resolves to, when the node has looked.
    pub target: Option<String>,
    /// The node's own rollout state, which §13.2 reads from its peers.
    pub state: State,
    /// Every check, in specification order.
    pub checks: Vec<Check>,
}

impl Report {
    /// The checks that did not hold.
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.holds).collect()
    }
}

/// A node's position in the rollout, as its peers see it (§13.2).
///
/// `Draining` and `Updating` are published deliberately: §13.2's predicate
/// refuses to admit a node while any peer reports either, which is what stops
/// two nodes rebooting at once without a lock between them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Running normally. The default: a node with no state file has not started
    /// an update, and defaulting to `draining` would stall every peer's
    /// predicate on a missing file (§13.2).
    #[default]
    Idle,
    /// Waiting for work to finish before an update (§14).
    Draining,
    /// Applying an update, or rebooting into one.
    Updating,
}

impl State {
    /// The token used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Draining => "draining",
            Self::Updating => "updating",
        }
    }
}

/// The thresholds the predicate is evaluated against, rendered from
/// `model/policy.toml` into the image (§7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    /// This node's name.
    pub node: String,
    /// The digest this node is expected to be running.
    pub expected_digest: String,
    /// Peer loopbacks that must answer, at the full MTU.
    pub peers: Vec<String>,
    /// Quadlet units declared for this node's role.
    pub quadlets: Vec<String>,
    /// The ICMP payload size that fails if the path is not carrying jumbo
    /// frames: the mesh MTU less 20 bytes of IP header and 8 of ICMP.
    pub mesh_probe_bytes: u32,
    /// Above this, the clock check does not hold.
    pub max_clock_offset_ms: u64,
}

impl Predicate {
    /// Evaluate every check against a set of observations.
    ///
    /// Pure: no command runs here. `Err` means a probe could not be executed
    /// and the node's health is *unknown*, which §13.2 halts on rather than
    /// treating as either answer.
    pub fn evaluate(&self, o: &Observations) -> Result<Report, ProbeError> {
        let mut checks = Vec::with_capacity(CheckId::ALL.len());

        // 1. `systemctl is-system-running` returns `running`, not `degraded`.
        checks.push(Check {
            id: CheckId::SystemRunning,
            holds: o.system_state == "running",
            detail: format!("systemctl is-system-running: {}", o.system_state),
        });

        // 2. `systemctl --failed` is empty.
        checks.push(Check {
            id: CheckId::NoFailedUnits,
            holds: o.failed_units.is_empty(),
            detail: if o.failed_units.is_empty() {
                "no failed units".to_string()
            } else {
                format!("failed: {}", o.failed_units.join(", "))
            },
        });

        // 3. The booted digest is the expected one. A node running an image
        //    nobody promoted is the one condition every other check would pass
        //    over silently.
        checks.push(Check {
            id: CheckId::BootedDigest,
            holds: o.booted_digest == self.expected_digest,
            detail: format!(
                "booted {}, expected {}",
                short(&o.booted_digest),
                short(&self.expected_digest)
            ),
        });

        // 4. Every peer loopback answers at the full MTU. Reachability alone is
        //    not enough: a path that has silently dropped to 1500 still answers
        //    a small ping and then fragments or blackholes a registry pull.
        let unreachable: Vec<&String> = self
            .peers
            .iter()
            .filter(|p| !o.mesh_reachable.contains(*p))
            .collect();
        checks.push(Check {
            id: CheckId::MeshReachable,
            holds: unreachable.is_empty(),
            detail: if unreachable.is_empty() {
                format!(
                    "{} peers answer at {} bytes",
                    self.peers.len(),
                    self.mesh_probe_bytes
                )
            } else {
                format!(
                    "no reply at {} bytes from {:?}",
                    self.mesh_probe_bytes, unreachable
                )
            },
        });

        // 5. Every Quadlet declared for this node's role is active.
        let inactive: Vec<&String> = self
            .quadlets
            .iter()
            .filter(|q| !o.active_units.contains(*q))
            .collect();
        checks.push(Check {
            id: CheckId::QuadletsActive,
            holds: inactive.is_empty(),
            detail: if inactive.is_empty() {
                format!("{} declared units active", self.quadlets.len())
            } else {
                format!("inactive: {inactive:?}")
            },
        });

        // 6. `/usr` read-only, `/var` writable. An immutability violation is
        //    the failure that makes every other guarantee in §5.2 untrue, and
        //    §18 alerts on it separately for that reason.
        let contract = o.usr_read_only && o.var_writable;
        checks.push(Check {
            id: CheckId::FilesystemContract,
            holds: contract,
            detail: format!(
                "/usr read-only: {}, /var writable: {}",
                o.usr_read_only, o.var_writable
            ),
        });

        // 7. Enforcing, and no AVC denial since boot. §8.3 makes a denial a
        //    build failure rather than a warning, so a denial observed here is a
        //    node that should never have been promoted.
        let selinux = o.selinux_enforcing && o.avc_denials == 0;
        checks.push(Check {
            id: CheckId::SelinuxEnforcing,
            holds: selinux,
            detail: format!(
                "enforcing: {}, AVC denials since boot: {}",
                o.selinux_enforcing, o.avc_denials
            ),
        });

        // 8. chrony synchronised and inside the declared offset. Not cosmetic:
        //    session idle age (§15.3) and every retention threshold are computed
        //    from a clock, and a node whose clock jumped is a node that archives
        //    a session that was attached an hour ago.
        let clock = o.chrony_synchronised && o.clock_offset_ms <= self.max_clock_offset_ms;
        checks.push(Check {
            id: CheckId::ClockSynchronised,
            holds: clock,
            detail: format!(
                "synchronised: {}, offset {} ms (limit {})",
                o.chrony_synchronised, o.clock_offset_ms, self.max_clock_offset_ms
            ),
        });

        debug_assert_eq!(checks.len(), CheckId::ALL.len());

        Ok(Report {
            node: self.node.clone(),
            healthy: checks.iter().all(|c| c.holds),
            booted: o.booted_digest.clone(),
            target: o.target_digest.clone(),
            state: o.state,
            checks,
        })
    }
}

/// The first twelve characters after `sha256:`, which is what a human reads.
fn short(digest: &str) -> String {
    let bare = digest.strip_prefix("sha256:").unwrap_or(digest);
    bare.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn predicate() -> Predicate {
        Predicate {
            node: "n2".to_string(),
            expected_digest: "sha256:aaaa".to_string(),
            peers: vec!["10.10.255.1".to_string(), "10.10.255.3".to_string()],
            quadlets: vec!["devcontainer-agent.service".to_string()],
            mesh_probe_bytes: 8972,
            max_clock_offset_ms: 100,
        }
    }

    fn healthy_observations() -> Observations {
        Observations {
            system_state: "running".to_string(),
            failed_units: Vec::new(),
            booted_digest: "sha256:aaaa".to_string(),
            target_digest: Some("sha256:aaaa".to_string()),
            state: State::Idle,
            mesh_reachable: vec!["10.10.255.1".to_string(), "10.10.255.3".to_string()],
            active_units: vec!["devcontainer-agent.service".to_string()],
            usr_read_only: true,
            var_writable: true,
            selinux_enforcing: true,
            avc_denials: 0,
            chrony_synchronised: true,
            clock_offset_ms: 4,
        }
    }

    /// `CB-01`: the predicate holds exactly when all eight declared checks
    /// hold.
    ///
    /// Both directions, because only one of them is the interesting one. That a
    /// healthy node passes is the control; that each check can fail *on its own*
    /// is the claim. A predicate whose checks have never been seen to fail
    /// individually is a predicate that might be answering on one of them and
    /// ignoring the other seven --- and greenboot would then stand behind a boot
    /// that satisfied a single condition (§13.3).
    #[test]
    fn the_predicate_holds_exactly_when_every_check_holds_cb_01() {
        let report = predicate().evaluate(&healthy_observations()).expect("runs");
        assert!(report.healthy, "{:?}", report.failures());
        assert_eq!(
            report.checks.len(),
            CheckId::ALL.len(),
            "every declared check must be evaluated, not merely declared"
        );
        for id in CheckId::ALL {
            assert!(
                report.checks.iter().any(|c| c.id == id),
                "{id:?} is declared in §10.1 but the predicate never evaluates it"
            );
        }
    }

    /// Each of the eight checks can fail on its own (`CB-01`).
    #[test]
    fn every_check_can_fail_on_its_own_cb_01() {
        let p = predicate();
        /// One way to break exactly one check.
        type Break = (CheckId, fn(&mut Observations));

        let breaks: Vec<Break> = vec![
            (CheckId::SystemRunning, |o| {
                o.system_state = "degraded".to_string()
            }),
            (CheckId::NoFailedUnits, |o| {
                o.failed_units.push("zot.service".to_string())
            }),
            (CheckId::BootedDigest, |o| {
                o.booted_digest = "sha256:bbbb".to_string()
            }),
            (CheckId::MeshReachable, |o| {
                o.mesh_reachable.retain(|p| p != "10.10.255.3")
            }),
            (CheckId::QuadletsActive, |o| o.active_units.clear()),
            (CheckId::FilesystemContract, |o| o.usr_read_only = false),
            (CheckId::SelinuxEnforcing, |o| o.avc_denials = 1),
            (CheckId::ClockSynchronised, |o| o.clock_offset_ms = 5_000),
        ];

        for (id, break_it) in breaks {
            let mut o = healthy_observations();
            break_it(&mut o);
            let report = p.evaluate(&o).expect("runs");
            assert!(!report.healthy, "{id:?} should have failed the predicate");
            let failures = report.failures();
            assert_eq!(
                failures.len(),
                1,
                "{id:?} broke {} checks, not one: {failures:?}",
                failures.len()
            );
            assert_eq!(failures[0].id, id);
        }
    }

    /// A reachable peer that will not carry the full MTU is not reachable for
    /// this cluster's purposes: it answers a small ping and then blackholes a
    /// registry pull.
    #[test]
    fn a_peer_that_will_not_carry_jumbo_frames_is_unreachable() {
        let mut o = healthy_observations();
        o.mesh_reachable = vec!["10.10.255.1".to_string()];
        let report = predicate().evaluate(&o).expect("runs");
        assert!(!report.healthy);
        assert!(report.failures()[0].detail.contains("8972"));
    }

    /// An unknown is not a failure. The type system carries the distinction so
    /// that a caller cannot accidentally read one as the other (§13.2).
    #[test]
    fn an_unrunnable_probe_is_not_a_failed_check() {
        let error = ProbeError {
            check: CheckId::ClockSynchronised.as_str(),
            attempted: "chronyc tracking".to_string(),
            because: "no such file or directory".to_string(),
        };
        assert!(error.to_string().contains("unknown, not a failure"));
    }

    #[test]
    fn a_digest_is_shortened_for_reading() {
        assert_eq!(short("sha256:0123456789abcdef"), "0123456789ab");
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
    }
}
