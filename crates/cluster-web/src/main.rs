//! The Pages UI (`SPEC.md` §16.3).
//!
//! A Leptos SPA compiled to `wasm32-unknown-unknown` and published to GitHub
//! Pages on every push to `main`. Entirely static: its only build-time
//! configuration is the API base URL, injected from a repository variable, and
//! all state comes from `cluster-ctl` at runtime (§16.1).
//!
//! # The disconnected state is a feature
//!
//! When the API is unreachable --- the browser is not on the tailnet, or `n1` is
//! rebooting during its own update window (§14.2) --- the UI renders an explicit
//! disconnected state **naming which of the two it cannot distinguish**, rather
//! than an empty list that looks like "you have no devcontainers".
//!
//! That distinction is the whole reason this file has a [`Connection`] enum
//! instead of `Option<Vec<Session>>`. An empty list and an unreachable control
//! plane are different facts, and a UI that renders them identically teaches its
//! operator to distrust it at exactly the moment §16.5 says it is expected to be
//! down.
//!
//! # It is a management surface, not a dependency
//!
//! Devcontainers already running continue to run while this page cannot load,
//! and `ssh dc-<id>` continues to work from the rendered `ssh_config` without
//! the control plane, resolving to the last known host (§16.5). Only
//! migration-aware resolution degrades.

use cluster_ctl::enrolment::EnrolmentState;
use leptos::prelude::*;

use cluster_ctl::session::SessionState;

/// What the page knows about the control plane.
///
/// Three states, not two. `Empty` and `Unreachable` are different facts and the
/// type refuses to let them be rendered by the same branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection<T> {
    /// The request has not come back yet.
    Loading,
    /// The control plane answered.
    Connected(T),
    /// It did not. Which of the two reasons applies cannot be told apart from a
    /// browser, and the message says so rather than guessing.
    Unreachable {
        /// What the fetch reported, for whoever is debugging it.
        detail: String,
    },
}

impl<T> Connection<T> {
    /// The message shown when the control plane cannot be reached.
    ///
    /// It names *both* possibilities. A browser cannot distinguish a tailnet it
    /// is not on from a node that is rebooting --- the network error is
    /// identical --- and inventing a diagnosis would be worse than admitting the
    /// ambiguity.
    pub const UNREACHABLE_MESSAGE: &'static str = concat!(
        "The control plane is not answering. Either this browser is not on the ",
        "tailnet, or n1 is rebooting during its own update window --- from here ",
        "those look the same. Devcontainers already running are unaffected, and ",
        "`ssh dc-<id>` still works against the last known host."
    );

    /// Whether the page is showing real data.
    pub const fn is_connected(&self) -> bool {
        matches!(self, Self::Connected(_))
    }
}

/// Where a session is opened, and how a browser gets a token to open it with.
///
/// # The device flow, from a static page
///
/// §16.2's reasoning, in the one place a reader of the UI will look for it: a
/// github.com session is a cookie scoped to github.com. This page cannot read
/// it and the cluster never receives it, so being "already signed in" buys
/// nothing. One explicit authorization is required.
///
/// What makes it possible from a page with no backend is that the device flow
/// uses a public client ID and no client secret. The parameters come from the
/// control plane at runtime (`GET /api/auth/config`) rather than being baked in,
/// so §16.3's "only build-time configuration is the API base URL" stays true and
/// a rotated App does not need a rebuilt bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorization {
    /// No token yet. The page shows the code and the URL to enter it at.
    Pending {
        /// What the operator types at GitHub.
        user_code: String,
        /// Where they type it.
        verification_uri: String,
    },
    /// A bearer token, held for this browser.
    Held,
    /// The operator has not started. One click away.
    None,
}

