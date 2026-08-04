//! `/usr/bin/devcontainer-agent` (`SPEC.md` §14.3, §15.1, §15.2).
//!
//! Four endpoints on the mesh loopback, and one command the SSH hook calls.
//!
//! | Endpoint | Purpose |
//! | --- | --- |
//! | `GET /workspaces/:id/dirty` | the answer §15.2 requires before any destructive step |
//! | `POST /workspaces/:id/attached` | records an attachment, which drives §15.3 |
//! | `POST /workspaces/:id/migrate` | runs §14.3's six steps |
//! | `GET /workspaces` | what this node is hosting, for a drain to enumerate |
//!
//! A hand-written HTTP responder rather than a framework, for the reason
//! `cluster-health` gives: this runs on the node whose devcontainers are the
//! thing being protected, and every dependency is one more thing that has to be
//! right before it can answer whether somebody's work is safe to delete.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use devcontainer_agent::{commands, is_dirty, observe_attachment, AgentError, Migration, Step};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") | None => serve(),
        // Called by the `sshrc` hook on every connection, and by the tunnel
        // attachment on `n2`. `last_attached_at` drives every reclamation
        // threshold, and a session somebody is using that looks idle is one that
        // gets archived out from under them (§15.1).
        Some("attached") => match args.get(1) {
            Some(session) => record_attachment(session),
            None => Err(AgentError {
                session: String::new(),
                attempted: "record an attachment".to_string(),
                because: "no session was named".to_string(),
            }),
        },
        Some(other) => {
            eprintln!(
                "devcontainer-agent <serve|attached SESSION>\n\
                 \n\
                 serve              answer for this node's workspaces (§15.2)\n\
                 attached SESSION   record an attachment (§15.1)\n\
                 \n\
                 unknown mode `{other}`"
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("devcontainer-agent: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve() -> Result<(), AgentError> {
    let bind = env_or("AGENT_BIND", "127.0.0.1:8081");
    let listener = TcpListener::bind(&bind).map_err(|e| AgentError {
        session: String::new(),
        attempted: format!("bind {bind}"),
        because: e.to_string(),
    })?;
    println!("devcontainer-agent: serving on {bind}");

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        handle(&mut stream);
    }
    Ok(())
}

/// Answer one request.
fn handle(stream: &mut TcpStream) {
    let mut reader = BufReader::new(&*stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (status, body) = match (method, segments.as_slice()) {
        ("GET", ["workspaces"]) => (200, hosted()),
        // §15.1's primary attachment signal. The control plane asks; only this
        // node can see the process table the answer comes from.
        ("GET", ["workspaces", _id, "attached"]) => {
            (200, format!("{{\"attached\":{}}}", observe_attachment()))
        }
        // §15.3: unregister before archiving. Tunnel names are globally unique
        // per account, so an archive that left the name registered collides with
        // any session later recreated under the same id --- and the collision
        // shows up as an editor that will not connect, a long way from its cause.
        ("POST", ["workspaces", id, "unregister"]) => match unregister(id) {
            Ok(()) => (
                200,
                format!("{{\"session\":\"{id}\",\"unregistered\":true}}"),
            ),
            Err(e) => (500, format!("{{\"error\":\"{e}\"}}")),
        },
        ("GET", ["workspaces", id, "dirty"]) => {
            // Computed now, from the worktree, every time. §15.2 forbids a
            // cached answer, and a cache here would be the one place the whole
            // dirty protection could quietly stop working.
            let state = is_dirty(&workspace_of(id));
            (
                200,
                format!(
                    "{{\"session\":\"{id}\",\"dirty\":{},\"reason\":\"{}\"}}",
                    state.is_dirty(),
                    state.reason()
                ),
            )
        }
        ("POST", ["workspaces", id, "attached"]) => match record_attachment(id) {
            Ok(()) => (200, format!("{{\"session\":\"{id}\",\"attached\":true}}")),
            Err(e) => (503, format!("{{\"error\":\"{e}\"}}")),
        },
        ("POST", ["workspaces", id, "migrate"]) => match migrate(id) {
            Ok(steps) => (200, format!("{{\"session\":\"{id}\",\"steps\":{steps}}}")),
            Err(e) => (500, format!("{{\"error\":\"{e}\"}}")),
        },
        _ => (404, "{}".to_string()),
    };

    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{body}",
        body.len()
    );
}

/// The sessions this node is hosting, for a drain to enumerate.
fn hosted() -> String {
    let root = PathBuf::from(env_or("AGENT_WORKSPACES", "/var/lib/devcontainers"));
    let Ok(entries) = std::fs::read_dir(&root) else {
        return "{\"workspaces\":[]}".to_string();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    let list = names
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"workspaces\":[{list}]}}")
}

fn workspace_of(session: &str) -> PathBuf {
    PathBuf::from(env_or("AGENT_WORKSPACES", "/var/lib/devcontainers")).join(session)
}

/// Tell the control plane a session was attached to (§15.1).
fn record_attachment(session: &str) -> Result<(), AgentError> {
    let control = env_or("AGENT_CONTROL_PLANE", "http://127.0.0.1:8080");
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "5",
            "--request",
            "POST",
            &format!("{control}/api/sessions/{session}/attached"),
        ])
        .output()
        .map_err(|e| AgentError {
            session: session.to_string(),
            attempted: "record an attachment".to_string(),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(AgentError {
        session: session.to_string(),
        attempted: "record an attachment".to_string(),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Release a session's tunnel name (§15.3).
fn unregister(session: &str) -> Result<(), AgentError> {
    let output = Command::new("podman")
        .args([
            "exec",
            &format!("devcontainer-{session}"),
            "/usr/local/share/cluster-tunnel/unregister.sh",
        ])
        .output()
        .map_err(|e| AgentError {
            session: session.to_string(),
            attempted: "unregister the tunnel".to_string(),
            because: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(AgentError {
        session: session.to_string(),
        attempted: "unregister the tunnel".to_string(),
        because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Run §14.3's six steps for one session.
fn migrate(session: &str) -> Result<String, AgentError> {
    let control = env_or("AGENT_CONTROL_PLANE", "http://127.0.0.1:8080");
    let migration = Migration {
        session: session.to_string(),
        workspace: workspace_of(session),
        home: env_or("AGENT_HOME", "/var/lib/devcontainer-home"),
        target: env_or("CLUSTER_MIGRATION_TARGET", "n1"),
        image_digest: digest_of(session)?,
        grace_s: env_or("AGENT_STOP_GRACE_S", "30").parse().unwrap_or(30),
    };

    let mut done = Vec::new();
    for step in Step::ALL {
        for argv in commands(&migration, step) {
            let (program, rest) = argv.split_first().expect("a command has a program");
            // The two control-plane steps carry a path; the base URL is this
            // node's configuration, not the library's.
            let rest: Vec<String> = rest
                .iter()
                .map(|a| {
                    if a.starts_with("/api/") {
                        format!("{control}{a}")
                    } else {
                        a.clone()
                    }
                })
                .collect();

            let output = Command::new(program)
                .args(&rest)
                .output()
                .map_err(|e| AgentError {
                    session: session.to_string(),
                    attempted: format!("{}: {program}", step.as_str()),
                    because: e.to_string(),
                })?;
            if !output.status.success() {
                // Past the point of no return the failure is reported as it is.
                // Before it, the container is still running and the caller can
                // leave the session where it is --- §14.3's budget then decides
                // whether the rollout halts or the session is stopped with
                // notice, and that decision is the control plane's, not ours.
                return Err(AgentError {
                    session: session.to_string(),
                    attempted: format!(
                        "{} ({})",
                        step.as_str(),
                        if step.is_reversible() {
                            "reversible: the container is still running here"
                        } else {
                            "past the point of no return"
                        }
                    ),
                    because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                });
            }
        }
        done.push(format!("\"{}\"", step.as_str()));
    }
    Ok(format!("[{}]", done.join(",")))
}

/// The digest a session's container was built from (§14.3).
fn digest_of(session: &str) -> Result<String, AgentError> {
    let output = Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.ImageName}}",
            &format!("devcontainer-{session}"),
        ])
        .output()
        .map_err(|e| AgentError {
            session: session.to_string(),
            attempted: "read the container's image".to_string(),
            because: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(AgentError {
            session: session.to_string(),
            attempted: "read the container's image".to_string(),
            because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}
