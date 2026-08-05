//! `/usr/bin/cluster-ctl` (`SPEC.md` §16.1).
//!
//! Two modes. `serve` runs the API; `reclaim run` is what the daily timer
//! invokes (§15.3).
//!
//! Configuration comes from the environment the image renders (§7.2) --- the
//! authorized logins, the reclamation thresholds, the node endpoints. Nothing
//! here parses `model/`.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use cluster_ctl::api::{Api, AuthConfig, NodeEndpoint};
use cluster_ctl::auth::Authorizer;
use cluster_ctl::enrolment::Enrolment;
use cluster_ctl::github::GitHub;
use cluster_ctl::reclaim::{decide, Action, RolloutStatus, Thresholds};
use cluster_ctl::session::{DirtyObservation, Session, SessionState};
use cluster_ctl::{ApiError, Store};
use cluster_updater::drain::Capacity;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("serve");

    let result = match mode {
        "serve" => serve(),
        "reclaim" => reclaim(),
        other => {
            eprintln!(
                "cluster-ctl <serve|reclaim run>\n\
                 \n\
                 serve         run the control plane API (§16.1)\n\
                 reclaim run   one pass of devcontainer reclamation (§15.3)\n\
                 \n\
                 unknown mode `{other}`"
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cluster-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve() -> Result<(), ApiError> {
    let api = api_from_environment()?;
    let bind = env_or("CLUSTER_CTL_BIND", "127.0.0.1:8080");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ApiError::StoreUnavailable {
            attempted: "start the async runtime".to_string(),
            because: e.to_string(),
        })?;

    runtime.block_on(async move {
        let listener =
            tokio::net::TcpListener::bind(&bind)
                .await
                .map_err(|e| ApiError::StoreUnavailable {
                    attempted: format!("bind {bind}"),
                    because: e.to_string(),
                })?;
        println!("cluster-ctl: serving on {bind}");
        axum::serve(listener, cluster_ctl::api::router(api))
            .await
            .map_err(|e| ApiError::StoreUnavailable {
                attempted: "serve".to_string(),
                because: e.to_string(),
            })
    })
}

/// One reclamation pass (§15.3).
///
/// Emits one line per session so §18 can alert on unexpected volume: more than
/// five archived in a run is what a policy bug or a clock bug looks like from
/// the outside, and it is cheaper to notice than to reconstruct.
fn reclaim() -> Result<(), ApiError> {
    let api = api_from_environment()?;
    let store = api.store.lock().map_err(|_| ApiError::StoreUnavailable {
        attempted: "reclaim".to_string(),
        because: "the store lock is poisoned".to_string(),
    })?;

    // §15.4: reclamation never runs during a rollout. Read here rather than
    // assumed, because the timer fires on a schedule and a rollout does not.
    //
    // Whether to look at all is a model fact. A deployment that wanted
    // reclamation to run regardless would set it in `model/policy.toml`, and
    // this reads that rather than deciding on its behalf.
    let suspends = env_or("CLUSTER_RECLAIM_SUSPEND_DURING_ROLLOUT", "true") == "true";
    let rollout = if suspends && rollout_in_progress(&api) {
        RolloutStatus::InProgress
    } else {
        RolloutStatus::Quiet
    };

    let now = now_seconds();
    let mut archived = 0usize;
    let mut held = 0usize;

    for session in store.sessions()? {
        // §15.1's primary attachment signal, taken before anything is decided.
        // A session with a client connected right now is not idle, whatever its
        // recorded timestamp says --- and acting on the stale timestamp is
        // exactly how a session gets archived out from under somebody.
        let session = if observe_attachment(&session) {
            let touched = session.with_attachment(now);
            store.put(&touched)?;
            touched
        } else {
            session
        };

        // Dirty is recomputed immediately before any destructive step (§15.2).
        let observed = observe_dirty(&session);
        let reclaimable = session.with_recomputed_dirty(observed);
        let action = decide(&reclaimable, &api.thresholds, now, rollout);

        match action {
            Action::Leave => continue,
            Action::Notify => println!("reclaim: {} idle, owner notified", session.id),
            Action::Archive => {
                // Release the tunnel name before anything else (§15.3). Tunnel
                // names are globally unique per account, so an archive that left
                // one registered collides with any session later recreated under
                // the same id --- and the collision appears as an editor that
                // will not connect, a long way from its cause.
                //
                // Failure here does not stop the archive: a name still
                // registered is a nuisance, and refusing to snapshot because of
                // it would trade a nuisance for lost work.
                if let Err(e) = unregister(&session) {
                    eprintln!("reclaim: {} tunnel not released: {e}", session.id);
                }
                // The snapshot comes next, and the state change only if it
                // succeeded. §15.3 calls archiving reversible --- a restore
                // rebuilds from the snapshot and the devcontainer.json --- and
                // a record that said `archived` with nothing behind it would
                // make that sentence false for the one session it mattered for.
                snapshot(&api, &session)?;
                archived += 1;
                println!("reclaim: {} archived", session.id);
                store.put(&session.with_state(SessionState::Archived))?;
            }
            Action::Purge => {
                forget(&api, &session)?;
                println!("reclaim: {} purged", session.id);
                store.put(&session.with_state(SessionState::Purged))?;
            }
            Action::HoldDirty => {
                held += 1;
                println!("reclaim: {} held --- {action}", session.id);
            }
        }
    }

    println!("reclaim: {archived} archived, {held} dirty archives held");
    Ok(())
}

/// Release a session's tunnel name before archiving (§15.3).
fn unregister(session: &Session) -> Result<(), ApiError> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "30",
            "--request",
            "POST",
            &format!(
                "http://{}.mesh:8081/workspaces/{}/unregister",
                session.host, session.id
            ),
        ])
        .output()
        .map_err(|e| ApiError::StoreUnavailable {
            attempted: format!("unregister the tunnel for {}", session.id),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ApiError::StoreUnavailable {
        attempted: format!("unregister the tunnel for {}", session.id),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Snapshot a session's workspace and volumes before it is archived (§15.3).
///
/// `restic` because it deduplicates: ninety days of held archives from
/// workspaces that mostly share a repository cost far less than ninety copies,
/// which is what makes holding a dirty archive indefinitely affordable.
fn snapshot(api: &Api, session: &Session) -> Result<(), ApiError> {
    let repository = env_or(
        "CLUSTER_SNAPSHOT_REPOSITORY",
        "/var/lib/restic/devcontainers",
    );
    let home = env_or("CLUSTER_DEVCONTAINER_HOME", "/export/devcontainers");
    let _ = api;

    let tool = env_or("CLUSTER_SNAPSHOT_TOOL", "restic");
    let output = std::process::Command::new(&tool)
        .args([
            "--repo",
            &repository,
            "backup",
            "--tag",
            &format!("session={}", session.id),
            "--tag",
            &format!("repo={}", session.repo),
            &format!("{home}/{}", session.id),
        ])
        .output()
        .map_err(|e| ApiError::StoreUnavailable {
            attempted: format!("snapshot {}", session.id),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ApiError::StoreUnavailable {
        attempted: format!("snapshot {}", session.id),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Delete a session's archive (§15.3).
///
/// The one irreversible step in the whole policy, and it is only ever reached
/// for a workspace observed clean immediately beforehand.
fn forget(api: &Api, session: &Session) -> Result<(), ApiError> {
    let repository = env_or(
        "CLUSTER_SNAPSHOT_REPOSITORY",
        "/var/lib/restic/devcontainers",
    );
    let _ = api;

    let tool = env_or("CLUSTER_SNAPSHOT_TOOL", "restic");
    let output = std::process::Command::new(&tool)
        .args([
            "--repo",
            &repository,
            "forget",
            "--tag",
            &format!("session={}", session.id),
            "--prune",
        ])
        .output()
        .map_err(|e| ApiError::StoreUnavailable {
            attempted: format!("delete the archive of {}", session.id),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ApiError::StoreUnavailable {
        attempted: format!("delete the archive of {}", session.id),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn rollout_in_progress(api: &Api) -> bool {
    api.nodes.iter().any(|node| {
        let Ok(output) = std::process::Command::new("curl")
            .args(["--silent", "--fail", "--max-time", "5", &node.health_url])
            .output()
        else {
            // A peer that will not answer might be mid-reboot. Treating that as
            // "no rollout" would let reclamation run during exactly the window
            // §15.4 excludes it from.
            return true;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        !output.status.success() || text.contains("\"draining\"") || text.contains("\"updating\"")
    })
}

/// Whether a client is attached to this session right now (§15.1).
///
/// The VS Code server process spawns only when a client connects, so its
/// presence is a direct signal. An unreachable agent answers "attached": the two
/// errors are not the same size, and a session archived while in use costs the
/// trust §15.3 exists to keep.
fn observe_attachment(session: &Session) -> bool {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--max-time",
            "5",
            &format!(
                "http://{}.mesh:8081/workspaces/{}/attached",
                session.host, session.id
            ),
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains("\"attached\":true")
        }
        _ => true,
    }
}

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
        Ok(o) if o.status.success() => DirtyObservation::observed(
            !String::from_utf8_lossy(&o.stdout).contains("\"dirty\":false"),
        ),
        // Unknown is dirty. An extra held archive costs a few gigabytes on a
        // 2 TB disk; a wrong purge costs someone's work (§15.3).
        _ => DirtyObservation::observed(true),
    }
}

fn api_from_environment() -> Result<Api, ApiError> {
    let store = Store::open(&env_or(
        "CLUSTER_CTL_DB",
        "/var/lib/cluster-ctl/sessions.db",
    ))?;

    let nodes = env_list("CLUSTER_PEERS")
        .iter()
        .filter_map(|spec| {
            let mut fields = spec.splitn(3, ':');
            let name = fields.next()?.to_string();
            let position = fields.next()?.parse().ok()?;
            let health_url = fields.next()?.to_string();
            Some(NodeEndpoint {
                name,
                position,
                health_url,
            })
        })
        .collect();

    Ok(Api {
        store: Arc::new(Mutex::new(store)),
        authorizer: Arc::new(Authorizer::new(
            Box::new(GitHub {
                user_url: env_or("CLUSTER_GITHUB_USER_URL", "https://api.github.com/user"),
                timeout_s: u64::from(env_number("CLUSTER_AUTH_VALIDATION_TIMEOUT_S", 10)?),
            }),
            env_list("CLUSTER_AUTHORIZED_LOGINS"),
            u64::from(env_number("CLUSTER_AUTH_TOKEN_CACHE_TTL_S", 300)?),
        )),
        thresholds: Thresholds {
            notify_after_days: env_number("CLUSTER_RECLAIM_NOTIFY_DAYS", 14)?,
            archive_after_days: env_number("CLUSTER_RECLAIM_ARCHIVE_DAYS", 30)?,
            purge_after_days: env_number("CLUSTER_RECLAIM_PURGE_DAYS", 90)?,
        },
        nodes,
        allowed_origin: env_or("CLUSTER_ALLOWED_ORIGIN", "https://afflom.github.io"),
        web_root: env_or("CLUSTER_WEB_ROOT", "/var/lib/cluster-ctl/web"),
        auth_config: AuthConfig {
            client_id: env_or("CLUSTER_GITHUB_CLIENT_ID", ""),
            scopes: env_list("CLUSTER_GITHUB_SCOPES"),
            device_code_url: env_or(
                "CLUSTER_GITHUB_DEVICE_CODE_URL",
                "https://github.com/login/device/code",
            ),
            token_url: env_or(
                "CLUSTER_GITHUB_TOKEN_URL",
                "https://github.com/login/oauth/access_token",
            ),
        },
        // Where each enrolled secret goes (§12.2). Read from the rendered
        // policy, so this binary carries no destination of its own --- and a
        // control plane that cannot read it says so rather than silently
        // offering an empty form.
        enrolment: {
            let path = env_or("CLUSTER_ENROLMENT", "/usr/lib/cluster/enrolment.conf");
            let text =
                std::fs::read_to_string(&path).map_err(|e| ApiError::EnrolmentUnavailable {
                    because: format!("reading {path}: {e}"),
                })?;
            Enrolment::parse(&text)?
        },
        enrolment_root: env_or("CLUSTER_ENROLMENT_ROOT", "/var/lib/cluster-ctl/enrolment"),
        // Only the storage node advertises the management subnet, and the mesh
        // is never advertised (§4.5).
        advertise_routes: std::env::var("CLUSTER_ADVERTISE_ROUTES")
            .ok()
            .filter(|p| !p.is_empty()),
        capacity: Capacity {
            target: env_or("CLUSTER_MIGRATION_TARGET", "storage"),
            never_receives: env_list("CLUSTER_NEVER_RECEIVES"),
            memory_cap_gib: env_number("CLUSTER_MIGRATION_MEMORY_CAP_GIB", 12)?,
        },
    })
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// A number from the environment, or a refusal.
///
/// **Absent is not the same as unreadable.** An absent key takes the documented
/// default, which is what makes the unit file able to say less than everything.
/// A key that is *present and not a number* is a misconfiguration, and it used
/// to take the default silently.
///
/// That matters because of which numbers these are. `CLUSTER_RECLAIM_PURGE_DAYS`
/// decides when somebody's archived work is deleted; a unit file carrying
/// `ninety` would have been read as ninety days by an operator and as the
/// compiled-in default by this binary, and the two agree only by luck. Every one
/// of these is rendered from `model/policy.toml`, so a malformed one means the
/// rendered tree and this binary disagree --- which is R1's failure, and it fails
/// here rather than at the first deletion.
fn env_number(key: &str, fallback: u32) -> Result<u32, ApiError> {
    match std::env::var(key) {
        Err(_) => Ok(fallback),
        Ok(raw) => raw.trim().parse().map_err(|_| ApiError::NotPermitted {
            attempted: format!("read {key} from the environment"),
            because: format!(
                "`{raw}` is not a number. It is rendered from model/policy.toml, so this \
                 is the rendered unit and this binary disagreeing --- and these numbers \
                 decide when work is deleted (§7.2, §15.3)"
            ),
        }),
    }
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rendered number that is not a number is a refusal, not a default.
    ///
    /// Absent and unreadable are different conditions. An absent key takes the
    /// documented default, which is what lets the unit file say less than
    /// everything. A key that is present and malformed means the rendered unit
    /// and this binary disagree --- and these are the numbers that decide when
    /// somebody's archived work is deleted (§7.2, §15.3).
    #[test]
    fn a_malformed_threshold_is_refused_rather_than_defaulted() {
        // Absent: the documented default.
        std::env::remove_var("CLUSTER_TEST_NUMBER");
        assert_eq!(env_number("CLUSTER_TEST_NUMBER", 90).unwrap(), 90);

        // Present and readable, including with the whitespace a unit file
        // rendering might leave.
        std::env::set_var("CLUSTER_TEST_NUMBER", "30");
        assert_eq!(env_number("CLUSTER_TEST_NUMBER", 90).unwrap(), 30);
        std::env::set_var("CLUSTER_TEST_NUMBER", " 30 ");
        assert_eq!(env_number("CLUSTER_TEST_NUMBER", 90).unwrap(), 30);

        // Present and not a number. This used to be ninety days of retention
        // that an operator had written as `ninety` and this binary had read as
        // the default --- agreeing only by luck.
        for malformed in ["ninety", "", "30d", "-1", "3.5"] {
            std::env::set_var("CLUSTER_TEST_NUMBER", malformed);
            let err = env_number("CLUSTER_TEST_NUMBER", 90)
                .expect_err(&format!("`{malformed}` is not a number"));
            assert_eq!(err.status(), 409);
            assert!(
                err.to_string().contains("CLUSTER_TEST_NUMBER"),
                "the refusal names the key: {err}"
            );
        }
        std::env::remove_var("CLUSTER_TEST_NUMBER");
    }

    /// A list is empty when the key is absent, and holds no blanks.
    #[test]
    fn a_list_from_the_environment_holds_no_blanks() {
        std::env::remove_var("CLUSTER_TEST_LIST");
        assert!(env_list("CLUSTER_TEST_LIST").is_empty());

        std::env::set_var("CLUSTER_TEST_LIST", "");
        assert!(
            env_list("CLUSTER_TEST_LIST").is_empty(),
            "an empty allowlist refuses everyone rather than admitting everyone"
        );

        std::env::set_var("CLUSTER_TEST_LIST", "afflom, someone , ,");
        assert_eq!(
            env_list("CLUSTER_TEST_LIST"),
            vec!["afflom".to_string(), "someone".to_string()],
            "a trailing comma does not add an empty login that would match an empty header"
        );
        std::env::remove_var("CLUSTER_TEST_LIST");
    }
}
