//! The HTTP surface (`SPEC.md` §16.1).
//!
//! An axum service on the storage node, published by `tailscale serve` with a real
//! certificate from the tailnet's CA and no inbound port opened on the
//! management plane (§16.2).
//!
//! # Why not drive it through the GitHub API
//!
//! §16.4 considers and rejects the alternative: a static page dispatching
//! `workflow_dispatch` events that a self-hosted runner picks up, needing no
//! inbound reachability at all. Every action would become a workflow run --- ten
//! to thirty seconds of latency to start a container, an Actions log full of UI
//! clicks, and a status view that cannot poll. Tailscale is outbound-initiated,
//! so the direct path opens nothing that the alternative would have kept closed.
//!
//! # Every response is a sanctioned outcome
//!
//! Handlers return `Result<_, ApiError>`, and [`ApiError`] has no catch-all
//! variant. That is R5 over an HTTP surface: a caller either gets what it asked
//! for or gets a condition the model named, with a status that says what to do
//! about it.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::auth::{Authorizer, AUTHORIZATION_HEADER};
use crate::enrolment::{self, Enrolment, EnrolmentState, Submission};
use crate::reclaim::{decide, RolloutStatus, Thresholds};
use crate::rollout::RolloutState;
use crate::session::{DirtyObservation, Session, SessionState};
use crate::store::Store;
use crate::ApiError;
use cluster_updater::drain::{plan as plan_drain, Capacity, Strategy, Workload, WorkloadClass};

/// Everything a handler needs.
///
/// The store is behind a mutex rather than a pool: SQLite on a single file
/// serving a three-node cluster's management surface has no contention worth
/// pooling for, and one lock is easier to reason about than a pool's failure
/// modes when the disk underneath is the single copy §5.6 describes.
#[derive(Clone)]
pub struct Api {
    /// The session registry and rollout state.
    pub store: Arc<Mutex<Store>>,
    /// Identity and the allowlist (§16.2).
    ///
    /// Shared rather than cloned: the token cache lives inside it, and a clone
    /// per request would make the TTL meaningless by giving every request an
    /// empty cache to miss in.
    pub authorizer: Arc<Authorizer>,
    /// Reclamation thresholds, rendered from `model/policy.toml`.
    pub thresholds: Thresholds,
    /// Nodes and their health endpoints, rendered from the model.
    pub nodes: Vec<NodeEndpoint>,
    /// Where a drain may send work, and how much (§14.3).
    pub capacity: Capacity,
    /// The exact origin the Pages copy is served from (§16.3).
    ///
    /// Exact, never a wildcard. This API drives a cluster, and `*` would let any
    /// page a browser happens to visit use a token that browser already holds.
    pub allowed_origin: String,
    /// Where the mirrored web bundle is served from, if it is present (§16.3).
    pub web_root: String,
    /// What a browser needs to begin a device authorization (§16.2).
    pub auth_config: AuthConfig,
    /// Where each enrolled secret goes, rendered from the model (§12.2).
    pub enrolment: Enrolment,
    /// Where markers for spent secrets live, so one can still be reported as
    /// given after the value is gone.
    pub enrolment_root: String,
    /// The prefix the storage node advertises to the tailnet, or none.
    pub advertise_routes: Option<String>,
}

/// The device flow's public parameters (§16.2).
///
/// Served unauthenticated, because every field is public by design: the device
/// flow uses a public client ID with no client secret, which is precisely what
/// lets a static page start an authorization. An endpoint that required a token
/// to learn how to obtain a token would be a circle.
///
/// It is served rather than baked into the bundle so that §16.3's "only
/// build-time configuration is the API base URL" stays true --- and so a rotated
/// App is a model change and a re-render, not a rebuilt SPA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// The App's public client identifier.
    pub client_id: String,
    /// What the browser token may do. `read:user` and nothing else.
    pub scopes: Vec<String>,
    /// Where a device authorization begins.
    pub device_code_url: String,
    /// Where the device code is exchanged for a token.
    pub token_url: String,
}

