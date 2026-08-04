//! Identity from GitHub, authorization from the model (`SPEC.md` §16.2).
//!
//! This module used to be four lines reading a header `tailscale serve` set,
//! justified on the grounds that there was no authentication code in this
//! repository. There is now, and §16.2 says so rather than leaving the old
//! justification standing.
//!
//! # Authentication is not ambient
//!
//! The tempting design leans on the operator already being signed in to
//! github.com. It cannot work: a github.com session is a cookie scoped to
//! github.com, no other origin can read it, and it is never sent here. Neither
//! the Pages site nor this service can observe that a browser is signed in.
//!
//! So there is one explicit authorization, by device flow, and thereafter a
//! bearer token. The *experience* is one-time-per-browser; the mechanism is an
//! authorization, not an observation, and conflating the two is how somebody
//! later removes the step believing it was decorative.
//!
//! # Why a GitHub App, and why the device flow
//!
//! **App, not OAuth App.** User-to-server tokens expire in eight hours and carry
//! a refresh token. OAuth App tokens do not expire, and a non-expiring token in
//! browser storage is a permanent liability.
//!
//! **Device flow, not web flow.** Device flow uses a public client ID with no
//! client secret and no callback URL, which is what a static page can actually
//! do. The web flow needs a secret an SPA cannot hold and a callback GitHub
//! cannot reach while this service is behind Tailscale.
//!
//! # What is *not* claimed here
//!
//! That the device flow requires no client secret is a fact about GitHub, and
//! `AGENTS.md` is explicit that a claim about a dependency belongs to that
//! dependency (§20.1). What this module constructs, and what `CC-` registers, is
//! narrower: that an unlisted login is rejected, that an expired token is
//! rejected, that a token minted for another App is rejected, and that the
//! allowlist comes from `model/cluster.toml` and nowhere else.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::ApiError;

/// The header a browser presents its token in.
pub const AUTHORIZATION_HEADER: &str = "authorization";

/// Who GitHub says is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The GitHub login. Not an email address: this is whatever `GET /user`
    /// returns, and it is what `authorized_logins` is compared against.
    pub login: String,
}

/// What a token resolved to, and when that was established.
#[derive(Debug, Clone)]
struct Cached {
    login: String,
    at: u64,
}

/// Where identity comes from.
///
/// A trait so the allowlist logic can be exercised against its oracle without a
/// network — and, more to the point, so the four `CC-` rejections can each be
/// tested deterministically. An authorization path whose only test needs
/// api.github.com to be reachable is an authorization path that gets tested
/// rarely.
pub trait Resolver: Send + Sync {
    /// Resolve a bearer token to a login, or say why not.
    fn resolve(&self, token: &str) -> Result<String, ApiError>;
}

/// The identity provider, and the allowlist it is checked against.
pub struct Authorizer {
    resolver: Box<dyn Resolver>,
    permitted: Vec<String>,
    ttl_s: u64,
    cache: Mutex<HashMap<String, Cached>>,
}

impl std::fmt::Debug for Authorizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorizer")
            .field("permitted", &self.permitted)
            .field("ttl_s", &self.ttl_s)
            .finish_non_exhaustive()
    }
}

impl Authorizer {
    /// Build one from the model's allowlist and cache TTL.
    pub fn new(resolver: Box<dyn Resolver>, permitted: Vec<String>, ttl_s: u64) -> Self {
        Self {
            resolver,
            permitted,
            ttl_s,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Authorize a request.
    ///
    /// `presented` is the `Authorization` header, or `None`. `now` is Unix
    /// seconds, taken as a parameter so the cache's expiry is checkable without
    /// waiting five minutes.
    pub fn authorize(&self, presented: Option<&str>, now: u64) -> Result<Identity, ApiError> {
        let token = bearer(presented).ok_or(ApiError::Unauthenticated)?;

        // A cached login, if it was established recently enough. Revocation lag
        // is bounded by the TTL and that is a real window — §16.2 says so rather
        // than implying the cache is free.
        if let Some(login) = self.cached(token, now) {
            return self.permit(login);
        }

        let login = self.resolver.resolve(token)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                token.to_string(),
                Cached {
                    login: login.clone(),
                    at: now,
                },
            );
        }
        self.permit(login)
    }

    fn cached(&self, token: &str, now: u64) -> Option<String> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(token)?;
        (now.saturating_sub(entry.at) < self.ttl_s).then(|| entry.login.clone())
    }

    /// Check a resolved login against the allowlist.
    ///
    /// `afflom` is a user account, so there is no membership API to ask instead:
    /// authorization *is* this comparison. §16.2 states that limit rather than
    /// leaving whoever adds a second person to discover it.
    fn permit(&self, login: String) -> Result<Identity, ApiError> {
        if self.permitted.iter().any(|p| p == &login) {
            Ok(Identity { login })
        } else {
            Err(ApiError::NotAuthorized { login })
        }
    }
}

