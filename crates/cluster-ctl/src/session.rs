//! Devcontainer session records (`SPEC.md` §15.1, §15.2).
//!
//! One record per devcontainer. What makes a session recoverable is not its
//! process state but the four fields that say how to rebuild it: the repository,
//! the ref, the `devcontainer.json` path, and the image digest it was built
//! from. A session is therefore never "lost" in the way a running container is;
//! it is at worst rebuilt.
//!
//! # Dirty
//!
//! `dirty` is the one flag that overrides the retention policy, and §15.2 is
//! emphatic about *when* it is computed: immediately before any destructive
//! step, never read from cache. A cached `dirty = false` from a week ago is not
//! an observation about the workspace as it is now, and acting on one is how a
//! system deletes work somebody did on Tuesday.
//!
//! The type here enforces that: [`Session::dirty`] is not a public field, and
//! the only way to reach a destructive step is through
//! [`Session::with_recomputed_dirty`], which takes a freshly observed value.

use serde::{Deserialize, Serialize};

use crate::ApiError;

/// The longest identifier a session may carry.
///
/// An SSH alias, a container name, a directory name and a tunnel name are all
/// built from it, and the shortest of those limits is the hostname label's 63
/// characters. `dc-` is prepended to make the alias, so 60 is what is left.
pub const MAX_SESSION_ID: usize = 60;

/// Check that an identifier is one every consumer of it can carry (§15.1).
///
/// # Why this exists
///
/// The identifier is not merely a database key. It becomes a directory name
/// under the workspace root, a path segment in a URL the agent is asked for, a
/// `podman exec` container name, and the `dc-` SSH alias. Each of those has a
/// grammar, and the identifier was checked against none of them --- the only
/// test at creation was that no session already had it.
///
/// Three consequences, in ascending order of unpleasantness. An identifier with
/// a space makes an SSH alias nothing resolves. One with a `/` makes a URL
/// whose path is not the path anybody meant. And one with `..` in it makes
/// `workspace_of` a traversal out of the workspace root, which is where the
/// dirty computation reads and the migration copies from.
///
/// Restricted rather than escaped. Escaping is right when the input is arbitrary
/// and has to survive; here the input is a name somebody chooses, four different
/// grammars have to accept it, and the intersection of them is small and easy to
/// state. A name that is refused can simply be a different name.
pub fn check_session_id(id: &str) -> Result<(), ApiError> {
    let refuse = |because: &str| {
        Err(ApiError::NotPermitted {
            attempted: format!("use `{id}` as a session identifier"),
            because: because.to_string(),
        })
    };

    if id.is_empty() {
        return refuse("it is empty");
    }
    if id.len() > MAX_SESSION_ID {
        return refuse(&format!(
            "it is {} characters. The identifier becomes an SSH alias, a container name \
             and a directory name, and the shortest of those limits is {MAX_SESSION_ID} \
             (§15.1)",
            id.len()
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return refuse(
            "it may hold only lowercase letters, digits and hyphens. The identifier \
             becomes a directory name, a URL path segment, a container name and an SSH \
             alias, and each of those has a grammar (§15.1)",
        );
    }
    if id.starts_with('-') || id.ends_with('-') {
        return refuse(
            "a hostname label may not begin or end with a hyphen, and the `dc-` alias is one",
        );
    }
    Ok(())
}

/// Where a session is in its lifecycle (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    /// Being built for the first time.
    Creating,
    /// Up, and reachable at its `dc-` alias.
    Running,
    /// Deliberately stopped, and restartable.
    Stopped,
    /// Being moved between nodes by a drain (§14.3).
    Migrating,
    /// Snapshotted and removed; restorable from the snapshot and its
    /// `devcontainer.json` (§15.3).
    Archived,
    /// The archive has been deleted. Not reversible.
    Purged,
}

impl SessionState {
    /// The token used on the wire and in the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Migrating => "migrating",
            Self::Archived => "archived",
            Self::Purged => "purged",
        }
    }

    /// Parse the token, or `None` if it names no state.
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "creating" => Self::Creating,
            "running" => Self::Running,
            "stopped" => Self::Stopped,
            "migrating" => Self::Migrating,
            "archived" => Self::Archived,
            "purged" => Self::Purged,
            _ => return None,
        })
    }

    /// Was this session stopped by a drain rather than by reclamation?
    ///
    /// §15.4: a session archived because it was idle must not be confused with
    /// one stopped because its host was updating, and `state` is what
    /// distinguishes them.
    pub const fn is_drain_state(self) -> bool {
        matches!(self, Self::Migrating)
    }
}