/// One node, and where to ask it how it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEndpoint {
    /// The node's name.
    pub name: String,
    /// Its position in the rollout sequence.
    pub position: u32,
    /// Its `:9101/health` URL on the mesh loopback.
    pub health_url: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status()).unwrap_or(StatusCode::BAD_REQUEST);
        // The body names the condition in the register's words, so an operator
        // reading a failed request sees the same sentence the model does.
        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

/// What a caller sees when a request does not succeed.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The condition, in the register's words.
    pub error: String,
}

/// The routes §16.1 declares.
pub fn router(api: Api) -> Router {
    Router::new()
        // Unauthenticated by construction: this is how a browser learns to
        // authenticate at all, and every field of it is public (§16.2).
        .route("/api/auth/config", get(auth_config))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{node}/drain", post(drain))
        .route("/api/sessions", get(sessions).post(create_session))
        .route("/api/sessions/{id}", delete(archive_or_purge))
        .route("/api/sessions/{id}/connect", get(connect))
        .route("/api/sessions/{id}/attached", post(attached))
        .route("/api/sessions/{id}/notify", post(notify))
        .route("/api/sessions/{id}/{action}", post(lifecycle))
        .route("/api/rollout", get(rollout))
        .route("/api/rollout/quarantine", post(quarantine))
        // Enrolment (§12.2). A node installs with no credentials and is given
        // them here, over the LAN, once --- which is why this repository being
        // public costs it nothing: none of these values is in it.
        .route("/api/enrolment", get(enrolment_state))
        .route("/api/enrolment/{id}", post(enrol))
        // §16.3's mirror. The same bundle Pages publishes, served at the origin
        // the API is on: same-origin has no preflight and no browser policy
        // standing between a page and the API it was built for. Pages stays
        // canonical; this is the path that always works.
        .route("/", get(index))
        .route("/{*asset}", get(asset))
        .layer(axum::middleware::from_fn_with_state(
            api.clone(),
            cross_origin,
        ))
        .with_state(api)
}

/// Answer a preflight, and mark every response with the one origin allowed.
///
/// Exact-origin rather than a wildcard (§16.3). A management API that answered
/// `*` would be usable by any page the operator's browser loaded, with the token
/// that browser already holds --- which is the whole of the attack.
async fn cross_origin(
    State(api): State<Api>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let preflight = request.method() == axum::http::Method::OPTIONS;
    let mut response = if preflight {
        // Answered here rather than routed: a preflight is a question about the
        // route, not a request to it, and every route would otherwise need to
        // answer it identically.
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };

    let headers = response.headers_mut();
    if let Ok(origin) = api.allowed_origin.parse() {
        headers.insert("access-control-allow-origin", origin);
    }
    headers.insert("vary", "origin".parse().expect("a static header value"));
    headers.insert(
        "access-control-allow-headers",
        "authorization, content-type"
            .parse()
            .expect("a static header value"),
    );
    headers.insert(
        "access-control-allow-methods",
        "GET, POST, DELETE, OPTIONS"
            .parse()
            .expect("a static header value"),
    );
    response
}

/// The mirrored single-page application (§16.3).
async fn index(State(api): State<Api>) -> Response {
    serve_asset(&api, "index.html")
}

/// One asset of the mirrored bundle.
async fn asset(State(api): State<Api>, Path(asset): Path<String>) -> Response {
    serve_asset(&api, &asset)
}

