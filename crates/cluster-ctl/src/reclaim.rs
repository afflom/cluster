//! Reclamation, and the exemption that is the point of it (`SPEC.md` §15.3).
//!
//! Reclaiming resources from an abandoned container is housekeeping. Deleting
//! someone's uncommitted work because a timer expired is a betrayal, and a
//! system that does it once is never trusted again. The cost of holding a dirty
//! archive forever is a few gigabytes on a 2 TB disk.
//!
//! So [`decide`] takes a [`Reclaimable`] --- a session whose dirty flag was
//! recomputed just now --- and there is no path from a stored record to a
//! destructive [`Action`] that does not pass through a fresh observation.
//!
//! # Reclamation is not drain
//!
//! They are separate mechanisms with separate triggers, and reclamation never
//! runs during a rollout (§15.4). [`decide`] refuses outright while a rollout is
//! in progress rather than leaving that to the caller, because "the timer fired
//! during a rollout" is exactly the situation in which a session stopped by a
//! drain looks, to a policy that only reads timestamps, like a session nobody
//! wants.

use std::fmt;

use crate::session::{Reclaimable, SessionState};

/// The thresholds, rendered from `model/policy.toml` (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// Age at which the owner is notified.
    pub notify_after_days: u32,
    /// Age at which the session is snapshotted and archived. Reversible.
    pub archive_after_days: u32,
    /// Age at which the archive is deleted. Not reversible.
    pub purge_after_days: u32,
}

impl Thresholds {
    fn seconds(days: u32) -> u64 {
        u64::from(days) * 24 * 60 * 60
    }
}

/// What reclamation will do to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Young enough, or already at rest. Nothing happens.
    Leave,
    /// Notify the owner and mark the session idle in the UI.
    Notify,
    /// Snapshot the workspace and volumes, remove the container, and archive.
    /// Reversible: a restore rebuilds from the snapshot and the
    /// `devcontainer.json`.
    Archive,
    /// Delete the archive. **Not reversible.**
    Purge,
    /// Old enough to purge, but dirty. Held indefinitely and listed in the UI
    /// as requiring acknowledgement (§15.3).
    HoldDirty,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leave => write!(f, "leave"),
            Self::Notify => write!(f, "notify"),
            Self::Archive => write!(f, "archive"),
            Self::Purge => write!(f, "purge"),
            Self::HoldDirty => write!(
                f,
                "hold: the workspace is dirty. Deleting someone's uncommitted work \
                 because a timer expired is a betrayal (§15.3)"
            ),
        }
    }
}

impl Action {
    /// Whether this action destroys something that cannot be rebuilt.
    pub const fn is_irreversible(self) -> bool {
        matches!(self, Self::Purge)
    }
}

/// Whether a rollout is in progress (§15.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutStatus {
    /// No rollout. Reclamation may run.
    Quiet,
    /// A rollout is under way. Reclamation does not run at all.
    InProgress,
}