/// One devcontainer, as the control plane records it (§15.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Short stable identifier; the `dc-<id>` SSH alias.
    pub id: String,
    /// Tailscale login of whoever owns it (§16.2).
    pub owner: String,
    /// The repository it serves.
    pub repo: String,
    /// The ref it was built from.
    pub git_ref: String,
    /// Where its `devcontainer.json` lives in that repository.
    pub config_path: String,
    /// The image digest it was built from.
    pub image_digest: String,
    /// The node currently hosting it, updated by a migration (§14.3).
    pub host: String,
    /// Where it is in its lifecycle.
    pub state: SessionState,
    /// Unix seconds when it was created.
    pub created_at: u64,
    /// Unix seconds of the last SSH connection or tunnel attachment. Drives
    /// reclamation (§15.3).
    pub last_attached_at: u64,
    /// Declared memory, for the migration capacity cap (§14.3).
    pub memory_gib: u32,

    /// Whether the workspace has work that is not anywhere else.
    ///
    /// Private, and deliberately so. §15.2 requires this to be recomputed
    /// immediately before any destructive step rather than read from cache, and
    /// a public field is an invitation to read the cached one. Reach it through
    /// [`Session::is_dirty`] for display and through
    /// [`Session::with_recomputed_dirty`] before acting.
    dirty: bool,
}

impl Session {
    /// Build a record. `dirty` is whatever was last observed and is only good
    /// for display until it is recomputed.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        owner: impl Into<String>,
        repo: impl Into<String>,
        git_ref: impl Into<String>,
        config_path: impl Into<String>,
        image_digest: impl Into<String>,
        host: impl Into<String>,
        state: SessionState,
        created_at: u64,
        last_attached_at: u64,
        memory_gib: u32,
        dirty: bool,
    ) -> Self {
        Self {
            id: id.into(),
            owner: owner.into(),
            repo: repo.into(),
            git_ref: git_ref.into(),
            config_path: config_path.into(),
            image_digest: image_digest.into(),
            host: host.into(),
            state,
            created_at,
            last_attached_at,
            memory_gib,
            dirty,
        }
    }

    /// The last observed dirty flag. For display only.
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// A copy carrying a freshly observed dirty flag.
    ///
    /// The only way to obtain a [`Reclaimable`], which is the only thing
    /// [`crate::reclaim::decide`] will act on. That chain is what makes §15.2's
    /// "never read from cache" a property of the code rather than a rule
    /// somebody has to remember.
    pub fn with_recomputed_dirty(&self, observed: DirtyObservation) -> Reclaimable {
        Reclaimable {
            session: Self {
                dirty: observed.dirty,
                ..self.clone()
            },
        }
    }

    /// A copy in a new lifecycle state, carrying the same dirty flag.
    ///
    /// A method rather than struct-update syntax, because `dirty` is private
    /// (§15.2) --- and that is the point: a caller cannot silently reset the
    /// flag while moving a session between states, which is precisely how a
    /// dirty workspace would come to look clean on its way to being purged.
    pub fn with_state(&self, state: SessionState) -> Self {
        Self {
            state,
            ..self.clone()
        }
    }

    /// A copy recording an attachment just now (§15.1).
    ///
    /// `last_attached_at` drives every reclamation threshold, so this is the one
    /// field a session's *owner* moves. A session somebody is using that looks
    /// idle is a session archived out from under them.
    pub fn with_attachment(&self, at: u64) -> Self {
        Self {
            last_attached_at: at,
            ..self.clone()
        }
    }

    /// A copy on a new host, after a migration (§14.3).
    pub fn with_host(&self, host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            ..self.clone()
        }
    }

    /// How long since anyone attached, in seconds.
    ///
    /// Saturating: a `last_attached_at` in the future is a clock that jumped,
    /// and the honest answer is "not idle at all" rather than an enormous
    /// number that would archive a session somebody is using. §10.1's clock
    /// check exists so this case is reported rather than merely survived.
    pub const fn idle_seconds(&self, now: u64) -> u64 {
        now.saturating_sub(self.last_attached_at)
    }
}

/// A freshly taken observation of a workspace's state (§15.2).
///
/// Constructed only by whatever actually looked at the worktree, which is what
/// makes it evidence rather than a value someone passed along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyObservation {
    /// Whether the workspace has uncommitted tracked changes, unpushed commits
    /// on any branch, or untracked non-ignored files (§15.2).
    pub dirty: bool,
}