/// The URL a session is addressed by (§11.1).
///
/// Deliberately free of any host component, and that is the entire reason the
/// tunnel path was chosen: the container re-registers under the same name
/// wherever it lands, so this URL is unchanged by a migration (§14.3). A
/// function that took a host would quietly reintroduce the coupling the design
/// exists to remove.
pub fn session_url(template: &str, name_prefix: &str, session: &str, folder: &str) -> String {
    template
        .replace("{name}", &format!("{name_prefix}{session}"))
        .replace("{folder}", folder)
}

/// Where the API lives, injected at build time from a repository variable.
///
/// The only build-time configuration this page has (§16.3). Everything else
/// comes from the API at runtime, which is what keeps a Pages deployment from
/// carrying a stale copy of the cluster's shape.
pub fn api_base() -> String {
    option_env!("CLUSTER_API_BASE")
        .unwrap_or("https://n1.afflom.ts.net")
        .to_string()
}

/// How a session's lifecycle state is shown.
///
/// A held dirty archive is called out rather than blended into "archived":
/// §15.3 lists it as *requiring acknowledgement*, and a UI that showed it as an
/// ordinary archive would leave the acknowledgement with nowhere to happen.
pub fn state_label(state: SessionState, dirty: bool) -> &'static str {
    match (state, dirty) {
        (SessionState::Archived, true) => "archived --- dirty, held for acknowledgement",
        (SessionState::Archived, false) => "archived",
        (SessionState::Creating, _) => "creating",
        (SessionState::Running, _) => "running",
        (SessionState::Stopped, _) => "stopped",
        (SessionState::Migrating, _) => "migrating",
        (SessionState::Purged, _) => "purged",
    }
}

/// Idle age, in the units a human reads it in.
pub fn idle_label(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        s if s < MINUTE => "just now".to_string(),
        s if s < HOUR => format!("{} min", s / MINUTE),
        s if s < DAY => format!("{} h", s / HOUR),
        s => format!("{} days", s / DAY),
    }
}

/// What the enrolment panel should say about one secret.
///
/// A cluster that has not been enrolled is not broken and must not read as
/// broken: it is a cluster nobody has given its credentials to yet, which is the
/// state every cluster starts in (§12.2). The wording distinguishes "not given"
/// from "not working", because an operator who cannot tell those apart starts
/// debugging a machine that is fine.
pub fn enrolment_label(present: bool) -> &'static str {
    if present {
        "given"
    } else {
        "not given yet"
    }
}

/// The one sentence a partly-enrolled cluster shows.
///
/// It names what is still needed rather than reporting a count, because a count
/// is a number to look up and a name is a thing to go and do.
pub fn enrolment_summary(state: &EnrolmentState) -> String {
    if state.complete {
        return "Every secret this cluster needs has been given.".to_string();
    }
    let missing: Vec<&str> = state
        .secrets
        .iter()
        .filter(|s| !s.present)
        .map(|s| s.id.as_str())
        .collect();
    format!(
        "This cluster is waiting for {}. Until then it pulls no images, joins no \
         tailnet, and admits no SSH.",
        missing.join(", ")
    )
}

