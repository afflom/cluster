//! The control plane (`SPEC.md` §15, §16).
//!
//! `cluster-ctl` is three things behind one process: the session registry
//! (§15.1), the rollout state store (§13.4), and the API the web UI speaks to
//! (§16.1).
//!
//! # R5 and an HTTP API
//!
//! §16.1 says it plainly: this is a shipped crate, and an HTTP API is precisely
//! the surface R5 exists for. Every error a caller can see is [`ApiError`], and
//! every one of its variants is a condition the model sanctions. There is no
//! "internal server error" variant, because a 500 is not a condition --- it is
//! an admission that something happened the design did not account for, and
//! wiring one in makes it permanently easy not to account for things.
//!
//! # Authentication, and what changed
//!
//! An earlier version of this crate read a Tailscale identity header and
//! justified it on the grounds that there was no authentication code here. There
//! is now: identity is GitHub's, by device flow, and [`auth`] is the fifty-odd
//! lines that make it so. §16.2 rewrites that justification rather than leaving
//! it standing over code that contradicts it.
//!
//! Authorization is still a list of logins in `model/cluster.toml`, because
//! `afflom` is a user account with no membership API to ask instead --- and
//! §16.2 states that limit rather than leaving it to be found.

#![deny(missing_docs)]

// The HTTP surface and the database. Behind `server` so that `cluster-web` can
// link the wire types for wasm32 without them (see Cargo.toml).
#[cfg(feature = "server")]
pub mod api;
#[cfg(feature = "server")]
pub mod store;

pub mod auth;
#[cfg(feature = "server")]
pub mod github;
pub mod reclaim;
pub mod rollout;
pub mod session;

use std::fmt;

pub use auth::{Authorizer, Identity, Resolver};
pub use reclaim::{decide, Action, RolloutStatus, Thresholds};
pub use rollout::{Quarantine, RolloutState};
pub use session::{DirtyObservation, Reclaimable, Session, SessionState};
#[cfg(feature = "server")]
pub use store::Store;

/// The one error a caller of this crate can see (R5).
///
/// Sanctioned by `CC-02` in `model/ids.toml`. Four conditions, each one a thing
/// a caller can do something about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The caller is not on the tailnet, or presented no identity header.
    Unauthenticated,
    /// The caller is authenticated but is not a login `model/cluster.toml`
    /// permits (§16.2).
    NotAuthorized {
        /// The login that was presented, so the operator can add it if it
        /// should have been there.
        login: String,
    },
    /// No session or digest by that name.
    NotFound {
        /// What kind of thing was looked for.
        kind: &'static str,
        /// The identifier that named nothing.
        id: String,
    },
    /// The request names a transition the lifecycle does not allow, such as
    /// purging a session that was never archived (§15.3).
    NotPermitted {
        /// What was asked for.
        attempted: String,
        /// Why the lifecycle refuses it.
        because: String,
    },
    /// The session store could not be read or written.
    ///
    /// A real condition rather than a catch-all: the database is a single file
    /// on `lv_data`, and §5.6 records that `lv_data` is a single copy. A caller
    /// that sees this knows the control plane's storage is the problem, which is
    /// actionable in a way "internal error" is not.
    StoreUnavailable {
        /// What was attempted.
        attempted: String,
        /// What the store said.
        because: String,
    },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthenticated => write!(
                f,
                "no Tailscale identity. The control plane is published by `tailscale serve` \
                 and there is no other way in (§16.2)"
            ),
            Self::NotAuthorized { login } => write!(
                f,
                "{login} is not an authorized login. Who may drive the cluster is a model \
                 fact in model/cluster.toml (§16.2)"
            ),
            Self::NotFound { kind, id } => write!(f, "no {kind} named {id}"),
            Self::NotPermitted { attempted, because } => write!(f, "{attempted}: {because}"),
            Self::StoreUnavailable { attempted, because } => write!(
                f,
                "{attempted}: the session store is unavailable: {because}"
            ),
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// The HTTP status a caller sees.
    ///
    /// Every variant maps to a status that says what the caller should do. None
    /// maps to 500: a 500 is the absence of a diagnosis, and this type exists so
    /// there is always one.
    pub const fn status(&self) -> u16 {
        match self {
            Self::Unauthenticated => 401,
            Self::NotAuthorized { .. } => 403,
            Self::NotFound { .. } => 404,
            Self::NotPermitted { .. } => 409,
            Self::StoreUnavailable { .. } => 503,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CC-02`: every reportable condition has a status that tells the caller
    /// what to do, and none of them is a 500.
    #[test]
    fn every_error_carries_an_actionable_status_cc_02() {
        let errors = [
            ApiError::Unauthenticated,
            ApiError::NotAuthorized {
                login: "someone@example.com".to_string(),
            },
            ApiError::NotFound {
                kind: "session",
                id: "abc123".to_string(),
            },
            ApiError::NotPermitted {
                attempted: "purge abc123".to_string(),
                because: "it has not been archived".to_string(),
            },
            ApiError::StoreUnavailable {
                attempted: "list sessions".to_string(),
                because: "database is locked".to_string(),
            },
        ];
        for error in &errors {
            assert_ne!(
                error.status(),
                500,
                "a 500 is the absence of a diagnosis, and this type exists so there is \
                 always one: {error}"
            );
            assert!(!error.to_string().is_empty());
        }
    }
}