impl DirtyObservation {
    /// Record what was seen in the worktree.
    pub const fn observed(dirty: bool) -> Self {
        Self { dirty }
    }
}

/// A session whose dirty flag was recomputed just now.
///
/// [`crate::reclaim::decide`] takes one of these and nothing else, so a
/// destructive decision cannot be reached from a cached flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reclaimable {
    session: Session,
}

impl Reclaimable {
    /// The session, with its fresh flag.
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// Whether the workspace is dirty, as just observed.
    pub const fn dirty(&self) -> bool {
        self.session.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CC-10`: an identifier is one every consumer of it can carry.
    ///
    /// It becomes a directory under the workspace root, a URL path segment the
    /// agent is asked for, a `podman exec` container name and the `dc-` SSH
    /// alias. The only check used to be that nothing else had claimed it.
    #[test]
    fn an_identifier_every_consumer_can_carry_cc_10() {
        for ok in [
            "abc123",
            "a",
            "my-project-2",
            "0",
            &"a".repeat(MAX_SESSION_ID),
        ] {
            assert!(
                check_session_id(ok).is_ok(),
                "`{ok}` is a name somebody would reasonably choose"
            );
        }

        // Each of these reaches a different consumer, and breaks it differently.
        for (bad, why) in [
            ("", "empty"),
            ("../../etc", "a traversal out of the workspace root"),
            ("a/b", "a URL path segment that is two segments"),
            ("a b", "an SSH alias nothing resolves"),
            ("Abc", "a container name podman lowercases"),
            ("-lead", "a hostname label may not begin with a hyphen"),
            ("trail-", "a hostname label may not end with a hyphen"),
            ("a.b", "a dot makes `dc-a.b` two labels"),
            ("a\nb", "a newline in a request line"),
            ("a\"b", "a quote in a document somebody parses"),
            (&"a".repeat(MAX_SESSION_ID + 1), "longer than a label"),
        ] {
            assert!(
                check_session_id(bad).is_err(),
                "`{bad}` must be refused: {why}"
            );
        }

        // The refusal is a sanctioned condition with a status that says what to
        // do, not a panic and not a silent rename.
        let err = check_session_id("../etc").expect_err("a traversal");
        assert_eq!(err.status(), 409);

        // And the one that mattered most: an identifier crafted to put a
        // `"dirty":false` substring into the agent's answer, which is what the
        // control plane used to search that answer for.
        assert!(
            check_session_id("x\",\"dirty\":false,\"y\":\"").is_err(),
            "this identifier is why the control plane parses rather than searches"
        );
    }

    fn session(dirty: bool, last_attached_at: u64) -> Session {
        Session::new(
            "abc123",
            "alex@uor.foundation",
            "afflom/cluster",
            "main",
            ".devcontainer/devcontainer.json",
            "sha256:aaaa",
            "node2",
            SessionState::Running,
            0,
            last_attached_at,
            4,
            dirty,
        )
    }

    /// `CG-05`: the flag a reclamation decision sees is the one just observed,
    /// not the one stored.
    #[test]
    fn a_reclaim_decision_uses_a_freshly_observed_flag_cg_05() {
        // The record says clean; somebody edited the workspace since.
        let stored = session(false, 0);
        assert!(!stored.is_dirty());

        let fresh = stored.with_recomputed_dirty(DirtyObservation::observed(true));
        assert!(
            fresh.dirty(),
            "§15.2: dirty is recomputed immediately before any destructive step, \
             never read from cache"
        );
        // The stored record is untouched: recomputation produces a new value
        // rather than mutating the one the database holds.
        assert!(!stored.is_dirty());
    }

    /// A clock that jumped backwards must not make a session look ancient.
    #[test]
    fn a_future_attachment_is_not_idle_cg_05() {
        let s = session(false, 1_000);
        assert_eq!(s.idle_seconds(900), 0);
        assert_eq!(s.idle_seconds(1_600), 600);
    }

    #[test]
    fn every_state_round_trips_through_its_token_cg_05() {
        for state in [
            SessionState::Creating,
            SessionState::Running,
            SessionState::Stopped,
            SessionState::Migrating,
            SessionState::Archived,
            SessionState::Purged,
        ] {
            assert_eq!(SessionState::parse(state.as_str()), Some(state));
        }
        assert_eq!(SessionState::parse("nonsense"), None);
    }
}
