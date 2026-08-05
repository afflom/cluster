//! The HTTP surface, driven as a service (`SPEC.md` §16.1, §16.2, §16.3).
//!
//! # Why this file exists
//!
//! `api.rs` declares sixteen routes, an authorization gate, a cross-origin
//! layer and an asset server, in eight hundred lines. Two tests covered it, and
//! one of those built the response struct *by hand inside the test* and
//! asserted over its own construction --- so the handler it was named for could
//! have returned anything at all and it would still have passed.
//!
//! Nothing had ever driven a route. Not the authorization gate, not the exact
//! origin §16.3 requires, not the traversal refusal, not one lifecycle
//! transition, not enrolment. Every one of those is a decision the model makes
//! and the router is where they are actually joined: a handler that is correct
//! and unreachable, or reachable without its authorization layer, is a defect
//! no unit test of the handler can see.
//!
//! So this assembles the real [`Router`] --- middleware, extractors, routing
//! table and state --- and sends requests through it. The store is in memory
//! and the identity provider is a fake, which is what makes each refusal
//! deterministic; everything between the request and the response is the
//! shipping code.
//!
//! # What is not covered here, and why
//!
//! Three handlers shell out to a node that does not exist in a test: `drain`
//! and the dirty observation call `curl`, and `enrol`'s Tailscale step calls
//! `tailscale`. Their *decisions* are tested --- `plan` in `cluster-updater`,
//! `read_dirty` and `tailscale_arguments` here in unit tests --- and T2 drives
//! the rest inside guests. This file says so rather than reaching for a mock
//! that would prove only that the mock was called.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use cluster_ctl::api::{Api, AuthConfig, NodeEndpoint};
use cluster_ctl::auth::{Authorizer, Resolver};
use cluster_ctl::enrolment::Enrolment;
use cluster_ctl::reclaim::Thresholds;
use cluster_ctl::session::{Session, SessionState};
use cluster_ctl::{ApiError, Store};
use cluster_updater::drain::Capacity;
use tower::ServiceExt;

/// The origin the model declares. Exact, never a wildcard (§16.3).
const ORIGIN: &str = "https://afflom.github.io";

/// A resolver standing in for GitHub, so every refusal is deterministic.
struct Fake;

