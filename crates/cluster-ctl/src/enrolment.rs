//! The secrets an operator gives a booted cluster (`SPEC.md` §12.2).
//!
//! A node installs with no credentials. It has the control plane, reachable over
//! the LAN, and nothing else: no SSH key, no registry token, no tailnet. The
//! operator opens it in a browser, authenticates with the GitHub App device flow
//! --- the one credential that can be checked without any of the others existing
//! --- and enters the rest.
//!
//! # Why not the ISO
//!
//! These were `@@PLACEHOLDER@@` names in the kickstart, substituted at ISO build
//! time from Actions secrets. Two things were wrong with that. The Actions
//! secrets did not exist, so a node would have installed the literal string
//! `@@AUTHORIZED_KEY@@` as root's authorized key --- locked out, on a headless
//! machine --- and then died at `tailscale up --erroronfail`. And an ISO is a
//! release artifact: a secret substituted into one is published to whoever
//! downloads it, and this repository is public (§9.1).
//!
//! # What is here and what is not
//!
//! Where each value goes is a model fact, rendered into `enrolment.conf` and
//! parsed here. What a value *is* appears in no model file, no image, no
//! rendered artifact and no log line --- including this crate's own errors,
//! which name the secret and never quote it.
//!
//! Reading back is not offered. The API says which secrets are set and which are
//! missing; it will not return one. An operator who has lost a token issues a
//! new one, and a control plane that would hand a credential back is one bearer
//! token away from handing it to somebody else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ApiError;

/// One secret's destination, as the model declares it (§12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Stable identifier, used by the API and the form.
    pub id: String,
    /// Where the value lands. Empty when applying it consumes the value.
    pub path: String,
    /// The mode the file takes.
    pub mode: u32,
    /// What happens after it is written.
    pub apply: Apply,
    /// How the entered value becomes the file's bytes.
    pub format: Format,
}

/// How an entered value becomes the file at its destination (§12.2).
///
/// Separate from [`Slot::path`], and the separation is the point. An operator
/// enters a credential; most destinations want exactly that credential and one
/// wants a document built around it. Writing the entered string verbatim into
/// every destination is what made the registry token useless: podman parses
/// `/etc/containers/auth.json` as JSON, so a bare token there is a parse error
/// on every pull --- unattended, at the next update, a long way from the
/// browser form where it was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    /// The value is the file.
    Raw,
    /// The value is a password in a containers-auth document keyed by this
    /// registry. The username is the login the device flow authenticated.
    DockerAuth {
        /// The registry host the document is keyed by, port included.
        registry: String,
    },
}

impl Format {
    /// Parse the rendered field: `raw`, or `docker-auth@<registry>`.
    ///
    /// The registry rides behind an `@` rather than in a field of its own
    /// because a registry may carry a port, and the row is colon-separated.
    fn parse(field: &str, line: usize) -> Result<Self, ApiError> {
        let (name, registry) = field.split_once('@').unwrap_or((field, ""));
        match (name, registry) {
            ("raw", "") => Ok(Self::Raw),
            ("docker-auth", "") => Err(ApiError::EnrolmentUnavailable {
                because: format!(
                    "line {line}: a docker-auth document is keyed by registry and this \
                     row names none"
                ),
            }),
            ("docker-auth", registry) => Ok(Self::DockerAuth {
                registry: registry.to_string(),
            }),
            (other, _) => Err(ApiError::EnrolmentUnavailable {
                because: format!(
                    "line {line}: `{other}` is not a format this control plane can \
                     materialise"
                ),
            }),
        }
    }

    /// The bytes that land at the destination.
    ///
    /// `login` is who the device flow authenticated. For a registry document
    /// that is exactly the username the pair wants: the operator entering a
    /// GHCR token is authenticated as the account it belongs to (§16.2).
    ///
    /// The document is built by a serialiser, never by `format!`. A password
    /// carrying a quote or a backslash would otherwise produce a file podman
    /// cannot parse, and the operator would be told the value was accepted.
    pub fn materialise(&self, value: &str, login: &str) -> Result<String, ApiError> {
        match self {
            Self::Raw => Ok(format!("{value}\n")),
            Self::DockerAuth { registry } => {
                let document = serde_json::json!({
                    "auths": {
                        registry: { "auth": base64(format!("{login}:{value}").as_bytes()) }
                    }
                });
                serde_json::to_string_pretty(&document)
                    .map(|mut s| {
                        s.push('\n');
                        s
                    })
                    // Serialising a map of strings cannot fail, but an
                    // `expect` here would be a panic in a request handler on
                    // the one node that has a control plane.
                    .map_err(|e| ApiError::EnrolmentUnavailable {
                        because: format!("building the registry credential document: {e}"),
                    })
            }
        }
    }
}

