//! The node-local agent (`SPEC.md` §14.3, §15.1, §15.2).
//!
//! The control plane knows *which* sessions exist; only this agent can see the
//! worktrees. Three jobs follow from that:
//!
//! **Dirty.** §15.2 requires the flag to be recomputed immediately before any
//! destructive step, never read from cache — and the only thing that can compute
//! it is something on the node with the worktree in front of it. [`is_dirty`] is
//! that computation, and it is deliberately conservative in one direction only.
//!
//! **Attachment.** `last_attached_at` drives every reclamation threshold
//! (§15.3), and it is updated on every SSH connection and every tunnel
//! attachment. A session somebody is using that looks idle is a session that
//! gets archived out from under them.
//!
//! **Migration.** §14.3's six steps, in order. What survives is the git
//! worktree, the declared volumes, and the `devcontainer.json` that built it —
//! not the process state. Attached editor sessions drop, by design; there is no
//! way around that short of CRIU, which is not reliable for VS Code server, open
//! TTYs and live SSH sockets.

#![deny(missing_docs)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one error a caller of this crate can see (R5).
///
/// Sanctioned by `CG-06` in `model/ids.toml`. It reports that the agent could
/// not carry out a step on this node's filesystem or container runtime.
///
/// It deliberately does **not** cover "the workspace is dirty": that is an
/// answer, and [`is_dirty`] returns it as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    /// Which session, where there is one.
    pub session: String,
    /// What was attempted.
    pub attempted: String,
    /// Why it could not be done.
    pub because: String,
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.session, self.attempted, self.because)
    }
}

impl std::error::Error for AgentError {}

/// What a workspace's git state says about whether work would be lost.
///
/// Three independent conditions, any of which makes the workspace dirty (§15.2).
/// They are kept apart rather than collapsed into a bool so that the answer can
/// say *why* — an operator acknowledging a held archive needs to know whether it
/// is an uncommitted edit or a branch nobody pushed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceState {
    /// Tracked files modified but not committed.
    pub uncommitted: bool,
    /// Commits on any branch that are not on a remote.
    pub unpushed: bool,
    /// Files present, not ignored, and never added.
    pub untracked: bool,
}

impl WorkspaceState {
    /// Whether any condition holds.
    pub const fn is_dirty(&self) -> bool {
        self.uncommitted || self.unpushed || self.untracked
    }

    /// Why, in the words §15.2 uses.
    pub fn reason(&self) -> String {
        let mut reasons = Vec::new();
        if self.uncommitted {
            reasons.push("uncommitted tracked changes");
        }
        if self.unpushed {
            reasons.push("unpushed commits");
        }
        if self.untracked {
            reasons.push("untracked non-ignored files");
        }
        if reasons.is_empty() {
            "clean".to_string()
        } else {
            reasons.join(", ")
        }
    }
}

/// Compute a workspace's state, right now.
///
/// # Conservative in one direction
///
/// A workspace that cannot be inspected — the path is gone, git will not run,
/// the repository is corrupt — is reported **dirty**. That is not a hedge: an
/// extra held archive costs a few gigabytes on a 2 TB disk, and a wrong purge
/// costs somebody's work, so the two errors are not the same size and the answer
/// should not pretend they are (§15.3).
pub fn is_dirty(workspace: &Path) -> WorkspaceState {
    let dirty = WorkspaceState {
        uncommitted: true,
        unpushed: true,
        untracked: true,
    };
    if !workspace.exists() {
        return dirty;
    }

    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).to_string())
    };

    // Not a repository at all: there is no history to have pushed anywhere, so
    // anything in it exists only here.
    if git(&["rev-parse", "--is-inside-work-tree"]).is_none() {
        return dirty;
    }

    let Some(status) = git(&["status", "--porcelain", "--untracked-files=normal"]) else {
        return dirty;
    };
    let mut state = WorkspaceState::default();
    for line in status.lines() {
        if line.starts_with("??") {
            state.untracked = true;
        } else if !line.trim().is_empty() {
            state.uncommitted = true;
        }
    }

    // Every branch, not just the current one. §15.2 says "unpushed commits on
    // any branch", and a developer who committed on a feature branch and checked
    // out main has work that exists nowhere else.
    match git(&[
        "for-each-ref",
        "--format=%(refname:short) %(upstream:track)",
        "refs/heads",
    ]) {
        None => state.unpushed = true,
        Some(refs) => {
            for line in refs.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // No upstream at all, or ahead of it.
                let tracked = line.contains('[');
                if !tracked || line.contains("ahead") {
                    state.unpushed = true;
                }
            }
        }
    }

    state
}