/// Decide what happens to one session.
///
/// `now` is Unix seconds. Taking it as a parameter rather than reading a clock
/// is what makes every threshold in §15.3 checkable without waiting ninety days.
pub fn decide(
    reclaimable: &Reclaimable,
    thresholds: &Thresholds,
    now: u64,
    rollout: RolloutStatus,
) -> Action {
    // §15.4: reclamation never runs during a rollout. Refused here rather than
    // by the caller, because a session `Migrating` because its host is updating
    // is indistinguishable, to a policy that reads only timestamps, from one
    // nobody wants.
    if rollout == RolloutStatus::InProgress {
        return Action::Leave;
    }

    let session = reclaimable.session();

    // A session already purged has nothing left to reclaim, and one being moved
    // by a drain belongs to the drain (§15.4).
    if session.state == SessionState::Purged || session.state.is_drain_state() {
        return Action::Leave;
    }

    let idle = session.idle_seconds(now);

    // The dirty exemption, checked before the purge threshold rather than after.
    // Order matters: a check placed after would be a check that runs only when
    // the code reaches it, and the whole point is that this one always does.
    if idle >= Thresholds::seconds(thresholds.purge_after_days) {
        if reclaimable.dirty() {
            return Action::HoldDirty;
        }
        // Only an archive is purged. A running container at ninety days is
        // archived first, so the reversible step is never skipped.
        return if session.state == SessionState::Archived {
            Action::Purge
        } else {
            Action::Archive
        };
    }

    if idle >= Thresholds::seconds(thresholds.archive_after_days) {
        return if session.state == SessionState::Archived {
            Action::Leave
        } else {
            Action::Archive
        };
    }

    if idle >= Thresholds::seconds(thresholds.notify_after_days) {
        return Action::Notify;
    }

    Action::Leave
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{DirtyObservation, Session};

    const DAY: u64 = 24 * 60 * 60;

    fn thresholds() -> Thresholds {
        Thresholds {
            notify_after_days: 14,
            archive_after_days: 30,
            purge_after_days: 90,
        }
    }

    /// Far enough from the epoch that even the ten-thousand-day cases below
    /// have a real `last_attached_at` to subtract from.
    const NOW: u64 = 20_000 * DAY;

    fn at(idle_days: u64, state: SessionState, dirty: bool) -> Reclaimable {
        let now = NOW;
        Session::new(
            "abc123",
            "alex@uor.foundation",
            "afflom/cluster",
            "main",
            ".devcontainer/devcontainer.json",
            "sha256:aaaa",
            "n2",
            state,
            0,
            now - idle_days * DAY,
            4,
            // Deliberately the opposite of the observation, so a test that
            // accidentally read the stored flag would fail.
            !dirty,
        )
        .with_recomputed_dirty(DirtyObservation::observed(dirty))
    }

    fn decide_at(idle_days: u64, state: SessionState, dirty: bool) -> Action {
        decide(
            &at(idle_days, state, dirty),
            &thresholds(),
            NOW,
            RolloutStatus::Quiet,
        )
    }

    /// `CG-01`: a session idle past the notify threshold is notified.
    #[test]
    fn an_idle_session_is_notified_at_its_threshold_cg_01() {
        assert_eq!(decide_at(13, SessionState::Running, false), Action::Leave);
        assert_eq!(decide_at(14, SessionState::Running, false), Action::Notify);
        assert_eq!(decide_at(29, SessionState::Running, false), Action::Notify);
    }

    /// `CG-02`: archiving happens at the threshold and is reversible.
    #[test]
    fn an_idle_session_is_archived_at_its_threshold_cg_02() {
        assert_eq!(decide_at(30, SessionState::Running, false), Action::Archive);
        assert!(!Action::Archive.is_irreversible());
        // Already archived and not yet old enough to purge: nothing to do.
        assert_eq!(decide_at(60, SessionState::Archived, false), Action::Leave);
        assert_eq!(decide_at(90, SessionState::Archived, false), Action::Purge);
        assert!(Action::Purge.is_irreversible());

        // A *dirty* session is archived at this threshold like any other. The
        // exemption in §15.3's table is on the ninety-day row alone, and it has
        // to be: archiving is reversible, so refusing to archive dirty sessions
        // would hold a container open forever rather than protecting anything.
        assert_eq!(decide_at(30, SessionState::Running, true), Action::Archive);
        assert_eq!(decide_at(60, SessionState::Running, true), Action::Archive);
    }

    /// `CG-03`: a dirty session is never purged, at any age.
    ///
    /// The class rule §19.2 anticipates for the first `CG-` row: retention that
    /// is only tested on clean workspaces is retention that has never been
    /// tested against the failure that matters.
    #[test]
    fn a_dirty_session_is_never_purged_cg_03() {
        for idle_days in [90, 365, 10_000] {
            for state in [
                SessionState::Running,
                SessionState::Stopped,
                SessionState::Archived,
            ] {
                let action = decide_at(idle_days, state, true);
                assert_eq!(
                    action,
                    Action::HoldDirty,
                    "a dirty workspace at {idle_days} days in {state:?} must be held"
                );
                assert!(!action.is_irreversible());
            }
        }

        // And the clean control does purge, so the test above is not passing
        // because nothing ever purges.
        assert_eq!(
            decide_at(90, SessionState::Archived, true),
            Action::HoldDirty
        );
        assert_eq!(decide_at(90, SessionState::Archived, false), Action::Purge);
    }

    /// A running session that reaches the purge threshold is archived first, so
    /// the reversible step is never skipped.
    #[test]
    fn the_reversible_step_is_never_skipped_cg_02() {
        assert_eq!(decide_at(90, SessionState::Running, false), Action::Archive);
    }

    /// `CG-04`: reclamation does not run during a rollout.
    #[test]
    fn reclamation_does_not_run_during_a_rollout_cg_04() {
        let ancient = at(10_000, SessionState::Archived, false);
        assert_eq!(
            decide(&ancient, &thresholds(), NOW, RolloutStatus::Quiet),
            Action::Purge
        );
        assert_eq!(
            decide(&ancient, &thresholds(), NOW, RolloutStatus::InProgress),
            Action::Leave,
            "§15.4: a session archived because it was idle must not be confused with \
             one stopped because its host was updating"
        );
    }

    /// A session a drain is moving belongs to the drain, not to reclamation.
    #[test]
    fn a_migrating_session_is_left_alone_cg_04() {
        assert_eq!(
            decide_at(10_000, SessionState::Migrating, false),
            Action::Leave
        );
    }

    /// Nothing acts on a purged session twice.
    #[test]
    fn a_purged_session_is_left_alone_cg_02() {
        assert_eq!(
            decide_at(10_000, SessionState::Purged, false),
            Action::Leave
        );
    }
}
