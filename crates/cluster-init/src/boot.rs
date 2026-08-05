//! The decisions `cluster-init` takes around its I/O (`SPEC.md` §8.5, §2.3.2).
//!
//! `main.rs` says it reads the ordering and the I/O, and that every step which
//! can decide something wrongly lives in the library beside it. That was not
//! quite true: four decisions were embedded in the I/O and tested by nothing.
//!
//! - what mode a file the boot writes actually ends up with, when one is
//!   already there;
//! - which kernel arguments a role's rendered set carries, and whether they
//!   differ from what was applied last;
//! - what the node environment says, and what it means for a role the policy
//!   does not declare;
//! - what `cluster-peers` reads back out of that environment on its next pass.
//!
//! Each is small, and each fails silently. A mode that is not narrowed leaves a
//! join secret readable; an argument set that compares unequal to itself stages
//! a deployment on every boot; a role the policy does not declare took position
//! zero and told the rollout this machine goes first.

use std::path::Path;

use crate::InitError;

/// What a role's rendered kernel-argument file asks for (§8.5).
///
/// The file is `options=…` or empty. Empty is meaningful and is not the same as
/// absent: two of the three roles add no arguments, and the renderer emits a
/// file for each of them precisely so that this reads a declared nothing rather
/// than failing to find anything.
pub fn kargs_of(rendered: &str) -> String {
    rendered
        .lines()
        .find_map(|l| l.trim().strip_prefix("options="))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Whether `bootc loader-entries` needs to be told anything (§8.5).
///
/// **That call stages a new deployment.** Making it unconditionally would stage
/// one on every boot of every machine --- including the two roles whose set is
/// empty, to remove a source that was never there. A boot that would set what
/// is already set does nothing at all.
///
/// `applied` is what was recorded last, which is absent on a first boot.
pub fn kargs_need_applying(applied: Option<&str>, wanted: &str) -> bool {
    applied.map(str::trim) != Some(wanted.trim())
}

/// The node environment `cluster-init` writes and `cluster-peers` reads.
///
/// One key per line. Read back by [`env_value`], which is the half that runs on
/// the next pass, several minutes and one `systemd-networkd` later.
pub fn env_value<'a>(env: &'a str, key: &str) -> Option<&'a str> {
    env.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name == key).then_some(value)
    })
}

/// What `cluster-peers` needs from the environment, or why it cannot proceed.
///
/// A missing ordinal means `cluster-init` has not run, which is a different
/// condition from a peer not answering and needs a different thing done about
/// it. Reported rather than defaulted: an ordinal of zero would be an address
/// on the mesh that belongs to no machine.
pub fn own_identity(env: &str) -> Result<(u32, String), InitError> {
    let ordinal = env_value(env, "CLUSTER_ORDINAL")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .ok_or_else(|| {
            InitError::Config(
                "the node environment carries no ordinal, so cluster-init has not run (§2.3.2)"
                    .into(),
            )
        })?;
    if ordinal == 0 {
        return Err(InitError::Addressing(
            "the node environment carries ordinal 0, which is an address on the mesh that \
             belongs to no machine (§4.1)"
                .into(),
        ));
    }
    let role = env_value(env, "CLUSTER_ROLE")
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| InitError::Config("the node environment carries no role (§2.3)".into()))?
        .trim()
        .to_string();
    Ok((ordinal, role))
}

