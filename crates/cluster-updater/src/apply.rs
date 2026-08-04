//! Resolving, verifying, and applying (`SPEC.md` §12.3, §13.1, §13.3).
//!
//! This is the only module that acts. Everything else in the crate decides.
//!
//! # Resolution order
//!
//! Local Zot first, GHCR on failure --- and it *will* fail, by design, during
//! `n1`'s own reboot (§14.2). Registries are tried in the order the model
//! renders, and a node that cannot reach any of them reports a
//! [`crate::RolloutError`] rather than inventing an answer: an unresolved target
//! is not "no update available", and treating it as one would silently pin a
//! node to whatever it last booted.
//!
//! # Verification is not optional
//!
//! §12.3's policy is what stands between a node and an arbitrary image. The node
//! applies whatever `:stable` points at, unattended, with no operator present,
//! so the signature check happens before staging and a failure is a halt
//! (§13.5) rather than a warning.

use std::process::Command;

use crate::{RolloutError, Stage};

/// Where the acting happens.
///
/// A trait because the alternative is a rollout whose only test is a reboot. T2
/// drives the real one inside guests; the decision logic in
/// [`crate::rollout`] and [`crate::drain`] is exercised against its oracle
/// without either.
pub trait Applier {
    /// What `:stable` resolves to, from the first registry that answers.
    fn resolve_target(&self) -> Result<String, RolloutError>;
    /// Verify a digest's signature against the shipped policy (§12.3).
    fn verify(&self, digest: &str) -> Result<(), RolloutError>;
    /// Publish this node's rollout state, so peers can read it (§13.2).
    fn publish_state(&self, state: cluster_health::State) -> Result<(), RolloutError>;
    /// `bootc upgrade`, then reboot. greenboot owns what happens next (§13.3).
    fn upgrade_and_reboot(&self) -> Result<(), RolloutError>;
    /// Record a digest as quarantined (§13.4).
    fn quarantine(&self, digest: &str) -> Result<(), RolloutError>;
}

/// The real applier: the node, acting on itself.
#[derive(Debug, Clone)]
pub struct SystemApplier {
    /// Registries in preference order, from the rendered environment.
    pub registries: Vec<String>,
    /// The image this node follows, without a tag.
    pub image: String,
    /// The tag whose digest is the target.
    pub tag: String,
    /// Where this node writes its own rollout state for peers to read.
    pub state_path: String,
    /// The control plane's base URL, for quarantine.
    pub control_plane: String,
    /// This node's name.
    pub node: String,
}

impl Applier for SystemApplier {
    fn resolve_target(&self) -> Result<String, RolloutError> {
        let mut attempts = Vec::new();
        for registry in &self.registries {
            let reference = format!("{registry}/{}:{}", self.image, self.tag);
            let output = Command::new("skopeo")
                .args([
                    "inspect",
                    "--format",
                    "{{.Digest}}",
                    &format!("docker://{reference}"),
                ])
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    let digest = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if !digest.is_empty() {
                        return Ok(digest);
                    }
                    attempts.push(format!("{reference}: answered with an empty digest"));
                }
                Ok(o) => attempts.push(format!(
                    "{reference}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => attempts.push(format!("{reference}: {e}")),
            }
        }
        // No registry answered. This is not "no update available": pinning a
        // node to its current image because the network was down would make an
        // outage look like a policy decision.
        Err(RolloutError {
            stage: Stage::Resolve,
            attempted: format!(
                "resolve {}:{} from {:?}",
                self.image, self.tag, self.registries
            ),
            because: attempts.join("; "),
        })
    }

    fn verify(&self, digest: &str) -> Result<(), RolloutError> {
        // The policy that decides this ships inside the image
        // (/etc/containers/policy.json, §12.3): a sigstore signature whose OIDC
        // issuer is GitHub and whose identity is this repository's promote.yml.
        // An image signed by anything else does not stage, including one signed
        // by a different workflow in the same repository.
        let reference = format!("{}@{digest}", self.image);
        let output = Command::new("skopeo")
            .args([
                "inspect",
                "--policy",
                "/etc/containers/policy.json",
                &format!("docker://{reference}"),
            ])
            .output()
            .map_err(|e| RolloutError {
                stage: Stage::Verify,
                attempted: format!("verify {reference}"),
                because: e.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(RolloutError {
            stage: Stage::Verify,
            attempted: format!("verify {reference}"),
            because: format!(
                "{}. The policy is the only thing standing between this node and an \
                 arbitrary image (§12.3)",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }

    fn publish_state(&self, state: cluster_health::State) -> Result<(), RolloutError> {
        // Written under /var, which the bootc filesystem contract makes writable
        // and persistent across image transitions (§5.2). `cluster-health` reads
        // it and serves it, so a peer evaluating §13.2 sees it.
        std::fs::write(&self.state_path, state.as_str()).map_err(|e| RolloutError {
            stage: Stage::Observe,
            attempted: format!("publish state `{}` to {}", state.as_str(), self.state_path),
            because: e.to_string(),
        })
    }

    fn upgrade_and_reboot(&self) -> Result<(), RolloutError> {
        let upgrade = Command::new("bootc")
            .arg("upgrade")
            .output()
            .map_err(|e| RolloutError {
                stage: Stage::Apply,
                attempted: "bootc upgrade".to_string(),
                because: e.to_string(),
            })?;
        if !upgrade.status.success() {
            return Err(RolloutError {
                stage: Stage::Apply,
                attempted: "bootc upgrade".to_string(),
                because: String::from_utf8_lossy(&upgrade.stderr).trim().to_string(),
            });
        }

        // From here greenboot owns the outcome: the boot is declared successful
        // only if the health predicate passes within the deadline, and on
        // failure the previous ostree deployment is restored automatically
        // (§13.3). That is the single reason unattended update is acceptable ---
        // a bad image costs one reboot cycle on one node rather than a cluster.
        Command::new("systemctl")
            .arg("reboot")
            .output()
            .map_err(|e| RolloutError {
                stage: Stage::Apply,
                attempted: "systemctl reboot".to_string(),
                because: e.to_string(),
            })?;
        Ok(())
    }

    fn quarantine(&self, digest: &str) -> Result<(), RolloutError> {
        let body = format!("{{\"digest\":\"{digest}\",\"node\":\"{}\"}}", self.node);
        let output = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "10",
                "--request",
                "POST",
                "--header",
                "content-type: application/json",
                "--data",
                &body,
                &format!("{}/api/rollout/quarantine", self.control_plane),
            ])
            .output()
            .map_err(|e| RolloutError {
                stage: Stage::Quarantine,
                attempted: format!("quarantine {digest}"),
                because: e.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(RolloutError {
            stage: Stage::Quarantine,
            attempted: format!("quarantine {digest}"),
            because: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}