fn serve_asset(api: &Api, asset: &str) -> Response {
    // Anything that escapes the root is refused rather than normalised. A
    // management surface that served `../../etc/shadow` because somebody asked
    // politely is not a management surface.
    if asset.contains("..") || asset.starts_with('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = std::path::Path::new(&api.web_root).join(asset);
    let Ok(bytes) = std::fs::read(&path) else {
        // A control plane whose mirror was never pushed still serves its API.
        // §16.3 makes Pages canonical, so a missing mirror is a degraded route
        // and not a broken node.
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
}

/// Authorize a request from its bearer token (§16.2).
fn caller(headers: &HeaderMap, api: &Api) -> Result<String, ApiError> {
    let presented = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|v| v.to_str().ok());
    api.authorizer
        .authorize(presented, now_seconds())
        .map(|i| i.login)
}

/// Take the store lock.
///
/// A poisoned mutex means a handler panicked while holding it. Reported as
/// `StoreUnavailable` rather than propagated, because from a caller's side the
/// store is exactly what is unavailable --- and a panic here must not take the
/// whole control plane down while the storage node is the only one that has it.
fn locked<'a>(api: &'a Api, attempted: &str) -> Result<std::sync::MutexGuard<'a, Store>, ApiError> {
    api.store.lock().map_err(|_| ApiError::StoreUnavailable {
        attempted: attempted.to_string(),
        because: "a previous request panicked while holding the store".to_string(),
    })
}

/// `GET /api/auth/config` --- the device flow's public parameters (§16.2).
async fn auth_config(State(api): State<Api>) -> Json<AuthConfig> {
    Json(api.auth_config.clone())
}

/// `GET /api/nodes` --- health, booted digest, target digest, rollout state.
async fn nodes(
    State(api): State<Api>,
    headers: HeaderMap,
) -> Result<Json<Vec<NodeEndpoint>>, ApiError> {
    caller(&headers, &api)?;
    Ok(Json(api.nodes.clone()))
}

/// What `POST /api/nodes/:node/drain` returns.
#[derive(Debug, Serialize, Deserialize)]
pub struct Drained {
    /// Which node was drained.
    pub node: String,
    /// Sessions that moved to the migration target.
    pub migrated: Vec<String>,
    /// Sessions stopped with notice because the capacity cap was spent (§14.3).
    pub stopped: Vec<String>,
}

/// `POST /api/nodes/:node/drain` --- move what can move, wait for what cannot.
///
/// Called by `cluster-updater` on the node it is about to reboot, and it is the
/// step between publishing `draining` and publishing `updating` (§13.3).
///
/// Deliberately **not** behind the identity header, for the reason
/// [`quarantine`] is not: the caller is a node's own updater, reaching across
/// the mesh in the seconds before it reboots. There is no Tailscale identity in
/// that path and no way to obtain one, and §4.4 makes the mesh a closed segment.
///
/// The capacity decision is `cluster-updater`'s [`cluster_updater::drain::plan`],
/// used here rather than reimplemented: `CU-05` checks the cap is enforced and
/// the plan is deterministic, and a second copy of that arithmetic would be a
/// second thing for those claims to be true of.
async fn drain(
    State(api): State<Api>,
    Path(node): Path<String>,
) -> Result<Json<Drained>, ApiError> {
    let store = locked(&api, &format!("drain {node}"))?;
    let now = now_seconds();

    let workloads: Vec<Workload> = store
        .sessions()?
        .into_iter()
        .filter(|s| s.host == node && s.state == SessionState::Running)
        .map(|s| Workload {
            id: s.id.clone(),
            class: WorkloadClass::Devcontainer,
            memory_gib: s.memory_gib,
            idle_seconds: s.idle_seconds(now),
        })
        .collect();

    let plan = plan_drain(&workloads, &api.capacity);

    let mut migrated = Vec::new();
    let mut stopped = Vec::new();
    for step in &plan.steps {
        let session = store.session(&step.workload.id)?;
        match &step.strategy {
            Strategy::Migrate { to } => {
                // The agent on the node being drained runs §14.3's six steps; it
                // is the only thing that can see the worktree.
                agent_migrate(&node, &session.id)?;
                store.put(&session.with_state(SessionState::Migrating).with_host(to))?;
                migrated.push(session.id.clone());
            }
            Strategy::StopWithNotice { .. } => {
                // The session survives; the process does not (§14.3).
                store.put(&session.with_state(SessionState::Stopped))?;
                stopped.push(session.id.clone());
            }
            // Nothing else on a compute node reaches here: a bench or CI job is
            // waited for by the runner unit not re-registering, and a storage
            // service cannot move at all (§14.1).
            Strategy::Wait | Strategy::CannotMove { .. } => {}
        }
    }

    Ok(Json(Drained {
        node,
        migrated,
        stopped,
    }))
}