/// Write a file only root can read, whether or not one is already there.
///
/// `0600` is set *before* the content lands, because a file created
/// world-readable and narrowed afterwards is world-readable for the width of
/// that window.
///
/// **And narrowed afterwards as well, because the first half does nothing to a
/// file that already exists.** `OpenOptions::mode` applies at creation only. The
/// join secret, the registrar's assignments and the applied kernel arguments all
/// come through here, and every one of them is rewritten on a later boot --- so a
/// file that was ever created with a wider mode kept it, silently, for the life
/// of the machine. The control plane's own secret writer had this exact pair of
/// lines already; this one had only the first.
pub fn write_private(path: &Path, content: &str) -> Result<(), InitError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| InitError::Io(format!("writing {}: {e}", path.display())))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| InitError::Io(format!("narrowing {}: {e}", path.display())))?;
    file.write_all(content.as_bytes())
        .map_err(|e| InitError::Io(format!("writing {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_role_that_adds_no_arguments_declares_so() {
        // The renderer emits a file for every role, including the two that add
        // nothing, so that this reads a declared nothing (§8.5).
        assert_eq!(kargs_of("# storage adds none\n"), "");
        assert_eq!(kargs_of(""), "");
        assert_eq!(
            kargs_of("# the testbed isolates its cores\noptions=isolcpus=2-7 nohz_full=2-7\n"),
            "isolcpus=2-7 nohz_full=2-7"
        );
        // Whitespace either side is layout, not an argument.
        assert_eq!(kargs_of("  options=  isolcpus=2-7  \n"), "isolcpus=2-7");
    }

    /// The call stages a deployment, so a boot that would set what is already
    /// set must do nothing at all.
    #[test]
    fn kernel_arguments_are_applied_only_when_they_change() {
        // First boot: nothing recorded.
        assert!(kargs_need_applying(None, "isolcpus=2-7"));
        // A role that adds nothing, on a machine that has never applied any.
        // Recorded as empty on the first pass, and quiet on every one after ---
        // which is the case that would otherwise stage a deployment per boot on
        // two of the three machines, to remove a source never set.
        assert!(kargs_need_applying(None, ""));
        assert!(!kargs_need_applying(Some(""), ""));
        assert!(!kargs_need_applying(Some("\n"), ""));

        assert!(!kargs_need_applying(Some("isolcpus=2-7"), "isolcpus=2-7"));
        assert!(!kargs_need_applying(Some("isolcpus=2-7\n"), "isolcpus=2-7"));
        assert!(kargs_need_applying(Some("isolcpus=2-7"), "isolcpus=4-7"));
        // Re-roled: a machine that was the testbed and is now a compute node
        // has arguments to remove.
        assert!(kargs_need_applying(Some("isolcpus=2-7"), ""));
    }

    #[test]
    fn the_node_environment_round_trips() {
        let env = crate::units::node_env(2, "node2", "compute", 2, "10.10.255.2");
        assert_eq!(env_value(&env, "CLUSTER_ORDINAL"), Some("2"));
        assert_eq!(env_value(&env, "CLUSTER_ROLE"), Some("compute"));
        assert_eq!(own_identity(&env).unwrap(), (2, "compute".to_string()));

        // A key that is a prefix of another is not a match for it.
        assert_eq!(env_value("CLUSTER_ROLE_X=a\n", "CLUSTER_ROLE"), None);
    }

    /// `cluster-peers` runs after `cluster-init`, and cannot proceed on what it
    /// did not write. Each absence is its own condition rather than a zero.
    #[test]
    fn peers_refuses_an_environment_it_cannot_use() {
        for (env, why) in [
            ("", "cluster-init has not run"),
            ("CLUSTER_ROLE=compute\n", "no ordinal"),
            ("CLUSTER_ORDINAL=2\n", "no role"),
            ("CLUSTER_ORDINAL=2\nCLUSTER_ROLE=\n", "an empty role"),
            ("CLUSTER_ORDINAL=two\nCLUSTER_ROLE=compute\n", "unparseable"),
            (
                "CLUSTER_ORDINAL=0\nCLUSTER_ROLE=compute\n",
                "ordinal zero is an address belonging to no machine",
            ),
        ] {
            assert!(
                own_identity(env).is_err(),
                "{why}: {env:?} must be refused rather than defaulted"
            );
        }
    }

    /// A file that was already there is narrowed too.
    ///
    /// `OpenOptions::mode` applies at creation only, so a secret created with a
    /// wider mode kept it for the life of the machine --- silently, because
    /// nothing reads a mode back.
    #[test]
    fn an_existing_file_is_narrowed_and_not_merely_created_narrow() {
        let dir = std::env::temp_dir().join(format!("cluster-init-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("join.secret");

        // A file somebody --- or an earlier version of this binary --- left
        // world-readable.
        std::fs::write(&path, "old").expect("it is written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("it is widened");

        write_private(&path, "a-new-secret").expect("it is rewritten");

        let mode = std::fs::metadata(&path)
            .expect("it exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "a join secret any local user can read is a join secret any local user has"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("it is readable by root"),
            "a-new-secret"
        );

        // And a fresh one is created narrow rather than narrowed afterwards.
        let fresh = dir.join("registry.json");
        write_private(&fresh, "{}").expect("it is written");
        assert_eq!(
            std::fs::metadata(&fresh)
                .expect("it exists")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
