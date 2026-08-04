//! The session database (`SPEC.md` §15.1, §16.1).
//!
//! SQLite on `lv_data`, which §5.6 records as a single copy with no replica and
//! no off-site target. That is tolerable only because of what is allowed to live
//! there: a session record is reconstructible from the running container, so
//! losing this file loses bookkeeping rather than work.
//!
//! Every `rusqlite` failure is wrapped into [`ApiError::StoreUnavailable`]
//! before it can reach a caller. That is R5 in practice: a caller who is told
//! "the session store is unavailable" knows the control plane's storage is the
//! problem, which is actionable in a way a leaked driver error is not.

use rusqlite::Connection;

use crate::rollout::{Quarantine, RolloutState};
use crate::session::{Session, SessionState};
use crate::ApiError;

/// The session registry and rollout state store.
pub struct Store {
    connection: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

/// Wrap a driver failure into the one error a caller may see (R5).
fn unavailable(attempted: &str, e: impl std::fmt::Display) -> ApiError {
    ApiError::StoreUnavailable {
        attempted: attempted.to_string(),
        because: e.to_string(),
    }
}

impl Store {
    /// Open the database at `path`, creating the schema if it is not there.
    pub fn open(path: &str) -> Result<Self, ApiError> {
        let connection =
            Connection::open(path).map_err(|e| unavailable(&format!("open {path}"), e))?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    /// An in-memory store, for exercising the schema and the queries against
    /// their oracle without a disk.
    pub fn in_memory() -> Result<Self, ApiError> {
        let connection =
            Connection::open_in_memory().map_err(|e| unavailable("open in-memory store", e))?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), ApiError> {
        // `IF NOT EXISTS` rather than a version table: one schema, created once,
        // and a change to it is a change that ships as two releases like every
        // other cross-version change (§13.6).
        self.connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS session (
                     id               TEXT PRIMARY KEY,
                     owner            TEXT NOT NULL,
                     repo             TEXT NOT NULL,
                     git_ref          TEXT NOT NULL,
                     config_path      TEXT NOT NULL,
                     image_digest     TEXT NOT NULL,
                     host             TEXT NOT NULL,
                     state            TEXT NOT NULL,
                     created_at       INTEGER NOT NULL,
                     last_attached_at INTEGER NOT NULL,
                     memory_gib       INTEGER NOT NULL,
                     dirty            INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS quarantine (
                     digest TEXT NOT NULL,
                     node   TEXT NOT NULL,
                     at     INTEGER NOT NULL,
                     PRIMARY KEY (digest, node)
                 );",
            )
            .map_err(|e| unavailable("create schema", e))
    }

    /// Every session, oldest first.
    pub fn sessions(&self) -> Result<Vec<Session>, ApiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, owner, repo, git_ref, config_path, image_digest, host, state,
                        created_at, last_attached_at, memory_gib, dirty
                 FROM session ORDER BY created_at, id",
            )
            .map_err(|e| unavailable("list sessions", e))?;

        let rows = statement
            .query_map([], |row| {
                Ok(Session::new(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    // An unrecognised state token is not a reason to drop the
                    // row: a session whose state this build does not understand
                    // still exists, and hiding it would make it unreclaimable
                    // and invisible at once. `Stopped` is the reading that acts
                    // on nothing.
                    SessionState::parse(&row.get::<_, String>(7)?).unwrap_or(SessionState::Stopped),
                    row.get::<_, i64>(8)? as u64,
                    row.get::<_, i64>(9)? as u64,
                    row.get::<_, i64>(10)? as u32,
                    row.get::<_, i64>(11)? != 0,
                ))
            })
            .map_err(|e| unavailable("list sessions", e))?;