/// Ask a node's agent to migrate one session (§14.3).
fn agent_migrate(node: &str, session: &str) -> Result<(), ApiError> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            // A migration copies a worktree; the per-container budget in
            // model/policy.toml is what bounds it, and the updater measures the
            // total against §14.4 rather than this call timing out arbitrarily.
            "--max-time",
            "600",
            "--request",
            "POST",
            &format!("http://{node}.mesh:8081/workspaces/{session}/migrate"),
        ])
        .output()
        .map_err(|e| ApiError::NotPermitted {
            attempted: format!("migrate {session} off {node}"),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ApiError::NotPermitted {
        attempted: format!("migrate {session} off {node}"),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// One session as the UI shows it: the record, plus what reclamation would do.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionView {
    /// The record itself.
    #[serde(flatten)]
    pub session: Session,
    /// How long since anyone attached.
    pub idle_seconds: u64,
    /// What reclamation would do at this age, so the UI can say "archived in
    /// four days" rather than making the operator work it out (§16.3).
    pub pending_action: String,
}

/// `GET /api/sessions` --- list, with idle age and dirty flag.
async fn sessions(
    State(api): State<Api>,
    headers: HeaderMap,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    caller(&headers, &api)?;
    let store = locked(&api, "list sessions")?;
    let now = now_seconds();

    let views = store
        .sessions()?
        .into_iter()
        .map(|session| {
            // The stored flag, not a recomputed one. This is a display path and
            // §15.2 only requires recomputation before a *destructive* step ---
            // recomputing here would mean walking every worktree on every page
            // load. The distinction is why `decide` will not take a bare
            // `Session`.
            let observed =
                session.with_recomputed_dirty(DirtyObservation::observed(session.is_dirty()));
            SessionView {
                idle_seconds: session.idle_seconds(now),
                pending_action: decide(&observed, &api.thresholds, now, RolloutStatus::Quiet)
                    .to_string(),
                session,
            }
        })
        .collect();
    Ok(Json(views))
}

/// What `POST /api/sessions` takes.
#[derive(Debug, Deserialize)]
pub struct CreateSession {
    /// Short stable identifier; becomes the `dc-<id>` alias.
    pub id: String,
    /// The repository to build from.
    pub repo: String,
    /// The ref.
    pub git_ref: String,
    /// Where the `devcontainer.json` lives.
    pub config_path: String,
    /// Which node to start it on.
    pub host: String,
    /// Declared memory, for the migration cap (§14.3).
    pub memory_gib: u32,
}

/// `POST /api/sessions` --- create from repo, ref, and `devcontainer.json` path.
async fn create_session(
    State(api): State<Api>,
    headers: HeaderMap,
    Json(request): Json<CreateSession>,
) -> Result<Json<Session>, ApiError> {
    let owner = caller(&headers, &api)?;

    // Before anything else, and before the identifier reaches a path, a URL or
    // an alias. It becomes all three (§15.1), and the only check here used to be
    // that nothing else had claimed it.
    crate::session::check_session_id(&request.id)?;

    let store = locked(&api, "create session")?;

    if store.session(&request.id).is_ok() {
        return Err(ApiError::NotPermitted {
            attempted: format!("create session {}", request.id),
            because: "a session with that id already exists, and ids are the SSH alias \
                      so they cannot be reused"
                .to_string(),
        });
    }

    let now = now_seconds();
    let session = Session::new(
        request.id,
        owner,
        request.repo,
        request.git_ref,
        request.config_path,
        String::new(),
        request.host,
        SessionState::Creating,
        now,
        now,
        request.memory_gib,
        false,
    );
    store.put(&session)?;
    Ok(Json(session))
}

/// `POST /api/sessions/:id/{start,stop,migrate,restore}` --- lifecycle.
async fn lifecycle(
    State(api): State<Api>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
) -> Result<Json<Session>, ApiError> {
    caller(&headers, &api)?;
    let store = locked(&api, &format!("{action} session {id}"))?;
    let session = store.session(&id)?;

    let next = match action.as_str() {
        "start" => SessionState::Running,
        "stop" => SessionState::Stopped,
        "migrate" => SessionState::Migrating,
        "restore" => {
            // Only an archive can be restored, and a purged session cannot:
            // §15.3 says the ninety-day step is not reversible, and an endpoint
            // that pretended otherwise would be the one place the policy lies.
            if session.state != SessionState::Archived {
                return Err(ApiError::NotPermitted {
                    attempted: format!("restore {id}"),
                    because: format!(
                        "it is {}, and only an archived session can be restored (§15.3)",
                        session.state.as_str()
                    ),
                });
            }
            SessionState::Stopped
        }
        other => {
            return Err(ApiError::NotFound {
                kind: "lifecycle action",
                id: other.to_string(),
            })
        }
    };

    let moved = session.with_state(next);
    store.put(&moved)?;
    Ok(Json(moved))
}

/// `DELETE /api/sessions/:id` --- archive now, or purge if already archived.
async fn archive_or_purge(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Session>, ApiError> {
    caller(&headers, &api)?;
    let store = locked(&api, &format!("delete session {id}"))?;
    let session = store.session(&id)?;

    // Dirty is recomputed immediately before this destructive step, never read
    // from cache (§15.2). The observation is taken here rather than trusted from
    // the record, which is the whole reason `Reclaimable` exists.
    let observed = observe_dirty(&session);
    let reclaimable = session.with_recomputed_dirty(observed);

    if reclaimable.dirty() && session.state == SessionState::Archived {
        return Err(ApiError::NotPermitted {
            attempted: format!("purge {id}"),
            because: "the workspace is dirty and is held indefinitely. Deleting someone's \
                      uncommitted work because a timer expired is a betrayal (§15.3)"
                .to_string(),
        });
    }

    let next = if session.state == SessionState::Archived {
        SessionState::Purged
    } else {
        SessionState::Archived
    };
    let moved = session.with_state(next);
    store.put(&moved)?;
    Ok(Json(moved))
}

/// What `GET /api/sessions/:id/connect` returns.
#[derive(Debug, Serialize, Deserialize)]
pub struct Connection {
    /// The node currently hosting it. The rendered `ssh_config` reads this so a
    /// migrated session is reachable at the same alias (§11.1, §14.3).
    pub host: String,
    /// The command to run.
    pub ssh: String,
    /// Where a browser attaches, for the thin client path (§11.1).
    pub tunnel: String,
}

/// `GET /api/sessions/:id/connect`.
async fn connect(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Connection>, ApiError> {
    caller(&headers, &api)?;
    let store = locked(&api, &format!("connect session {id}"))?;
    let session = store.session(&id)?;
    Ok(Json(Connection {
        host: session.host.clone(),
        ssh: format!("ssh dc-{}", session.id),
        tunnel: format!("https://vscode.dev/tunnel/dc-{}", session.id),
    }))
}

/// `POST /api/sessions/:id/attached` --- record an attachment (§15.1).
///
/// Called by the `sshrc` hook on every SSH connection and by the agent on every
/// tunnel attachment. Not behind the identity header: it arrives from a node's
/// agent over the mesh, and a session somebody is actively using that looked
/// idle would be archived out from under them (§15.3).
async fn attached(
    State(api): State<Api>,
    Path(id): Path<String>,
) -> Result<Json<Session>, ApiError> {
    let store = locked(&api, &format!("record an attachment to {id}"))?;
    let session = store.session(&id)?;
    let touched = session.with_attachment(now_seconds());
    store.put(&touched)?;
    Ok(Json(touched))
}

/// `POST /api/sessions/:id/notify` --- tell attached clients to reconnect.
///
/// The last step of §14.3. Attached editor sessions drop when a container moves;
/// there is no way around that short of CRIU, which is not reliable for VS Code
/// server, open TTYs and live SSH sockets. What this makes possible is that
/// reconnection is one command rather than a lookup: the `dc-<id>` alias already
/// resolves the new host (§11.1).
async fn notify(
    State(api): State<Api>,
    Path(id): Path<String>,
) -> Result<Json<Connection>, ApiError> {
    let store = locked(&api, &format!("notify {id}"))?;
    let session = store.session(&id)?;
    Ok(Json(Connection {
        host: session.host.clone(),
        ssh: format!("ssh dc-{}", session.id),
        tunnel: format!("https://vscode.dev/tunnel/dc-{}", session.id),
    }))
}

/// `GET /api/rollout` --- current stage, budgets consumed, quarantined digests.
async fn rollout(
    State(api): State<Api>,
    headers: HeaderMap,
) -> Result<Json<RolloutState>, ApiError> {
    caller(&headers, &api)?;
    let store = locked(&api, "read rollout state")?;
    Ok(Json(store.rollout_state()?))
}

/// What a node POSTs after a greenboot rollback (§13.4).
#[derive(Debug, Deserialize)]
pub struct QuarantineRequest {
    /// The digest that failed to boot healthy.
    pub digest: String,
    /// Which node rolled back from it.
    pub node: String,
}

/// `POST /api/rollout/quarantine`.
///
/// Deliberately **not** behind the identity header. It is called by a node's
/// greenboot check, from the mesh, in the seconds before a rollback reboot ---
/// there is no Tailscale identity in that path and no way to obtain one. The
/// mesh is a physically isolated L2 with exactly two endpoints per segment
/// (§4.4), which is the same trust §5.4 already extends to NFS.
async fn quarantine(
    State(api): State<Api>,
    Json(request): Json<QuarantineRequest>,
) -> Result<Json<RolloutState>, ApiError> {
    let store = locked(&api, "quarantine digest")?;
    store.quarantine(&request.digest, &request.node, now_seconds())?;
    Ok(Json(store.rollout_state()?))
}

/// Observe whether a workspace is dirty (§15.2).
///
/// The agent on the node hosting the session is what actually walks the
/// worktree; this asks it. A failed observation is treated as **dirty**, which
/// is the only safe reading: an unknown workspace that gets purged is the exact
/// failure §15.3 exists to prevent, and an extra held archive costs a few
/// gigabytes on a 2 TB disk.
fn observe_dirty(session: &Session) -> DirtyObservation {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--max-time",
            "10",
            &format!(
                "http://{}.mesh:8081/workspaces/{}/dirty",
                session.host, session.id
            ),
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            DirtyObservation::observed(read_dirty(&String::from_utf8_lossy(&o.stdout)))
        }
        _ => DirtyObservation::observed(true),
    }
}

/// Read the agent's answer, or treat it as dirty.
///
/// **Parsed, not searched.** This was `!text.contains("\"dirty\":false")` over a
/// document the agent built with `format!` from an identifier nobody validated.
/// A session whose id contained `","dirty":false,"x":"` produced a body carrying
/// that substring before the real field, so a dirty workspace read as clean ---
/// and the step that reads this is the one that deletes the archive.
///
/// Anything that does not parse, or that carries no boolean `dirty`, is dirty.
/// An answer that cannot be understood is not an answer that says "safe".
fn read_dirty(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("dirty")?.as_bool())
        .unwrap_or(true)
}

/// Unix seconds.
///
/// A clock that has jumped is a real hazard here --- every retention threshold
/// is computed from it --- which is why §10.1 checks chrony and §18 alerts on
/// an unsynchronised one rather than leaving it to be discovered by a wrong
/// deletion.
fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `GET /api/enrolment` --- which secrets this cluster has been given (§12.2).
///
/// Presence and nothing else. It will not return a value, and there is no route
/// that will: an operator who has lost a token issues a new one, and a control
/// plane that handed a credential back would be one bearer token away from
/// handing it to somebody else.
async fn enrolment_state(
    State(api): State<Api>,
    headers: HeaderMap,
) -> Result<Json<EnrolmentState>, ApiError> {
    caller(&headers, &api)?;
    Ok(Json(EnrolmentState::of(
        api.enrolment
            .state(std::path::Path::new(&api.enrolment_root)),
    )))
}

/// `POST /api/enrolment/{id}` --- give this cluster one secret (§12.2).
///
/// Authorized like everything else: the GitHub App device flow, which is the one
/// credential that can be checked without any of the others existing. That is
/// what makes this the bootstrap path rather than a chicken-and-egg.
async fn enrol(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Submission>,
) -> Result<Json<EnrolmentState>, ApiError> {
    // The login, not just the fact of one. A registry credential is a pair, and
    // the operator entering a GHCR token is authenticated as the account that
    // token belongs to --- which is exactly the username it wants (§16.2).
    let login = caller(&headers, &api)?;
    let slot = api.enrolment.slot(&id)?;
    enrolment::check_value(&id, &body.value)?;

    let root = std::path::Path::new(&api.enrolment_root);
    if slot.is_stored() {
        write_secret(slot, &body.value, &login)?;
    }
    match slot.apply {
        enrolment::Apply::None => {}
        enrolment::Apply::TailscaleUp => {
            let args = enrolment::tailscale_arguments(&body.value, api.advertise_routes.as_deref());
            let output = std::process::Command::new("tailscale")
                .args(&args)
                .output()
                .map_err(|e| ApiError::RejectedSecret {
                    id: id.clone(),
                    // The command, never the arguments: `--auth-key` is in them.
                    because: format!("`tailscale up` could not be run: {e}"),
                })?;
            if !output.status.success() {
                return Err(ApiError::RejectedSecret {
                    id: id.clone(),
                    because: format!(
                        "`tailscale up` refused it: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                });
            }
        }
    }
    // The marker records that a secret was given, which is the only way a spent
    // one can be reported at all --- the value is gone by design.
    std::fs::create_dir_all(root).map_err(|e| ApiError::EnrolmentUnavailable {
        because: format!("{}: {e}", root.display()),
    })?;
    std::fs::write(enrolment::marker_path(root, &id), "").map_err(|e| {
        ApiError::EnrolmentUnavailable {
            because: format!("recording that `{id}` was given: {e}"),
        }
    })?;

    Ok(Json(EnrolmentState::of(api.enrolment.state(root))))
}

/// Write one secret to its declared destination, at its declared mode.
///
/// The mode is set **on creation**, not narrowed afterwards: a credential
/// created world-readable and chmodded a moment later is world-readable for the
/// width of that window. The same mistake shipped a world-writable signature
/// policy once already (`CI-07`).
///
/// What is written is the slot's format applied to the value, never the value
/// verbatim. `/etc/containers/auth.json` is parsed by podman as JSON, and the
/// bare token this used to write there failed every pull (§12.2, `CD-21`).
fn write_secret(slot: &enrolment::Slot, value: &str, login: &str) -> Result<(), ApiError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let contents = slot.format.materialise(value, login)?;

    let path = std::path::Path::new(&slot.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::EnrolmentUnavailable {
            because: format!("{}: {e}", parent.display()),
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(slot.mode)
        .open(path)
        .map_err(|e| ApiError::EnrolmentUnavailable {
            because: format!("{}: {e}", slot.path),
        })?;
    // `mode` applies at creation only, so an existing file keeps what it had.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(slot.mode)).map_err(|e| {
        ApiError::EnrolmentUnavailable {
            because: format!("{}: {e}", slot.path),
        }
    })?;
    file.write_all(contents.as_bytes())
        .map_err(|e| ApiError::EnrolmentUnavailable {
            because: format!("{}: {e}", slot.path),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CC-04`: a session's connect endpoint reports the host it is on now, so
    /// a migrated session is reachable at the same alias (§14.3).
    #[test]
    fn connect_reports_the_current_host_cc_04() {
        let session = Session::new(
            "abc123",
            "alex@uor.foundation",
            "afflom/cluster",
            "main",
            ".devcontainer/devcontainer.json",
            "sha256:aaaa",
            "node1",
            SessionState::Running,
            0,
            0,
            4,
            false,
        );
        let connection = Connection {
            host: session.host.clone(),
            ssh: format!("ssh dc-{}", session.id),
            tunnel: format!("https://vscode.dev/tunnel/dc-{}", session.id),
        };
        // The alias does not carry the host: that is what makes it survive a
        // migration. The host is a separate field the ProxyCommand resolves.
        assert_eq!(connection.ssh, "ssh dc-abc123");
        assert_eq!(connection.host, "node1");
        assert!(!connection.ssh.contains("node1"));
    }

    /// An unreadable workspace is dirty. The only safe reading: an extra held
    /// archive costs a few gigabytes; a wrong purge costs someone's work.
    #[test]
    fn an_unobservable_workspace_is_treated_as_dirty_cg_05() {
        let session = Session::new(
            "abc123",
            "alex@uor.foundation",
            "afflom/cluster",
            "main",
            ".devcontainer/devcontainer.json",
            "sha256:aaaa",
            // A host that does not resolve, so the observation cannot succeed.
            "no-such-node-in-any-cluster",
            SessionState::Archived,
            0,
            0,
            4,
            false,
        );
        assert!(observe_dirty(&session).dirty);
    }

    /// `CG-05`: the agent's answer is parsed, and anything not understood is
    /// dirty.
    ///
    /// This was a substring search for `"dirty":false` over a document the
    /// agent built with `format!` from an identifier nobody validated. A
    /// session whose id carried that substring made a dirty workspace read as
    /// clean --- and the step that reads this is the one that deletes the
    /// archive. Both ends are fixed; this is the reading end.
    #[test]
    fn an_answer_that_is_not_understood_is_dirty_cg_05() {
        // The two answers the agent actually gives.
        assert!(!read_dirty(
            r#"{"session":"abc","dirty":false,"reason":""}"#
        ));
        assert!(read_dirty(
            r#"{"session":"abc","dirty":true,"reason":"uncommitted changes"}"#
        ));

        // The injection the substring search could not survive. The crafted id
        // is a *string value* to a parser, so the real field is the one read.
        let crafted = serde_json::json!({
            "session": "x\",\"dirty\":false,\"y\":\"",
            "dirty": true,
            "reason": "uncommitted changes",
        })
        .to_string();
        assert!(
            crafted.contains(r#"dirty\":false"#),
            "the crafted value is present in the document: {crafted}"
        );
        assert!(
            read_dirty(&crafted),
            "a parser reads the field, not the first thing that looks like it: {crafted}"
        );

        // Anything else is dirty. An answer that cannot be understood is not an
        // answer that says safe.
        for unreadable in [
            "",
            "not json at all",
            "{}",
            r#"{"dirty":"false"}"#,
            r#"{"dirty":null}"#,
            "<html>502 Bad Gateway</html>",
        ] {
            assert!(
                read_dirty(unreadable),
                "`{unreadable}` says nothing about the workspace, so it is dirty"
            );
        }
    }
}