/// Standard base64, as a containers-auth document wants it.
///
/// Written here rather than taken as a dependency: it is twenty lines with an
/// exact oracle, and R6 makes every arriving crate something to justify. The
/// test checks it against RFC 4648's own vectors.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            // One padding character per byte the final chunk was short.
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char);
            }
        }
    }
    out
}

/// What applying a secret does beyond writing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// Nothing. The file is the whole of it.
    None,
    /// Redeem a Tailscale auth key by joining the tailnet.
    ///
    /// The key is spent by this and deliberately not kept: a redeemed key is of
    /// no further use to this node and of some use to whoever finds it.
    TailscaleUp,
}

impl Slot {
    /// Whether the value is written to a file at all.
    pub fn is_stored(&self) -> bool {
        !self.path.is_empty()
    }
}

/// Every slot the model declares, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct Enrolment {
    slots: Vec<Slot>,
}

/// What the API reports about one secret.
///
/// Deliberately not the value. `present` is the whole of what a caller learns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlotState {
    /// The identifier the form posts back.
    pub id: String,
    /// Whether this cluster has been given it.
    pub present: bool,
}

/// A value being enrolled.
#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    /// The value. Never logged, never returned, never echoed in an error.
    pub value: String,
}

impl Enrolment {
    /// Parse the rendered enrolment policy.
    ///
    /// `secret=id:path:mode:apply:format`, with `#` comments --- the same shape
    /// as every other rendered policy in this repository, for the same reason: a
    /// destination in both the model and a binary is two sources for it.
    ///
    /// The format field is split off last, so a registry carrying a port keeps
    /// its colon.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        let mut slots: Vec<Slot> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            let Some(row) = line.strip_prefix("secret=") else {
                continue;
            };
            let parts: Vec<&str> = row.splitn(5, ':').collect();
            let [id, path, mode, apply, format] = parts.as_slice() else {
                return Err(ApiError::EnrolmentUnavailable {
                    because: format!(
                        "line {}: `{row}` needs five colon-separated fields, \
                         id:path:mode:apply:format",
                        number + 1
                    ),
                });
            };
            let mode = u32::from_str_radix(mode.trim_start_matches("0o"), 8).map_err(|_| {
                ApiError::EnrolmentUnavailable {
                    because: format!("line {}: mode `{mode}` is not octal", number + 1),
                }
            })?;
            let apply = match *apply {
                "none" => Apply::None,
                "tailscale-up" => Apply::TailscaleUp,
                other => {
                    return Err(ApiError::EnrolmentUnavailable {
                        because: format!(
                            "line {}: `{other}` is not an action this control plane knows",
                            number + 1
                        ),
                    })
                }
            };
            let format = Format::parse(format, number + 1)?;

            // A destination that is not absolute would be resolved against
            // whatever directory the unit happened to start in, which is not a
            // place anybody declared.
            if !path.is_empty() && !path.starts_with('/') {
                return Err(ApiError::EnrolmentUnavailable {
                    because: format!(
                        "line {}: `{path}` is not an absolute destination",
                        number + 1
                    ),
                });
            }
            // Two rows with one identifier is a form where the second entry
            // silently does nothing, or overwrites the first --- and which
            // depends on an ordering nobody chose.
            if slots.iter().any(|s| s.id == *id) {
                return Err(ApiError::EnrolmentUnavailable {
                    because: format!("line {}: `{id}` is declared twice", number + 1),
                });
            }

            slots.push(Slot {
                id: (*id).to_string(),
                path: (*path).to_string(),
                mode,
                apply,
                format,
            });
        }
        if slots.is_empty() {
            return Err(ApiError::EnrolmentUnavailable {
                because: "the rendered policy declares no secrets, so nothing can be enrolled"
                    .to_string(),
            });
        }
        Ok(Self { slots })
    }

    /// The slot with an identifier, or a refusal naming what is known.
    ///
    /// A refusal rather than a silent miss: a form posting an identifier this
    /// cluster does not have is a form built against a different model, and
    /// writing the value somewhere plausible would be worse than not writing it.
    pub fn slot(&self, id: &str) -> Result<&Slot, ApiError> {
        self.slots
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| ApiError::UnknownSecret {
                id: id.to_string(),
                known: self.slots.iter().map(|s| s.id.clone()).collect(),
            })
    }

    /// Every slot, and whether this cluster has been given it.
    ///
    /// Presence is decided by the file being there and non-empty. A secret whose
    /// applying consumes it --- a Tailscale key --- is reported by a marker
    /// instead, because the thing it produced is the tailnet membership and the
    /// key itself is deliberately gone.
    pub fn state(&self, root: &Path) -> Vec<SlotState> {
        self.slots
            .iter()
            .map(|slot| SlotState {
                id: slot.id.clone(),
                present: if slot.is_stored() {
                    std::fs::metadata(&slot.path).is_ok_and(|m| m.len() > 0)
                } else {
                    root.join(format!("applied.{}", slot.id)).exists()
                },
            })
            .collect()
    }

    /// Every declared identifier, in order.
    pub fn ids(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.id.clone()).collect()
    }
}

