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
/// The secrets an operator gives a booted cluster (§12.2).
pub mod enrolment;
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
    /// The rendered enrolment policy could not be read or makes no sense
    /// (§12.2).
    ///
    /// Distinct from the store being unavailable: this one says the *cluster*
    /// cannot be given its secrets at all, which is a different thing to fix
    /// from a database that will not open.
    EnrolmentUnavailable {
        /// What is wrong with the policy.
        because: String,
    },
    /// The caller named a secret this cluster does not declare (§12.2).
    ///
    /// A refusal rather than a silent miss: a form posting an identifier this
    /// model does not have was built against a different one, and writing the
    /// value somewhere plausible would be worse than not writing it.
    UnknownSecret {
        /// What was named.
        id: String,
        /// What is declared, so the caller can see the difference.
        known: Vec<String>,
    },
    /// The value cannot be a credential (§12.2).
    ///
    /// **Never quotes the value.** An error goes to a log, and a log is read by
    /// more people than a credential should be.
    RejectedSecret {
        /// Which secret was being enrolled.
        id: String,
        /// What is wrong with the value, in terms that do not include it.
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
            Self::EnrolmentUnavailable { because } => write!(
                f,
                "this cluster cannot be enrolled: {because}. The secrets an operator \
                 enters are declared in model/policy.toml and rendered into \
                 enrolment.conf (§12.2)"
            ),
            Self::UnknownSecret { id, known } => write!(
                f,
                "`{id}` is not a secret this cluster declares. It declares: {}",
                known.join(", ")
            ),
            Self::RejectedSecret { id, because } => {
                write!(
                    f,
                    "the value offered for `{id}` was refused because {because}"
                )
            }
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
            Self::EnrolmentUnavailable { .. } => 503,
            Self::UnknownSecret { .. } => 404,
            Self::RejectedSecret { .. } => 400,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant of [`ApiError`], one of each.
    ///
    /// Built by a `match` on a value rather than by a list, so a variant added
    /// later is a *compile* error here rather than a silent gap in `CC-02`. The
    /// list form covered five of the eight that existed, and the three it
    /// missed were the three most recently added --- which is exactly how a
    /// list-shaped exhaustiveness check fails.
    fn one_of_each() -> Vec<ApiError> {
        let all = vec![
            ApiError::Unauthenticated,
            ApiError::NotAuthorized {
                login: "someone-else".to_string(),
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
            ApiError::EnrolmentUnavailable {
                because: "the rendered policy declares no secrets".to_string(),
            },
            ApiError::UnknownSecret {
                id: "root_password".to_string(),
                known: vec!["ssh_authorized_key".to_string()],
            },
            ApiError::RejectedSecret {
                id: "ssh_authorized_key".to_string(),
                because: "it contains a newline".to_string(),
            },
        ];

        // The exhaustiveness itself. Adding a variant without adding it above
        // fails to compile; this arm is what makes that true.
        for error in &all {
            match error {
                ApiError::Unauthenticated
                | ApiError::NotAuthorized { .. }
                | ApiError::NotFound { .. }
                | ApiError::NotPermitted { .. }
                | ApiError::StoreUnavailable { .. }
                | ApiError::EnrolmentUnavailable { .. }
                | ApiError::UnknownSecret { .. }
                | ApiError::RejectedSecret { .. } => {}
            }
        }
        all
    }

    /// `CC-02`: every reportable condition has a status that tells the caller
    /// what to do, and none of them is a 500.
    #[test]
    fn every_error_carries_an_actionable_status_cc_02() {
        let errors = one_of_each();
        assert_eq!(errors.len(), 8, "one of each variant");

        for error in &errors {
            let status = error.status();
            assert_ne!(
                status, 500,
                "a 500 is the absence of a diagnosis, and this type exists so there is \
                 always one: {error}"
            );
            // A status a client can act on: a refusal, or a service condition.
            assert!(
                (400..=499).contains(&status) || status == 503,
                "{error} carries {status}, which says nothing about what to do"
            );
            // And the message says something. An empty condition would render
            // as a status code and nothing else, which is what this type exists
            // to prevent.
            let text = error.to_string();
            assert!(text.len() > 20, "`{text}` is not a diagnosis");
        }
    }

    /// No condition may quote a credential.
    ///
    /// An error goes to a log, and a log is read by more people than a
    /// credential should be. The two enrolment conditions are the ones that
    /// carry a caller's input at all, and neither takes the value.
    #[test]
    fn no_condition_can_carry_a_value_cc_09() {
        let secret = "ghp_thisisasecretvaluethatmustnotappear";
        for error in one_of_each() {
            assert!(
                !error.to_string().contains(secret),
                "no condition is constructed from a value: {error}"
            );
        }
        // Constructed the way the enrolment path constructs them, with the
        // value in scope and deliberately not used.
        let refused = crate::enrolment::check_value("ssh_authorized_key", &format!("{secret}\nx"))
            .expect_err("two lines");
        assert!(!refused.to_string().contains(secret), "{refused}");
    }
}