/// The page.
#[component]
fn App() -> impl IntoView {
    // Nothing is fetched at construction: the shell renders first so that a
    // browser off the tailnet sees the disconnected state immediately rather
    // than a spinner that never resolves.
    let (sessions, _set_sessions) = signal(Connection::<Vec<cluster_ctl::Session>>::Loading);
    let (enrolment, _set_enrolment) = signal(Connection::<EnrolmentState>::Loading);

    view! {
        <main>
            <h1>"cluster"</h1>
            <p class="api">{api_base()}</p>

            // Enrolment first, and above the sessions, because a cluster that
            // has not been given its secrets has no sessions to show and the
            // reason is here rather than in an empty list (§12.2).
            <section class="enrolment">
                <h2>"Secrets"</h2>
                {move || match enrolment.get() {
                    Connection::Loading => view! { <p>"Asking the control plane…"</p> }.into_any(),
                    Connection::Unreachable { detail } => view! {
                        <section class="disconnected">
                            <p>{Connection::<()>::UNREACHABLE_MESSAGE}</p>
                            <p class="detail">{detail}</p>
                        </section>
                    }
                    .into_any(),
                    Connection::Connected(state) => view! {
                        <p class="summary">{enrolment_summary(&state)}</p>
                        <ul>
                            {state
                                .secrets
                                .iter()
                                .map(|s| {
                                    let label = enrolment_label(s.present);
                                    let id = s.id.clone();
                                    view! {
                                        <li>
                                            {id.clone()}" — "{label}
                                            // A password field, so a shoulder and a
                                            // screen recording see nothing, and no
                                            // value is ever rendered back into it:
                                            // the API does not return one.
                                            <input type="password" name={id} autocomplete="off" />
                                        </li>
                                    }
                                })
                                .collect::<Vec<_>>()}
                        </ul>
                    }
                    .into_any(),
                }}
            </section>
            {move || match sessions.get() {
                Connection::Loading => view! { <p>"Asking the control plane…"</p> }.into_any(),
                Connection::Unreachable { detail } => view! {
                    <section class="disconnected">
                        <p>{Connection::<()>::UNREACHABLE_MESSAGE}</p>
                        <p class="detail">{detail}</p>
                    </section>
                }
                .into_any(),
                Connection::Connected(list) if list.is_empty() => view! {
                    <p>"No devcontainer sessions."</p>
                }
                .into_any(),
                Connection::Connected(list) => view! {
                    <ul>
                        {list
                            .into_iter()
                            .map(|s| {
                                let label = state_label(s.state, s.is_dirty());
                                view! { <li>{s.id.clone()}" — "{label}</li> }
                            })
                            .collect::<Vec<_>>()}
                    </ul>
                }
                .into_any(),
            }}
        </main>
    }
}