/// Reject a value that cannot be a credential before writing it anywhere.
///
/// Only the shape, and only what is universal: a value must not be empty and
/// must not carry a newline. The second matters more than it looks --- every
/// destination here is a line-oriented file, and a value with an embedded
/// newline appends a line to `authorized_keys` or `auth.json` that nobody
/// entered.
///
/// It deliberately does not try to recognise a *valid* token. A control plane
/// that guessed at GitHub's token format would reject a new one the day GitHub
/// changed it, and the failure would look like a broken cluster.
pub fn check_value(id: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::RejectedSecret {
            id: id.to_string(),
            because: "it is empty".to_string(),
        });
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(ApiError::RejectedSecret {
            id: id.to_string(),
            because: "it contains a newline, and every destination here is line-oriented: \
                      the extra line would be an entry nobody typed"
                .to_string(),
        });
    }
    Ok(())
}

/// Where a written secret's marker goes, so that a spent one can still be
/// reported as given.
pub fn marker_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("applied.{id}"))
}

/// The environment a `tailscale up` needs, kept apart from the running of it so
/// the decision is testable without a tailnet.
pub fn tailscale_arguments(value: &str, advertise_routes: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "up".to_string(),
        "--auth-key".to_string(),
        value.to_string(),
        "--advertise-tags=tag:cluster".to_string(),
    ];
    if let Some(prefix) = advertise_routes {
        // Only the storage node advertises the management subnet, and the mesh
        // is never advertised (§4.5).
        args.push(format!("--advertise-routes={prefix}"));
    }
    args
}

/// What the API reports for the whole cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolmentState {
    /// One row per declared secret.
    pub secrets: Vec<SlotState>,
    /// Whether every secret has been given.
    pub complete: bool,
}

impl EnrolmentState {
    /// Summarise, so a form can show one line as well as a list.
    pub fn of(secrets: Vec<SlotState>) -> Self {
        let complete = secrets.iter().all(|s| s.present);
        Self { secrets, complete }
    }
}