impl Resolver for Fake {
    fn resolve(&self, token: &str) -> Result<String, ApiError> {
        match token {
            "good" => Ok("afflom".to_string()),
            "stranger" => Ok("someone-else".to_string()),
            _ => Err(ApiError::Unauthenticated),
        }
    }
}

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(what: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cluster-ctl-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Self(dir)
    }

    fn join(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The assembled service, its store, and the scratch it writes into.
struct Harness {
    api: Api,
    scratch: Scratch,
}

impl Harness {
    fn new(what: &str) -> Self {
        let scratch = Scratch::new(what);
        // Destinations under the scratch, so enrolment writes somewhere a test
        // may look. The *shapes* are the model's: one raw file, one document.
        let policy = format!(
            "secret=ssh_authorized_key:{}:0600:none:raw\n\
             secret=registry_pull_token:{}:0600:none:docker-auth@ghcr.io\n\
             secret=tailnet_auth_key::0600:tailscale-up:raw\n",
            scratch.join("authorized_keys"),
            scratch.join("auth.json"),
        );
        let api = Api {
            store: Arc::new(Mutex::new(Store::in_memory().expect("an in-memory store"))),
            authorizer: Arc::new(Authorizer::new(
                Box::new(Fake),
                vec!["afflom".to_string()],
                300,
            )),
            thresholds: Thresholds {
                notify_after_days: 14,
                archive_after_days: 30,
                purge_after_days: 90,
            },
            nodes: vec![NodeEndpoint {
                name: "node1".to_string(),
                position: 3,
                health_url: "http://10.10.255.1:9101/health".to_string(),
            }],
            capacity: Capacity {
                target: "storage".to_string(),
                never_receives: vec!["testbed".to_string()],
                memory_cap_gib: 12,
            },
            allowed_origin: ORIGIN.to_string(),
            web_root: scratch.join("web"),
            auth_config: AuthConfig {
                client_id: "Iv23liCLUSTERafflom00".to_string(),
                scopes: vec!["read:user".to_string()],
                device_code_url: "https://github.com/login/device/code".to_string(),
                token_url: "https://github.com/login/oauth/access_token".to_string(),
            },
            enrolment: Enrolment::parse(&policy).expect("the policy parses"),
            enrolment_root: scratch.join("enrolment"),
            advertise_routes: Some("192.168.20.0/24".to_string()),
        };
        Self { api, scratch }
    }

    /// Send one request through the assembled router.
    async fn send(&self, request: Request<Body>) -> (StatusCode, Vec<(String, String)>, String) {
        let response = cluster_ctl::api::router(self.api.clone())
            .oneshot(request)
            .await
            .expect("the router is infallible");
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a bounded body");
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn get(&self, path: &str) -> (StatusCode, String) {
        let (status, _, body) = self
            .send(
                Request::builder()
                    .uri(path)
                    .header("authorization", "Bearer good")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        (status, body)
    }

    async fn post(&self, path: &str, json: &str) -> (StatusCode, String) {
        let (status, _, body) = self
            .send(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header("authorization", "Bearer good")
                    .header("content-type", "application/json")
                    .body(Body::from(json.to_string()))
                    .expect("a request"),
            )
            .await;
        (status, body)
    }

    fn put(&self, session: &Session) {
        self.api
            .store
            .lock()
            .expect("the store lock")
            .put(session)
            .expect("the store accepts it");
    }
}

fn session(id: &str, state: SessionState) -> Session {
    Session::new(
        id,
        "afflom",
        "afflom/cluster",
        "main",
        ".devcontainer/devcontainer.json",
        "sha256:aaaa",
        "node2",
        state,
        0,
        0,
        4,
        false,
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

/// `CC-01`: every route that drives the cluster is behind the allowlist, and
/// the two refusals are distinct.
///
/// Asserted over the *router*, not over `Authorizer`. A handler that forgot to
/// call `caller` is exactly the defect a unit test of the authorizer cannot
/// see, and it is one line of omission.
#[test]
fn every_driving_route_is_behind_the_allowlist_cc_01() {
    runtime().block_on(async {
        let h = Harness::new("auth");
        h.put(&session("abc123", SessionState::Running));

        // Every route that reads or changes cluster state.
        let guarded = [
            (Method::GET, "/api/nodes", ""),
            (Method::GET, "/api/sessions", ""),
            (Method::GET, "/api/sessions/abc123/connect", ""),
            (Method::GET, "/api/rollout", ""),
            (Method::GET, "/api/enrolment", ""),
            (Method::DELETE, "/api/sessions/abc123", ""),
            (Method::POST, "/api/sessions/abc123/start", ""),
            (
                Method::POST,
                "/api/sessions",
                r#"{"id":"new1","repo":"a/b","git_ref":"main","config_path":"c","host":"node2","memory_gib":4}"#,
            ),
            (
                Method::POST,
                "/api/enrolment/ssh_authorized_key",
                r#"{"value":"ssh-ed25519 AAAA"}"#,
            ),
        ];

        for (method, path, body) in guarded {
            // No credential at all.
            let (status, _, _) = h
                .send(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .expect("a request"),
                )
                .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} answered {status} with no credential"
            );

            // Authenticated by GitHub, not on the model's list. A different
            // condition from having no identity, and a different thing to do
            // about it (§16.2).
            let (status, _, body_out) = h
                .send(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("authorization", "Bearer stranger")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .expect("a request"),
                )
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path} answered {status} for an unlisted login"
            );
            assert!(
                body_out.contains("someone-else"),
                "the refusal names the login, so the operator can add it: {body_out}"
            );
        }
    });
}

/// `CC-08`: a browser with no token can learn how to obtain one, and only that.
#[test]
fn the_device_flow_parameters_are_served_unauthenticated_cc_08() {
    runtime().block_on(async {
        let h = Harness::new("authconfig");
        let (status, _, body) = h
            .send(
                Request::builder()
                    .uri("/api/auth/config")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        // An endpoint that required a token to learn how to obtain a token
        // would be a circle.
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(parsed["client_id"], "Iv23liCLUSTERafflom00");
        assert_eq!(parsed["scopes"][0], "read:user");
        // Every field of it is public by design; nothing else is.
        assert!(
            !body.contains("secret"),
            "the device flow uses a public client id and no client secret: {body}"
        );
    });
}

/// `CC-08`: cross-origin access names one exact origin, and a preflight is
/// answered without reaching a handler.
///
/// A management API that answered `*` would be usable by any page the
/// operator's browser had loaded, with the token that browser already holds.
#[test]
fn cross_origin_names_one_exact_origin_cc_08() {
    runtime().block_on(async {
        let h = Harness::new("cors");

        let (status, headers, _) = h
            .send(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/sessions")
                    .header("origin", "https://elsewhere.example")
                    .header("access-control-request-method", "POST")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;

        // A preflight is a question about the route, not a request to it, so it
        // is answered by the layer and never reaches the authorization gate.
        assert_eq!(status, StatusCode::NO_CONTENT);

        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{name} is set: {headers:?}"))
        };
        assert_eq!(header("access-control-allow-origin"), ORIGIN);
        assert_ne!(header("access-control-allow-origin"), "*");
        assert_eq!(header("vary"), "origin");
        assert!(header("access-control-allow-headers").contains("authorization"));

        // And the same on an ordinary response, not only on the preflight.
        let (_, headers, _) = h
            .send(
                Request::builder()
                    .uri("/api/auth/config")
                    .header("origin", "https://elsewhere.example")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        assert!(headers
            .iter()
            .any(|(k, v)| k == "access-control-allow-origin" && v == ORIGIN));
    });
}

/// `CC-02`: the mirrored bundle serves what is under its root and nothing above
/// it.
///
/// A management surface that served `../../etc/shadow` because somebody asked
/// politely is not a management surface.
#[test]
fn the_mirror_serves_nothing_above_its_root_cc_02() {
    runtime().block_on(async {
        let h = Harness::new("mirror");
        let web = std::path::Path::new(&h.api.web_root);
        std::fs::create_dir_all(web).expect("the web root");
        std::fs::write(web.join("index.html"), "<!doctype html>ok").expect("an index");
        std::fs::write(h.scratch.0.join("above.txt"), "not yours").expect("a file above it");

        let (status, _, body) = h
            .send(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ok"));

        for escape in [
            "/../above.txt",
            "/..%2fabove.txt",
            "/web/../../above.txt",
            "/%2e%2e/above.txt",
        ] {
            let (status, _, body) = h
                .send(
                    Request::builder()
                        .uri(escape)
                        .body(Body::empty())
                        .expect("a request"),
                )
                .await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{escape} answered {status} with {body}"
            );
            assert!(
                !body.contains("not yours"),
                "{escape} served a file above the root"
            );
        }

        // A mirror that was never pushed is a degraded route, not a broken
        // node: §16.3 makes Pages canonical and this the path that always works.
        let (status, _, _) = h
            .send(
                Request::builder()
                    .uri("/never-pushed.js")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    });
}

/// `CC-04`: the lifecycle answers over the wire, and `connect` reports the host
/// the session is on *now*.
///
/// The previous test for this built the response struct by hand and asserted
/// over its own construction. This drives the route.
#[test]
fn the_lifecycle_transitions_answer_over_the_wire_cc_04() {
    runtime().block_on(async {
        let h = Harness::new("lifecycle");
        h.put(&session("abc123", SessionState::Stopped));

        let (status, body) = h.post("/api/sessions/abc123/start", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).expect("JSON")["state"],
            "running"
        );

        let (status, body) = h.get("/api/sessions/abc123/connect").await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        // The alias does not carry the host: that is what makes it survive a
        // migration (§11.1, §14.3).
        assert_eq!(parsed["ssh"], "ssh dc-abc123");
        assert_eq!(parsed["host"], "node2");
        assert!(!parsed["ssh"].as_str().unwrap().contains("node2"));

        // Move it, and the same alias reports the new host.
        h.put(&session("abc123", SessionState::Running).with_host("storage"));
        let (_, body) = h.get("/api/sessions/abc123/connect").await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(parsed["ssh"], "ssh dc-abc123", "the alias is unchanged");
        assert_eq!(parsed["host"], "storage", "the host it resolves to is not");

        // An action the lifecycle does not name is a refusal, not a silent
        // no-op that returns the session unchanged.
        let (status, _) = h.post("/api/sessions/abc123/reticulate", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // And a session that does not exist.
        let (status, _) = h.get("/api/sessions/no-such/connect").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    });
}

/// `CG-02`, `CG-03`: only an archive is restorable, and a dirty archive is
/// never purged.
///
/// §15.3's ninety-day step is not reversible, and an endpoint that pretended
/// otherwise would be the one place the policy lies.
#[test]
fn the_destructive_steps_refuse_what_the_policy_forbids_cg_03() {
    runtime().block_on(async {
        let h = Harness::new("destructive");

        // Restore is for archives, and for nothing else.
        h.put(&session("running1", SessionState::Running));
        let (status, body) = h.post("/api/sessions/running1/restore", "").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("archived"), "it says why: {body}");

        h.put(&session("arch1", SessionState::Archived));
        let (status, body) = h.post("/api/sessions/arch1/restore", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).expect("JSON")["state"],
            "stopped",
            "a restored archive comes back stopped, not running"
        );

        // Delete archives first, and purges second --- and the dirty flag is
        // recomputed at the destructive step, never read from the record. The
        // observation cannot reach a node here, and an unobservable workspace
        // is dirty, so the purge is refused. That is the safe reading and the
        // one §15.3 requires.
        h.put(&session("held1", SessionState::Running));
        let (status, body) = h.post_delete("/api/sessions/held1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).expect("JSON")["state"],
            "archived"
        );

        let (status, body) = h.post_delete("/api/sessions/held1").await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "an archive whose workspace cannot be observed is held: {body}"
        );
        assert!(
            body.contains("betrayal") || body.contains("dirty"),
            "it says why: {body}"
        );

        // And it is still there afterwards.
        let (_, body) = h.get("/api/sessions").await;
        assert!(body.contains("held1"), "nothing was deleted: {body}");
    });
}

/// `CC-10`: an identifier the consumers cannot carry never reaches the store.
///
/// It becomes a directory under the workspace root, a URL path segment, a
/// container name and the `dc-` SSH alias. The only check used to be that
/// nothing else had claimed it.
#[test]
fn an_identifier_no_consumer_can_carry_is_refused_cc_10() {
    runtime().block_on(async {
        let h = Harness::new("ids");

        for bad in [
            "../../etc",
            "a/b",
            "a b",
            "UPPER",
            "x\",\"dirty\":false,\"y\":\"",
            "",
        ] {
            let json = serde_json::json!({
                "id": bad,
                "repo": "a/b",
                "git_ref": "main",
                "config_path": "c",
                "host": "node2",
                "memory_gib": 4,
            })
            .to_string();
            let (status, body) = h.post("/api/sessions", &json).await;
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "`{bad}` was accepted as a session identifier: {body}"
            );
        }

        // And a reasonable one is created.
        let json = serde_json::json!({
            "id": "my-project-2",
            "repo": "afflom/cluster",
            "git_ref": "main",
            "config_path": ".devcontainer/devcontainer.json",
            "host": "node2",
            "memory_gib": 4,
        })
        .to_string();
        let (status, body) = h.post("/api/sessions", &json).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(parsed["id"], "my-project-2");
        // The owner is the authenticated caller, not anything the body said.
        assert_eq!(parsed["owner"], "afflom");
        assert_eq!(parsed["state"], "creating");

        // Twice is a refusal: the id is the SSH alias, so it cannot be reused.
        let (status, _) = h.post("/api/sessions", &json).await;
        assert_eq!(status, StatusCode::CONFLICT);
    });
}