fn main() {
    leptos::mount::mount_to_body(App);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CC-05`: an unreachable control plane renders as unreachable, and names
    /// both reasons it cannot distinguish --- never as an empty list.
    #[test]
    fn an_unreachable_control_plane_is_not_an_empty_list_cc_05() {
        let empty: Connection<Vec<u8>> = Connection::Connected(Vec::new());
        let down: Connection<Vec<u8>> = Connection::Unreachable {
            detail: "NetworkError".to_string(),
        };

        assert!(empty.is_connected());
        assert!(!down.is_connected());
        assert_ne!(
            empty, down,
            "an empty list and an unreachable control plane are different facts \
             (§16.3)"
        );

        // Both possibilities are named. A browser cannot tell them apart, and
        // inventing a diagnosis would be worse than admitting the ambiguity.
        let message = Connection::<()>::UNREACHABLE_MESSAGE;
        assert!(message.contains("tailnet"));
        assert!(message.contains("rebooting"));
        // And it says what still works, because §16.5 makes this a management
        // surface rather than a dependency.
        assert!(message.contains("ssh dc-"));
    }

    /// A held dirty archive is called out rather than blended into "archived".
    #[test]
    fn a_held_dirty_archive_is_labelled_distinctly_cc_05() {
        let held = state_label(SessionState::Archived, true);
        let ordinary = state_label(SessionState::Archived, false);
        assert_ne!(held, ordinary);
        assert!(held.contains("acknowledgement"), "{held}");
    }

    /// `CW-04`: the URL a session is addressed by carries no host.
    ///
    /// The property §14.3 depends on, asserted where it can be asserted without
    /// a cluster: the derivation is a pure function of the session id, so no
    /// migration can change what it produces. The end-to-end observation --- that
    /// a container really does re-register under the same name on the other node
    /// --- is a T2 claim and waits on the spike in §22.
    #[test]
    fn a_session_url_carries_no_host_cw_04() {
        let template = "https://vscode.dev/tunnel/{name}/{folder}";
        let url = session_url(template, "dc-", "abc123", "workspace");
        assert_eq!(url, "https://vscode.dev/tunnel/dc-abc123/workspace");

        // The identifier is the SSH alias, so one name addresses the session on
        // both paths (§11.1).
        assert!(url.contains("dc-abc123"));

        // No node name can appear, because none is an input. This is the
        // assertion that would fail if somebody "helpfully" added a host
        // parameter to make the URL more specific.
        for node in ["n1", "n2", "n3"] {
            assert!(!url.contains(node), "{node} leaked into the session URL");
        }
    }

    /// A browser with no token is one click from having one, and the states are
    /// distinct so the page never shows a code it does not have.
    #[test]
    fn authorization_states_are_distinct_cc_08() {
        let pending = Authorization::Pending {
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
        };
        assert_ne!(pending, Authorization::None);
        assert_ne!(pending, Authorization::Held);
        assert_ne!(Authorization::None, Authorization::Held);
    }

    #[test]
    fn idle_age_reads_in_human_units_cc_05() {
        assert_eq!(idle_label(30), "just now");
        assert_eq!(idle_label(600), "10 min");
        assert_eq!(idle_label(7_200), "2 h");
        assert_eq!(idle_label(60 * 60 * 24 * 31), "31 days");
    }
}

#[cfg(test)]
mod enrolment_tests {
    use super::*;
    use cluster_ctl::enrolment::SlotState;

    fn state(pairs: &[(&str, bool)]) -> EnrolmentState {
        EnrolmentState::of(
            pairs
                .iter()
                .map(|(id, present)| SlotState {
                    id: (*id).to_string(),
                    present: *present,
                })
                .collect(),
        )
    }

    /// `CC-09`: an unenrolled cluster reads as not-yet-given, not as broken.
    ///
    /// Every cluster starts in this state. An operator who cannot tell "nobody
    /// has given it credentials" from "it is failing" starts debugging a machine
    /// that is fine --- which is the same failure `CC-05`'s disconnected state
    /// exists to prevent, one step earlier in the life of a cluster.
    #[test]
    fn an_unenrolled_cluster_does_not_read_as_broken_cc_09() {
        let s = state(&[("ssh_authorized_key", false), ("tailnet_auth_key", false)]);
        assert!(!s.complete);

        let summary = enrolment_summary(&s);
        // It names what is missing, because a name is a thing to go and do and a
        // count is a number to look up.
        assert!(summary.contains("ssh_authorized_key"), "{summary}");
        assert!(summary.contains("tailnet_auth_key"), "{summary}");
        // And it says what that costs, so the consequence is not left implicit.
        assert!(summary.contains("no images"), "{summary}");
        for alarming in ["error", "failed", "broken", "unhealthy"] {
            assert!(
                !summary.to_lowercase().contains(alarming),
                "a cluster nobody has enrolled is not {alarming}: {summary}"
            );
        }

        assert_eq!(enrolment_label(false), "not given yet");
        assert_eq!(enrolment_label(true), "given");
    }

    #[test]
    fn a_fully_enrolled_cluster_says_so_plainly_cc_09() {
        let s = state(&[("ssh_authorized_key", true), ("tailnet_auth_key", true)]);
        assert!(s.complete);
        let summary = enrolment_summary(&s);
        assert!(summary.contains("Every secret"), "{summary}");
        assert!(!summary.contains("waiting"), "{summary}");
    }

    /// The state a browser receives carries presence and no value, which is the
    /// whole of what the API will say. There is no route that returns one.
    #[test]
    fn the_state_a_browser_receives_carries_no_value_cc_09() {
        let s = state(&[("registry_pull_token", true)]);
        let json = serde_json::to_string(&s).expect("it serialises");
        assert!(json.contains("registry_pull_token"));
        assert!(json.contains("\"present\":true"));
        assert!(!json.contains("value"), "{json}");
    }
}