/// Whether a client is attached to a session's tunnel (`SPEC.md` §15.1).
///
/// # Why the server process, and not the tunnel process
///
/// The tunnel process runs continuously for as long as the container lives,
/// whether or not anybody is looking. It says nothing about attachment. The VS
/// Code **server** process spawns only when a client actually connects, so its
/// presence is a direct statement that somebody is there --- no log parsing, no
/// heuristic, no inactivity timer to tune.
///
/// This matters more than its size suggests. §15.3's entire retention policy is
/// computed from `last_attached_at`, and a session somebody is working in that
/// looks idle is a session archived out from under them.
///
/// # Conservative in one direction
///
/// If the process table cannot be read, the answer is **attached**. The two
/// errors are not the same size: a session held slightly too long costs disk,
/// and one archived while in use costs the trust the whole of §15.3 is trying to
/// keep.
pub fn is_attached(processes: &str) -> bool {
    processes
        .lines()
        .any(|line| line.contains("vscode-server") || line.contains(".vscode-server"))
}

/// Read the process table and say whether a client is attached.
pub fn observe_attachment() -> bool {
    match Command::new("ps").args(["-eo", "args"]).output() {
        Ok(output) if output.status.success() => {
            is_attached(&String::from_utf8_lossy(&output.stdout))
        }
        // Unreadable is attached. See above: the errors are not symmetric.
        _ => true,
    }
}

/// One step of §14.3's migration, in the order the specification gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Stop accepting new starts; notify attached sessions.
    Quiesce,
    /// `rsync` the workspace to its home on the storage node.
    Sync,
    /// Stop the container, with its declared grace period.
    Stop,
    /// Start it on the target from the same image digest.
    Recreate,
    /// Update the session's current host in the control plane.
    Record,
    /// Tell attached clients to reconnect.
    Notify,
}

impl Step {
    /// Every step, in order.
    pub const ALL: [Self; 6] = [
        Self::Quiesce,
        Self::Sync,
        Self::Stop,
        Self::Recreate,
        Self::Record,
        Self::Notify,
    ];

    /// The step's name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quiesce => "quiesce",
            Self::Sync => "sync",
            Self::Stop => "stop",
            Self::Recreate => "recreate",
            Self::Record => "record",
            Self::Notify => "notify",
        }
    }

    /// Whether this step can still be undone by leaving the container where it
    /// is.
    ///
    /// Everything up to and including `Sync` is: the container is still running
    /// and the copy is additional. From `Stop` onwards the session is moving,
    /// and a failure has to be reported rather than silently retried in place.
    pub const fn is_reversible(self) -> bool {
        matches!(self, Self::Quiesce | Self::Sync)
    }
}

/// Where a migration is sending a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The session being moved.
    pub session: String,
    /// Its workspace on this node.
    pub workspace: PathBuf,
    /// Where the durable copy lives on the target.
    pub home: String,
    /// The node receiving it.
    pub target: String,
    /// The image digest it was built from, so it is recreated from the same one.
    pub image_digest: String,
    /// The container's declared stop grace period.
    pub grace_s: u64,
}