/// A map of identifier to value, used only in tests and by the form's decoder.
pub type Values = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = "\
# a comment
secret=ssh_authorized_key:/etc/ssh/authorized_keys.d/root:0644:none:raw
secret=registry_pull_token:/etc/containers/auth.json:0600:none:docker-auth@ghcr.io
secret=tailnet_auth_key::0600:tailscale-up:raw
";

    #[test]
    fn the_rendered_policy_parses() {
        let e = Enrolment::parse(POLICY).expect("it parses");
        assert_eq!(
            e.ids(),
            [
                "ssh_authorized_key",
                "registry_pull_token",
                "tailnet_auth_key"
            ]
        );
        assert_eq!(e.slot("registry_pull_token").unwrap().mode, 0o600);
        assert_eq!(
            e.slot("tailnet_auth_key").unwrap().apply,
            Apply::TailscaleUp
        );
    }

    /// A key that is spent by applying it is not stored, and that is the point
    /// rather than an omission.
    #[test]
    fn a_spent_secret_has_no_destination() {
        let e = Enrolment::parse(POLICY).expect("it parses");
        assert!(!e.slot("tailnet_auth_key").unwrap().is_stored());
        assert!(e.slot("ssh_authorized_key").unwrap().is_stored());
    }

    /// An identifier this cluster does not declare is refused, and the refusal
    /// says what is known. Writing the value somewhere plausible would be worse
    /// than not writing it.
    #[test]
    fn an_unknown_identifier_is_refused_and_names_what_is_known() {
        let e = Enrolment::parse(POLICY).expect("it parses");
        let err = e.slot("root_password").expect_err("it is not declared");
        let text = format!("{err}");
        assert!(text.contains("root_password"));
        assert!(
            text.contains("ssh_authorized_key"),
            "it lists what is: {text}"
        );
    }

    #[test]
    fn a_policy_declaring_nothing_is_refused() {
        assert!(Enrolment::parse("# nothing here\n").is_err());
        assert!(Enrolment::parse("secret=a:b:c\n").is_err(), "five fields");
        assert!(
            Enrolment::parse("secret=a:/b:0600:none\n").is_err(),
            "the format field is not optional"
        );
        assert!(
            Enrolment::parse("secret=a:/b:9999:none:raw\n").is_err(),
            "octal"
        );
        assert!(
            Enrolment::parse("secret=a:/b:0600:reboot:raw\n").is_err(),
            "action"
        );
        assert!(
            Enrolment::parse("secret=a:/b:0600:none:pkcs12\n").is_err(),
            "a format this control plane cannot materialise"
        );
        assert!(
            Enrolment::parse("secret=a:/b:0600:none:docker-auth\n").is_err(),
            "a docker-auth document with no registry is keyed by nothing"
        );
        assert!(
            Enrolment::parse("secret=a:b:0600:none:raw\n").is_err(),
            "a destination resolved against whatever directory the unit started in"
        );
        assert!(
            Enrolment::parse("secret=a:/b:0600:none:raw\nsecret=a:/c:0600:none:raw\n").is_err(),
            "one identifier declared twice is a form whose second entry silently \
             does nothing"
        );
    }

    /// A registry may carry a port, and the row is colon-separated. The format
    /// field is split off last so the port survives.
    #[test]
    fn a_registry_may_carry_a_port() {
        let e = Enrolment::parse("secret=t:/a:0600:none:docker-auth@registry.local:5000\n")
            .expect("it parses");
        assert_eq!(
            e.slot("t").unwrap().format,
            Format::DockerAuth {
                registry: "registry.local:5000".to_string()
            }
        );
    }

    /// RFC 4648 §10's own vectors. The encoder is written here rather than
    /// taken as a dependency, so it is checked against the authority rather
    /// than against itself.
    #[test]
    fn base64_matches_rfc_4648() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected, "base64({input:?})");
        }
        // Every byte value, so the alphabet's high indices are exercised too.
        let all: Vec<u8> = (0u8..=255).collect();
        let encoded = base64(&all);
        assert_eq!(encoded.len(), 344, "256 bytes is 344 base64 characters");
        assert!(encoded.ends_with('='), "256 is not a multiple of three");
    }

    /// `CD-21`: what lands at the destination is the format applied to the
    /// value, never the value verbatim.
    ///
    /// The registry token used to be written raw into a file podman parses as
    /// JSON. Every pull failed, unattended, at the next update --- and the
    /// operator had been told the value was accepted.
    #[test]
    fn a_registry_credential_is_a_document_podman_can_parse_cd_21() {
        let e = Enrolment::parse(POLICY).expect("it parses");
        let slot = e.slot("registry_pull_token").expect("declared");

        let written = slot
            .format
            .materialise("ghp_tokenvalue", "afflom")
            .expect("it materialises");

        let parsed: serde_json::Value = serde_json::from_str(&written)
            .expect("podman parses this file as JSON, so it has to be JSON");
        let auth = parsed["auths"]["ghcr.io"]["auth"]
            .as_str()
            .expect("keyed by the registry the model declares");
        // base64("afflom:ghp_tokenvalue"), which is the pair ghcr.io wants.
        assert_eq!(auth, base64(b"afflom:ghp_tokenvalue"));
        assert!(
            !written.contains("ghp_tokenvalue"),
            "the token appears only inside the encoded pair: {written}"
        );

        // A raw slot is unchanged by any of this: the value is the file.
        let key = e.slot("ssh_authorized_key").expect("declared");
        assert_eq!(
            key.format
                .materialise("ssh-ed25519 AAAA", "afflom")
                .unwrap(),
            "ssh-ed25519 AAAA\n"
        );
    }

    /// The document is built by a serialiser, never by `format!`. A password
    /// carrying a quote would otherwise produce a file podman cannot parse,
    /// and the operator would be told the value was accepted.
    #[test]
    fn a_value_carrying_json_punctuation_still_produces_valid_json() {
        let format = Format::DockerAuth {
            registry: "ghcr.io".to_string(),
        };
        let awkward = r#"tok"en\with}punctuation"#;
        let written = format
            .materialise(awkward, r#"lo"gin"#)
            .expect("materialises");
        let parsed: serde_json::Value =
            serde_json::from_str(&written).expect("still JSON: {written}");
        assert_eq!(
            parsed["auths"]["ghcr.io"]["auth"].as_str().unwrap(),
            base64(format!("lo\"gin:{awkward}").as_bytes())
        );
    }

    /// The check is about shape and refuses the two things that are universally
    /// wrong. A newline is the one that matters: every destination is
    /// line-oriented, so an embedded one appends an entry nobody typed.
    #[test]
    fn an_empty_or_multiline_value_is_refused() {
        assert!(check_value("k", "").is_err());
        assert!(check_value("k", "   ").is_err());
        let err = check_value("k", "ssh-ed25519 AAAA\nssh-ed25519 BBBB").expect_err("two lines");
        assert!(format!("{err}").contains("newline"));
        assert!(check_value("k", "ssh-ed25519 AAAA").is_ok());
    }

    /// It does not try to recognise a *valid* token. A control plane that
    /// guessed at GitHub's format would reject a new one the day GitHub changed
    /// it, and the failure would look like a broken cluster.
    #[test]
    fn a_value_of_an_unfamiliar_shape_is_accepted() {
        assert!(check_value("registry_pull_token", "something-new-2031").is_ok());
    }

    /// No error this module produces may quote the value it refused. An error
    /// goes to a log, and a log is read by more people than a credential should
    /// be.
    #[test]
    fn a_refusal_never_quotes_the_value() {
        let value = "ghp_thisisasecretvaluethatmustnotappear";
        let err =
            check_value("registry_pull_token", &format!("{value}\nsecond")).expect_err("two lines");
        assert!(
            !format!("{err}").contains(value),
            "an error must name the secret and never quote it: {err}"
        );
    }

    #[test]
    fn tailscale_advertises_the_subnet_only_when_asked() {
        let with = tailscale_arguments("k", Some("192.168.20.0/24"));
        assert!(with
            .iter()
            .any(|a| a == "--advertise-routes=192.168.20.0/24"));
        let without = tailscale_arguments("k", None);
        assert!(!without.iter().any(|a| a.starts_with("--advertise-routes")));
        // The mesh is never advertised, whichever branch is taken (§4.5).
        for args in [&with, &without] {
            assert!(!args.iter().any(|a| a.contains("10.10.")));
            assert!(args.iter().any(|a| a == "--advertise-tags=tag:cluster"));
        }
    }

    #[test]
    fn state_reports_presence_and_never_a_value() {
        let dir = std::env::temp_dir().join(format!("enrol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let e = Enrolment::parse(POLICY).expect("it parses");
        let state = EnrolmentState::of(e.state(&dir));
        assert_eq!(state.secrets.len(), 3);
        assert!(!state.complete, "nothing has been enrolled");
        let json = serde_json::to_string(&state).expect("it serialises");
        assert!(json.contains("\"present\""));
        assert!(!json.contains("value"), "no value may appear: {json}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod api_contract {
    use super::*;

    const POLICY: &str = "\
secret=ssh_authorized_key:/etc/ssh/authorized_keys.d/root:0644:none:raw
secret=tailnet_auth_key::0600:tailscale-up:raw
";

    /// `CC-09`: the refusals a caller can provoke, and what each one says.
    ///
    /// Both are about a caller getting something wrong, and both matter more
    /// than they look. An identifier this cluster does not declare means a form
    /// built against a different model --- writing the value somewhere plausible
    /// would be worse than refusing. A value with a newline appends an entry to
    /// a line-oriented file that nobody typed, which for `authorized_keys` is a
    /// second key.
    #[test]
    fn the_refusals_name_the_problem_and_never_the_value_cc_09() {
        let e = Enrolment::parse(POLICY).expect("it parses");

        let unknown = e.slot("root_password").expect_err("not declared");
        let text = format!("{unknown}");
        assert!(text.contains("root_password"), "{text}");
        assert!(
            text.contains("ssh_authorized_key"),
            "it says what is: {text}"
        );
        assert_eq!(unknown.status(), 404);

        let secret = "ghp_averyrealisticlookingtokenvalue";
        let rejected =
            check_value("ssh_authorized_key", &format!("{secret}\nextra")).expect_err("two lines");
        assert_eq!(rejected.status(), 400);
        assert!(
            !format!("{rejected}").contains(secret),
            "a refusal goes to a log, and a log is read by more people than a credential \
             should be: {rejected}"
        );
    }

    /// A policy this control plane cannot read is a cluster that cannot be
    /// enrolled, which is a different thing to fix from a database that will not
    /// open --- so it is a different condition with its own status.
    #[test]
    fn an_unreadable_policy_is_its_own_condition_cc_09() {
        let err = Enrolment::parse("# nothing\n").expect_err("declares nothing");
        assert_eq!(err.status(), 503);
        assert!(format!("{err}").contains("cannot be enrolled"));
    }
}