        // Collected one at a time rather than through a turbofish. `collect` into
        // an inferred `Result` would name a driver error type in a signature R5
        // reads, and the gate is a grep on purpose: the fix is to wrap each row's
        // failure where it happens, which is also where the context is.
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(|e| unavailable("list sessions", e))?);
        }
        Ok(sessions)
    }

    /// One session, or [`ApiError::NotFound`].
    pub fn session(&self, id: &str) -> Result<Session, ApiError> {
        self.sessions()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| ApiError::NotFound {
                kind: "session",
                id: id.to_string(),
            })
    }

    /// Insert or replace a session.
    pub fn put(&self, session: &Session) -> Result<(), ApiError> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO session
                   (id, owner, repo, git_ref, config_path, image_digest, host, state,
                    created_at, last_attached_at, memory_gib, dirty)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                rusqlite::params![
                    session.id,
                    session.owner,
                    session.repo,
                    session.git_ref,
                    session.config_path,
                    session.image_digest,
                    session.host,
                    session.state.as_str(),
                    session.created_at as i64,
                    session.last_attached_at as i64,
                    session.memory_gib as i64,
                    i64::from(session.is_dirty()),
                ],
            )
            .map(|_| ())
            .map_err(|e| unavailable(&format!("write session {}", session.id), e))
    }

    /// Record a rollback (§13.4).
    pub fn quarantine(&self, digest: &str, node: &str, at: u64) -> Result<(), ApiError> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO quarantine (digest, node, at) VALUES (?1,?2,?3)",
                rusqlite::params![digest, node, at as i64],
            )
            .map(|_| ())
            .map_err(|e| unavailable(&format!("quarantine {digest}"), e))
    }

    /// The rollout state, including every quarantined digest.
    pub fn rollout_state(&self) -> Result<RolloutState, ApiError> {
        let mut statement = self
            .connection
            .prepare("SELECT digest, node, at FROM quarantine ORDER BY at, digest")
            .map_err(|e| unavailable("read rollout state", e))?;
        let rows = statement
            .query_map([], |row| {
                Ok(Quarantine {
                    digest: row.get::<_, String>(0)?,
                    node: row.get::<_, String>(1)?,
                    at: row.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| unavailable("read rollout state", e))?;

        let mut quarantined = Vec::new();
        for row in rows {
            quarantined.push(row.map_err(|e| unavailable("read rollout state", e))?);
        }

        let booted = self
            .sessions()?
            .iter()
            .map(|s| (s.host.clone(), s.image_digest.clone()))
            .collect();

        Ok(RolloutState {
            target: None,
            quarantined,
            booted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> Session {
        Session::new(
            id,
            "alex@uor.foundation",
            "afflom/cluster",
            "main",
            ".devcontainer/devcontainer.json",
            "sha256:aaaa",
            "n2",
            SessionState::Running,
            10,
            20,
            4,
            false,
        )
    }

    /// `CC-02`: an identifier that names nothing is a sanctioned condition with
    /// a status a caller can act on --- not a panic and not a 500.
    #[test]
    fn an_unknown_session_is_a_sanctioned_condition_cc_02() {
        let store = Store::in_memory().expect("schema");
        let error = store.session("nothing").expect_err("must not be found");
        assert_eq!(
            error,
            ApiError::NotFound {
                kind: "session",
                id: "nothing".to_string()
            }
        );
        assert_eq!(error.status(), 404);
    }

    #[test]
    fn a_session_round_trips_through_the_store_cc_02() {
        let store = Store::in_memory().expect("schema");
        store.put(&session("abc123")).expect("write");
        let read = store.session("abc123").expect("read");
        assert_eq!(read, session("abc123"));
        assert_eq!(store.sessions().expect("list").len(), 1);
    }

    /// A quarantine survives the restart that follows the rollback that caused
    /// it --- which is the only reason to persist it at all (§13.4).
    #[test]
    fn a_quarantine_is_persisted_cc_03() {
        let store = Store::in_memory().expect("schema");
        store.quarantine("sha256:bad", "n3", 1_000).expect("write");
        store
            .quarantine("sha256:bad", "n3", 1_100)
            .expect("rewrite");

        let state = store.rollout_state().expect("read");
        assert_eq!(state.quarantined.len(), 1, "one node, one digest, one row");
        assert!(state.is_quarantined("sha256:bad"));
    }
}