/// The commands one migration step issues.
///
/// Returned rather than run, so the sequence is checkable without a node. The
/// order and the reversibility boundary are the parts that matter, and both are
/// properties of this function.
pub fn commands(migration: &Migration, step: Step) -> Vec<Vec<String>> {
    let s = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<String>>();
    match step {
        // Nothing new starts here while the move is under way. Done first so a
        // session created mid-migration is not left behind on a node that is
        // about to reboot.
        Step::Quiesce => vec![s(&["touch", "/run/devcontainer-agent.quiesced"])],
        // Trailing slash on the source: copy the contents, not the directory.
        // `--delete` so a file removed on this node does not reappear on the
        // target as a resurrection of work somebody deleted on purpose.
        Step::Sync => vec![s(&[
            "rsync",
            "--archive",
            "--delete",
            "--hard-links",
            "--acls",
            "--xattrs",
            &format!("{}/", migration.workspace.display()),
            &format!("{}/{}/", migration.home, migration.session),
        ])],
        Step::Stop => vec![s(&[
            "podman",
            "stop",
            "--time",
            &migration.grace_s.to_string(),
            &format!("devcontainer-{}", migration.session),
        ])],
        // The same image digest. Rebuilding would produce a container that is
        // *like* the one that was running, and §14.3's durable state includes
        // what it was built from precisely so it does not have to be.
        Step::Recreate => vec![s(&[
            "podman",
            "--remote",
            "--connection",
            &migration.target,
            "run",
            "--detach",
            "--name",
            &format!("devcontainer-{}", migration.session),
            "--volume",
            &format!("{}/{}:/workspaces:rw,Z", migration.home, migration.session),
            &migration.image_digest,
        ])],
        Step::Record => vec![s(&[
            "curl",
            "--silent",
            "--show-error",
            "--fail",
            "--request",
            "POST",
            &format!("/api/sessions/{}/migrate", migration.session),
        ])],
        Step::Notify => vec![s(&[
            "curl",
            "--silent",
            "--fail",
            "--request",
            "POST",
            &format!("/api/sessions/{}/notify", migration.session),
        ])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration() -> Migration {
        Migration {
            session: "abc123".to_string(),
            workspace: PathBuf::from("/var/lib/devcontainers/abc123"),
            home: "/var/lib/devcontainer-home".to_string(),
            target: "node1".to_string(),
            image_digest: "sha256:aaaa".to_string(),
            grace_s: 30,
        }
    }

    /// The attachment signal §15.1 relies on: a server process, not a tunnel.
    #[test]
    fn a_running_server_process_is_an_attachment_cg_08() {
        // The tunnel alone is not an attachment: it runs whether or not anybody
        // is looking, and treating it as one would make every session look
        // permanently in use and §15.3 never reclaim anything.
        assert!(!is_attached(
            "/usr/local/bin/code tunnel --name dc-abc123\n"
        ));

        // A server process is. It spawns only when a client connects.
        assert!(is_attached(
            "/usr/local/bin/code tunnel --name dc-abc123\n\
             /home/vscode/.vscode-server/bin/abc/node /home/vscode/.vscode-server/bin/abc/out/server-main.js\n"
        ));
        assert!(is_attached(
            "/root/.vscode-server/bin/deadbeef/node --inspect\n"
        ));

        // Nothing at all is not an attachment, but it is also not an error:
        // an idle container has a tunnel and no client.
        assert!(!is_attached(""));
    }

    /// `CG-06`: a workspace that cannot be inspected is dirty.
    ///
    /// The two errors are not the same size: an extra held archive costs a few
    /// gigabytes, a wrong purge costs somebody's work.
    #[test]
    fn an_uninspectable_workspace_is_dirty_cg_06() {
        let state = is_dirty(Path::new("/definitely/not/a/workspace"));
        assert!(state.is_dirty());
        assert_eq!(
            state.reason(),
            "uncommitted tracked changes, unpushed commits, untracked non-ignored files"
        );

        // A directory that exists but is not a repository: nothing in it has
        // been pushed anywhere, because there is nowhere for it to have gone.
        let scratch = std::env::temp_dir().join("cluster-agent-not-a-repo");
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
        assert!(is_dirty(&scratch).is_dirty());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// `CG-06`: a clean workspace is clean, and each condition is reported
    /// independently.
    #[test]
    fn each_dirty_condition_is_reported_independently_cg_06() {
        assert!(!WorkspaceState::default().is_dirty());
        assert_eq!(WorkspaceState::default().reason(), "clean");

        for (state, expected) in [
            (
                WorkspaceState {
                    uncommitted: true,
                    ..Default::default()
                },
                "uncommitted tracked changes",
            ),
            (
                WorkspaceState {
                    unpushed: true,
                    ..Default::default()
                },
                "unpushed commits",
            ),
            (
                WorkspaceState {
                    untracked: true,
                    ..Default::default()
                },
                "untracked non-ignored files",
            ),
        ] {
            assert!(state.is_dirty());
            assert_eq!(state.reason(), expected);
        }
    }

    /// `CW-03`: the migration runs §14.3's steps in order, and the point of no
    /// return is where the container stops.
    #[test]
    fn the_migration_runs_the_declared_steps_in_order_cw_03() {
        assert_eq!(
            Step::ALL.map(Step::as_str),
            ["quiesce", "sync", "stop", "recreate", "record", "notify"],
            "§14.3 gives these in this order, and quiesce before sync is what \
             stops a session being created onto a node that is about to reboot"
        );

        // Everything up to the sync leaves the container running, so a failure
        // can be abandoned in place. From the stop onwards it cannot.
        assert!(Step::Quiesce.is_reversible());
        assert!(Step::Sync.is_reversible());
        for step in [Step::Stop, Step::Recreate, Step::Record, Step::Notify] {
            assert!(!step.is_reversible(), "{step:?}");
        }
    }

    /// `CW-03`: what survives is the worktree, the volumes, and the digest it
    /// was built from --- not the process state.
    #[test]
    fn the_migration_preserves_the_durable_state_cw_03() {
        let m = migration();

        let sync = &commands(&m, Step::Sync)[0];
        assert!(sync.contains(&"--archive".to_string()));
        // A trailing slash on the source copies the contents; without it rsync
        // would nest the workspace one directory deeper on every migration.
        assert!(sync.iter().any(|a| a.ends_with("/abc123/")));
        // A file deleted here must not reappear there as a resurrection of work
        // somebody removed on purpose.
        assert!(sync.contains(&"--delete".to_string()));

        // The declared grace period, from the model.
        let stop = &commands(&m, Step::Stop)[0];
        assert!(stop.contains(&"30".to_string()));

        // The same digest. Rebuilding would produce a container that is *like*
        // the one that was running (§14.3).
        let recreate = &commands(&m, Step::Recreate)[0];
        assert!(recreate.contains(&m.image_digest));
        assert!(recreate.iter().any(|a| a.contains(&m.target)));
        // And the workspace it mounts is the copy on the target, not the one on
        // the node being drained.
        assert!(recreate.iter().any(|a| a.starts_with(&m.home)));
    }
}
