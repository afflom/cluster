//! The rollout ordering predicate, drain, and apply (`SPEC.md` §13, §14).
//!
//! Nodes update themselves when a release is published. No operator is present,
//! so every step is either safe to take unattended or is a halt --- and the
//! whole crate is arranged around making that distinction impossible to blur.
//!
//! [`rollout::admits`] decides *whether* this node may proceed. [`drain::plan`]
//! decides *what happens to the work* first. Both are pure functions, and
//! [`apply`] is the only place that acts.
//!
//! That split is what makes §13's guarantees testable without a cluster: the
//! claim that at most one node updates at a time is a property of `admits`, and
//! the claim that the migration cap is enforced rather than exceeded silently is
//! a property of `plan`. Neither needs a machine to check, and both would be
//! nearly uncheckable if they were tangled up with `bootc upgrade`.

#![deny(missing_docs)]

pub mod apply;
pub mod drain;
pub mod rollout;

use std::fmt;

pub use apply::{Applier, SystemApplier};
pub use drain::{Budget, BudgetOutcome, Capacity, OnExceed, Plan, Step, Strategy, Workload};
pub use rollout::{admits, Decision, Observation, PeerReport};

/// The one error a caller of this crate can see (R5).
///
/// Sanctioned by `CU-01` in `model/ids.toml`. It reports that the rollout could
/// not proceed: a registry that would not answer, a signature that did not
/// verify, an upgrade that would not apply.
///
/// It deliberately does **not** cover the ordinary outcomes. "This node is not
/// next" and "a peer is unhealthy" are [`Decision`]s, not errors --- the first
/// is the normal middle of a rollout and the second is a halt that alerts.
/// Making them errors would put three very different things behind one `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutError {
    /// Which stage could not be completed.
    pub stage: Stage,
    /// What was attempted.
    pub attempted: String,
    /// Why it could not be done.
    pub because: String,
}

/// Where in the rollout a failure happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Asking a registry what `:stable` resolves to (§13.1).
    Resolve,
    /// Verifying the signature before staging (§12.3).
    Verify,
    /// Reading a peer's health (§13.2).
    Observe,
    /// Draining the node (§14).
    Drain,
    /// `bootc upgrade` and the reboot (§13.3).
    Apply,
    /// Recording a quarantined digest (§13.4).
    Quarantine,
}

impl Stage {
    /// The stage's name, as it appears in a log line and an alert.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Verify => "verify",
            Self::Observe => "observe",
            Self::Drain => "drain",
            Self::Apply => "apply",
            Self::Quarantine => "quarantine",
        }
    }
}

impl fmt::Display for RolloutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: `{}` did not complete: {}",
            self.stage.as_str(),
            self.attempted,
            self.because
        )
    }
}

impl std::error::Error for RolloutError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A halt is a decision, not an error. The two are different types so a
    /// caller cannot handle one where it meant the other (§13.5).
    #[test]
    fn a_halt_is_a_decision_and_not_an_error_cu_03() {
        let halt = Decision::Halt("a peer is unhealthy".to_string());
        assert!(halt.halts());
        assert!(!halt.applies());

        let error = RolloutError {
            stage: Stage::Resolve,
            attempted: "resolve :stable".to_string(),
            because: "no registry answered".to_string(),
        };
        assert!(error.to_string().starts_with("resolve:"));
    }
}