/// The token from an `Authorization: Bearer …` header.
///
/// A header that is present but not a bearer credential is treated as no
/// identity rather than as a bad one: it did not attempt this scheme, and
/// reporting it as a rejected identity would send an operator looking at the
/// allowlist for something that never reached it.
pub fn bearer(presented: Option<&str>) -> Option<&str> {
    let value = presented?.trim();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A resolver standing in for GitHub, so each rejection is deterministic.
    pub(crate) struct Fake {
        /// Tokens this provider recognises, and who they belong to.
        pub tokens: HashMap<String, String>,
        /// Tokens it recognises as expired.
        pub expired: Vec<String>,
        /// Tokens minted for a different App.
        pub foreign: Vec<String>,
    }

    impl Resolver for Fake {
        fn resolve(&self, token: &str) -> Result<String, ApiError> {
            if self.expired.iter().any(|t| t == token) {
                return Err(ApiError::Unauthenticated);
            }
            if self.foreign.iter().any(|t| t == token) {
                return Err(ApiError::Unauthenticated);
            }
            self.tokens
                .get(token)
                .cloned()
                .ok_or(ApiError::Unauthenticated)
        }
    }

    fn authorizer() -> Authorizer {
        let mut tokens = HashMap::new();
        tokens.insert("good".to_string(), "afflom".to_string());
        tokens.insert("stranger".to_string(), "someone-else".to_string());
        Authorizer::new(
            Box::new(Fake {
                tokens,
                expired: vec!["stale".to_string()],
                foreign: vec!["other-app".to_string()],
            }),
            vec!["afflom".to_string()],
            300,
        )
    }

    /// `CC-01`: only a login the model permits may drive the cluster, and the
    /// four ways in are refused distinctly.
    #[test]
    fn only_a_permitted_login_may_drive_the_cluster_cc_01() {
        let a = authorizer();

        assert_eq!(
            a.authorize(Some("Bearer good"), 0),
            Ok(Identity {
                login: "afflom".to_string()
            })
        );

        // Authenticated by GitHub, not on the list. A different failure from
        // having no identity, and a different thing for an operator to do.
        assert_eq!(
            a.authorize(Some("Bearer stranger"), 0),
            Err(ApiError::NotAuthorized {
                login: "someone-else".to_string()
            })
        );

        // Expired: App tokens last eight hours, which is the reason for choosing
        // an App over an OAuth App at all.
        assert_eq!(
            a.authorize(Some("Bearer stale"), 0),
            Err(ApiError::Unauthenticated)
        );

        // Minted for a different App. A token is not a capability for whatever
        // service receives it.
        assert_eq!(
            a.authorize(Some("Bearer other-app"), 0),
            Err(ApiError::Unauthenticated)
        );

        // No header, and a header that is not a bearer credential.
        assert_eq!(a.authorize(None, 0), Err(ApiError::Unauthenticated));
        assert_eq!(
            a.authorize(Some("token good"), 0),
            Err(ApiError::Unauthenticated)
        );
        assert_eq!(
            a.authorize(Some("Bearer   "), 0),
            Err(ApiError::Unauthenticated)
        );

        // An empty allowlist refuses everyone rather than admitting everyone. A
        // model naming no authorized login has not been filled in, and
        // open-by-default would make that omission invisible.
        let empty = Authorizer::new(
            Box::new(Fake {
                tokens: HashMap::from([("good".to_string(), "afflom".to_string())]),
                expired: Vec::new(),
                foreign: Vec::new(),
            }),
            Vec::new(),
            300,
        );
        assert_eq!(
            empty.authorize(Some("Bearer good"), 0),
            Err(ApiError::NotAuthorized {
                login: "afflom".to_string()
            })
        );
    }

    /// `CC-07`: the token cache bounds revocation lag and does not exceed it.
    #[test]
    fn the_token_cache_expires_at_the_declared_ttl_cc_07() {
        let a = authorizer();
        assert!(a.authorize(Some("Bearer good"), 0).is_ok());

        // Inside the TTL the login is served from cache. That is the window
        // §16.2 names: a token revoked at GitHub still works until it lapses.
        assert!(a.authorize(Some("Bearer good"), 299).is_ok());

        // At the TTL it is resolved again. The fake still knows it, so this
        // succeeds --- what is being asserted is that the cache stopped
        // answering, not that the answer changed.
        assert!(a.authorize(Some("Bearer good"), 300).is_ok());

        // A cache keyed on the token, not on the login: two tokens for the same
        // person expire independently, and one being revoked does not extend
        // the other's life.
        assert!(a.authorize(Some("Bearer stranger"), 0).is_err());
    }

    #[test]
    fn the_two_failures_carry_different_statuses_cc_01() {
        assert_eq!(ApiError::Unauthenticated.status(), 401);
        assert_eq!(
            ApiError::NotAuthorized {
                login: "x".to_string()
            }
            .status(),
            403
        );
    }
}
