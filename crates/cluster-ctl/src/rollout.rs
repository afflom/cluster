//! Rollout state, and the quarantine that stops a bad image (`SPEC.md` §13.4).
//!
//! A node that rolls back POSTs the failed digest here as **quarantined**.
//! Quarantine is a precondition in §13.2's ordering predicate, so recording one
//! is what stops the rollout rather than merely reporting on it.
//!
//! Because `n3` moves first, a bad image is normally caught by the node whose
//! failure costs least, and `n2` and `n1` never see it. The exception is worth
//! naming and is not smoothed over here: if `n1` --- last in the sequence ---
//! fails and rolls back, the cluster is left split-version. That is a
//! legitimate, alerted state requiring a human decision, and nothing in this
//! module reconciles it silently.

use serde::{Deserialize, Serialize};

/// One quarantined digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantine {
    /// The digest that failed to boot healthy.
    pub digest: String,
    /// Which node rolled back from it.
    pub node: String,
    /// Unix seconds when it was recorded.
    pub at: u64,
}

/// What `GET /api/rollout` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutState {
    /// The digest the fleet is moving towards, when there is one.
    pub target: Option<String>,
    /// Every quarantined digest (§13.4).
    pub quarantined: Vec<Quarantine>,
    /// Nodes and the digest each has booted, for the split-version alert (§18).
    pub booted: Vec<(String, String)>,
}

impl RolloutState {
    /// Is this digest quarantined?
    pub fn is_quarantined(&self, digest: &str) -> bool {
        self.quarantined.iter().any(|q| q.digest == digest)
    }

    /// Record a rollback. Recording the same digest twice from the same node is
    /// not an error: greenboot may roll back more than once before an operator
    /// arrives, and a duplicate POST should not be a failure a node has to
    /// handle in the middle of a reboot loop.
    pub fn quarantine(&mut self, digest: impl Into<String>, node: impl Into<String>, at: u64) {
        let digest = digest.into();
        let node = node.into();
        if self
            .quarantined
            .iter()
            .any(|q| q.digest == digest && q.node == node)
        {
            return;
        }
        self.quarantined.push(Quarantine { digest, node, at });
    }

    /// Are the nodes on differing digests? §18 alerts on this after two hours.
    pub fn is_split_version(&self) -> bool {
        let mut digests: Vec<&str> = self.booted.iter().map(|(_, d)| d.as_str()).collect();
        digests.sort_unstable();
        digests.dedup();
        digests.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CC-03`: a rollback is recorded, and the record is what stops the next
    /// node from trying the same digest.
    #[test]
    fn a_rollback_quarantines_the_digest_cc_03() {
        let mut state = RolloutState::default();
        assert!(!state.is_quarantined("sha256:bad"));

        state.quarantine("sha256:bad", "n3", 1_000);
        assert!(state.is_quarantined("sha256:bad"));
        assert!(!state.is_quarantined("sha256:good"));

        // greenboot may roll back more than once before an operator arrives, and
        // a duplicate POST must not be a failure a node handles mid-reboot.
        state.quarantine("sha256:bad", "n3", 1_100);
        assert_eq!(state.quarantined.len(), 1);

        // A second node reporting the same digest is a distinct fact worth
        // keeping: it says the image is bad on more than one machine.
        state.quarantine("sha256:bad", "n2", 1_200);
        assert_eq!(state.quarantined.len(), 2);
    }

    /// The split-version condition §18 alerts on, and §13.4 refuses to
    /// reconcile silently.
    #[test]
    fn differing_digests_are_a_split_version_cc_03() {
        let booted = |pairs: &[(&str, &str)]| RolloutState {
            booted: pairs
                .iter()
                .map(|(n, d)| ((*n).to_string(), (*d).to_string()))
                .collect(),
            ..RolloutState::default()
        };

        // n1 behind its peers: the state §13.4 leaves behind when the last node
        // in the sequence rolls back, and refuses to reconcile silently.
        assert!(booted(&[
            ("n1", "sha256:old"),
            ("n2", "sha256:new"),
            ("n3", "sha256:new"),
        ])
        .is_split_version());

        assert!(!booted(&[("n1", "sha256:new"), ("n2", "sha256:new")]).is_split_version());

        assert!(!RolloutState::default().is_split_version());
    }
}