/// `CC-09`, `CD-21`: enrolment reports presence, never a value --- and what it
/// writes is the format applied to the value.
#[test]
fn enrolment_reports_presence_and_writes_the_declared_shape_cc_09() {
    runtime().block_on(async {
        let h = Harness::new("enrol");

        // Nothing given yet, and the report says so without alarm.
        let (status, body) = h.get("/api/enrolment").await;
        assert_eq!(status, StatusCode::OK);
        let state: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(state["complete"], false);
        assert_eq!(state["secrets"].as_array().expect("a list").len(), 3);
        assert!(
            state["secrets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["present"] == false),
            "{body}"
        );

        // Give it the key. A raw slot is written verbatim.
        let (status, body) = h
            .post(
                "/api/enrolment/ssh_authorized_key",
                r#"{"value":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 operator@laptop"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            std::fs::read_to_string(h.scratch.join("authorized_keys")).expect("it was written"),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 operator@laptop\n"
        );

        // And it now reports as given --- presence, and nothing else.
        let (_, body) = h.get("/api/enrolment").await;
        let state: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        let key = state["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "ssh_authorized_key")
            .expect("declared");
        assert_eq!(key["present"], true);
        assert!(
            !body.contains("AAAAC3"),
            "no route returns a value, and this is the one that reports them: {body}"
        );
        assert_eq!(state["complete"], false, "two are still missing");

        // The registry token becomes a document podman can parse, keyed by the
        // declared registry, with the authenticated login as the username.
        let (status, body) = h
            .post(
                "/api/enrolment/registry_pull_token",
                r#"{"value":"ghp_atokenvalue"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let written = std::fs::read_to_string(h.scratch.join("auth.json")).expect("it was written");
        let document: serde_json::Value =
            serde_json::from_str(&written).expect("podman parses this as JSON");
        assert!(
            document["auths"]["ghcr.io"]["auth"].is_string(),
            "{written}"
        );
        assert!(
            !written.contains("ghp_atokenvalue"),
            "the token appears only inside the encoded pair: {written}"
        );

        // Mode, at creation. A credential created world-readable and narrowed a
        // moment later is world-readable for the width of that window (`CI-07`).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(h.scratch.join("auth.json"))
            .expect("it exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the declared mode, not the umask's");

        // An identifier this cluster does not declare is refused, naming what it
        // does. A form posting one was built against a different model, and
        // writing the value somewhere plausible would be worse.
        let (status, body) = h
            .post("/api/enrolment/root_password", r#"{"value":"hunter2"}"#)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body.contains("ssh_authorized_key"),
            "it says what is: {body}"
        );
        assert!(
            !body.contains("hunter2"),
            "a refusal never quotes it: {body}"
        );

        // A value that would append a line nobody typed.
        let (status, body) = h
            .post(
                "/api/enrolment/ssh_authorized_key",
                r#"{"value":"ssh-ed25519 AAAA\nssh-ed25519 BBBB"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("newline"), "{body}");
        assert!(!body.contains("BBBB"), "a refusal never quotes it: {body}");

        // And the first key is still the one on disk: a refused submission
        // changes nothing.
        assert_eq!(
            std::fs::read_to_string(h.scratch.join("authorized_keys")).expect("still there"),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 operator@laptop\n"
        );
    });
}

/// `CC-03`: a node quarantines a digest without an identity, because there is
/// none in that path.
///
/// Called by a node's greenboot check from the mesh, in the seconds before a
/// rollback reboot. §4.4 makes the mesh a closed segment, which is the same
/// trust §5.4 already extends to NFS.
#[test]
fn a_node_quarantines_without_an_identity_cc_03() {
    runtime().block_on(async {
        let h = Harness::new("quarantine");

        let (status, _, body) = h
            .send(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/rollout/quarantine")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"digest":"sha256:bad","node":"node2"}"#.to_string(),
                    ))
                    .expect("a request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // And it is visible to an operator who *is* authenticated.
        let (status, body) = h.get("/api/rollout").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("sha256:bad"), "{body}");
        assert!(body.contains("node2"), "{body}");
    });
}

/// `CG-05`: recording an attachment needs no identity either, and a session
/// somebody is using stops looking idle.
///
/// It arrives from a node's agent over the mesh on every SSH connection. A
/// session in active use that looked idle would be archived out from under
/// whoever was using it.
#[test]
fn an_attachment_is_recorded_without_an_identity_cg_05() {
    runtime().block_on(async {
        let h = Harness::new("attached");
        h.put(&session("abc123", SessionState::Running));

        let (status, _, body) = h
            .send(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/abc123/attached")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let recorded: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        let at = recorded["last_attached_at"].as_u64().expect("a timestamp");
        assert!(at > 0, "the attachment is stamped with now, not left at 0");

        // And the listing agrees, because it was persisted rather than returned.
        let (_, body) = h.get("/api/sessions").await;
        let sessions: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(sessions[0]["last_attached_at"].as_u64(), Some(at));
        assert_eq!(
            sessions[0]["idle_seconds"].as_u64(),
            Some(0),
            "a session attached to right now is not idle"
        );
    });
}

/// `CC-05`: the listing says what reclamation would do, so the operator does
/// not have to work it out.
#[test]
fn the_listing_says_what_reclamation_would_do_cc_05() {
    runtime().block_on(async {
        let h = Harness::new("pending");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_secs();

        // Idle past the archive threshold but short of the purge one.
        let mut old = session("stale1", SessionState::Running);
        old = Session::new(
            &old.id,
            &old.owner,
            &old.repo,
            &old.git_ref,
            &old.config_path,
            &old.image_digest,
            &old.host,
            old.state,
            now - 60 * 86_400,
            now - 45 * 86_400,
            old.memory_gib,
            false,
        );
        h.put(&old);

        let (status, body) = h.get("/api/sessions").await;
        assert_eq!(status, StatusCode::OK);
        let sessions: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        let view = &sessions[0];
        assert_eq!(view["id"], "stale1");
        assert!(
            view["idle_seconds"].as_u64().expect("a number") >= 45 * 86_400,
            "{body}"
        );
        assert!(
            !view["pending_action"]
                .as_str()
                .expect("a decision")
                .is_empty(),
            "the UI says `archived in four days` rather than making the operator \
             work it out: {body}"
        );
    });
}

impl Harness {
    /// `DELETE`, which archives and then purges.
    async fn post_delete(&self, path: &str) -> (StatusCode, String) {
        let (status, _, body) = self
            .send(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(path)
                    .header("authorization", "Bearer good")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await;
        (status, body)
    }
}

/// The server binds and answers over a real socket.
///
/// Everything above drives the `Router` as a service, which is the shipping
/// code from the request onward but stops short of the listener. This is the
/// last inch: `serve` binds, accepts, and answers an HTTP request written onto
/// a TCP stream --- the path a browser on the LAN actually takes to reach a
/// cluster that has not been enrolled yet (§12.2, §16.1).
#[test]
fn the_control_plane_answers_on_a_socket() {
    runtime().block_on(async {
        let h = Harness::new("socket");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let addr = listener.local_addr().expect("a bound address");
        let router = cluster_ctl::api::router(h.api.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("the server is listening");
        stream
            .write_all(
                format!(
                    "GET /api/auth/config HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("the request is written");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("the server answers");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(
            text.contains("Iv23liCLUSTERafflom00"),
            "a browser learns how to authenticate over the wire: {text}"
        );
        server.abort();
    });
}
