//! Resolving a bearer token against GitHub (`SPEC.md` §16.2).
//!
//! One call, `GET /user`, whose answer is a login. Everything interesting about
//! the design is in [`crate::auth`]; this is the part that touches the network,
//! and it is separated for the reason the health predicate's probing is
//! separated from its predicate: an authorization path whose only test needs
//! api.github.com to be reachable is one that gets tested rarely.
//!
//! Requires outbound WAN from the control plane's node. §16.2 names that
//! dependency rather than leaving it to be discovered when GitHub is down and
//! the UI is the thing that would have explained why.

use crate::auth::Resolver;
use crate::ApiError;

/// GitHub, asked who a token belongs to.
#[derive(Debug, Clone)]
pub struct GitHub {
    /// Where to ask. From the model, so a GitHub Enterprise host is a model
    /// change rather than a recompile.
    pub user_url: String,
    /// How long to wait. A control plane blocking on a slow validation would
    /// make an unreachable GitHub look like an unreachable node, and §16.3's
    /// disconnected state already cannot distinguish enough things.
    pub timeout_s: u64,
}

impl Resolver for GitHub {
    fn resolve(&self, token: &str) -> Result<String, ApiError> {
        let output = std::process::Command::new("curl")
            .args([
                "--silent",
                "--fail",
                "--max-time",
                &self.timeout_s.to_string(),
                "--header",
                &format!("authorization: Bearer {token}"),
                "--header",
                "accept: application/vnd.github+json",
                "--header",
                "x-github-api-version: 2022-11-28",
                &self.user_url,
            ])
            .output()
            .map_err(|_| ApiError::Unauthenticated)?;

        // Every rejection GitHub can give --- expired, revoked, minted for a
        // different App, malformed --- arrives as a non-2xx and is one thing
        // from here: this token does not identify anybody. Distinguishing them
        // would mean telling an unauthenticated caller which of its guesses was
        // closer.
        if !output.status.success() {
            return Err(ApiError::Unauthenticated);
        }

        let body = String::from_utf8_lossy(&output.stdout);
        login_from(&body).ok_or(ApiError::Unauthenticated)
    }
}

/// The `login` field of a GitHub user document.
///
/// Scanned rather than parsed into a struct: one field of one document does not
/// justify modelling GitHub's user schema, and a schema this repository declared
/// would be a second source for something GitHub owns.
pub fn login_from(body: &str) -> Option<String> {
    let key = "\"login\"";
    let at = body.find(key)?;
    let rest = &body[at + key.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let open = after.find('"')?;
    let value = &after[open + 1..];
    let close = value.find('"')?;
    let login = &value[..close];
    (!login.is_empty()).then(|| login.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_is_read_from_the_user_document_cc_01() {
        assert_eq!(
            login_from(r#"{"login":"afflom","id":1,"type":"User"}"#).as_deref(),
            Some("afflom")
        );
        // Whitespace and field order are GitHub's to choose.
        assert_eq!(
            login_from("{ \"id\": 1, \"login\" : \"afflom\" }").as_deref(),
            Some("afflom")
        );
        // A document with no login identifies nobody, which is a rejection and
        // not an empty string somebody might compare against an allowlist.
        assert_eq!(login_from("{}"), None);
        assert_eq!(login_from(r#"{"login":""}"#), None);
    }
}
