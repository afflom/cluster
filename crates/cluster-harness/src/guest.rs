//! Booting a guest and asking it questions (`SPEC.md` §10.3, §10.4).
//!
//! # One base image, per-node overlays
//!
//! Hosted runners provide roughly 14 GB free, which does not hold three bootc
//! disk images (§9.4). Every guest therefore gets a qcow2 *overlay* over one
//! shared backing file: three nodes cost one image plus three deltas, and
//! throwing a node away is deleting a file.
//!
//! # The transition is the test
//!
//! A fresh boot proves an image is buildable. What is done to hardware is an
//! upgrade, and that is where breakage lives --- SELinux relabels, `/etc`
//! three-way merge conflicts, storage migrations (§10.4). [`Guest::upgrade_to`]
//! and [`Guest::rollback`] are what T2 drives, in both directions, because an
//! untested rollback is not a recovery path --- and under §13 it is a path taken
//! with no operator present.
//!
//! # Nothing here silently succeeds
//!
//! Every operation returns [`GuestError`] with what was attempted and what the
//! guest said. A harness that swallowed a failed SSH command would turn a broken
//! node into a passing tier, which is the one outcome worse than a red one.

use std::fmt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use cluster_model::{Cluster, Node};

use crate::{mesh_netdevs, qemu_args, ssh_port, Acceleration};

/// What went wrong driving a guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestError {
    /// Which guest.
    pub node: String,
    /// What was attempted.
    pub attempted: String,
    /// What happened.
    pub because: String,
}

impl fmt::Display for GuestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.node, self.attempted, self.because)
    }
}

impl std::error::Error for GuestError {}

/// How long to wait for a guest to answer SSH after a boot.
///
/// Generous: a bootc guest on a cold overlay relabels SELinux on first boot, and
/// a timeout tuned to a warm boot would make the *first* run of every tier flaky
/// --- which teaches whoever sees it to re-run rather than to read.
pub const BOOT_TIMEOUT_S: u64 = 300;

/// A booted guest.
#[derive(Debug)]
pub struct Guest {
    /// Which node it is standing in for.
    pub node: String,
    /// Where its SSH is forwarded on the host.
    pub ssh_port: u16,
    /// Its overlay disk.
    pub disk: PathBuf,
    process: Option<Child>,
}

/// Where the harness keeps its disks and where it finds OVMF.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// The shared backing image every overlay is cut from.
    pub backing: PathBuf,
    /// Scratch directory for overlays.
    pub scratch: PathBuf,
    /// The UEFI firmware image. A BIOS guest would be testing a boot path no
    /// node uses.
    pub firmware_code: PathBuf,
    /// The firmware's variable-store template, copied per guest so each has a
    /// writable one.
    pub firmware_vars: PathBuf,
}

/// Where a distribution puts OVMF, in the order they are tried.
///
/// Searched rather than assumed. A single hard-coded path meant T1 skipped on
/// every hosted runner --- reporting, wrongly, that KVM was absent --- when the
/// only problem was that Ubuntu had moved the file. Each entry is a real
/// location: the `_4M` pair is current Debian and Ubuntu, the unsuffixed pair is
/// older Ubuntu, and the `edk2` path is Fedora and RHEL.
const FIRMWARE_CANDIDATES: &[(&str, &str)] = &[
    (
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_VARS.fd",
    ),
    (
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    ),
    (
        "/usr/share/qemu/edk2-x86_64-code.fd",
        "/usr/share/qemu/edk2-i386-vars.fd",
    ),
];

/// The first firmware pair present on this machine.
fn discover_firmware() -> Option<(PathBuf, PathBuf)> {
    FIRMWARE_CANDIDATES.iter().find_map(|(code, vars)| {
        let (code, vars) = (PathBuf::from(code), PathBuf::from(vars));
        (code.exists() && vars.exists()).then_some((code, vars))
    })
}

impl Fixture {
    /// The fixture the tiers use, from the environment the workflow sets.
    pub fn from_environment() -> Self {
        Self {
            backing: PathBuf::from(
                std::env::var("CLUSTER_BACKING_IMAGE")
                    .unwrap_or_else(|_| "target/harness/base.qcow2".to_string()),
            ),
            scratch: PathBuf::from(
                std::env::var("CLUSTER_SCRATCH").unwrap_or_else(|_| "target/harness".to_string()),
            ),
            firmware_code: std::env::var("CLUSTER_OVMF_CODE")
                .map(PathBuf::from)
                .ok()
                .or_else(|| discover_firmware().map(|(code, _)| code))
                .unwrap_or_else(|| PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")),
            firmware_vars: std::env::var("CLUSTER_OVMF_VARS")
                .map(PathBuf::from)
                .ok()
                .or_else(|| discover_firmware().map(|(_, vars)| vars))
                .unwrap_or_else(|| PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        }
    }

    /// Whether everything a guest needs is present.
    ///
    /// Reported as a reason rather than a bool, so a tier that cannot run says
    /// *which* piece is missing instead of printing a generic skip.
    pub fn missing(&self) -> Option<String> {
        if !Acceleration::probe().can_boot() {
            return Some("/dev/kvm is absent".to_string());
        }
        if !self.backing.exists() {
            return Some(format!("{} does not exist", self.backing.display()));
        }
        for firmware in [&self.firmware_code, &self.firmware_vars] {
            if !firmware.exists() {
                return Some(format!(
                    "{} does not exist; none of {:?} was found either",
                    firmware.display(),
                    FIRMWARE_CANDIDATES
                        .iter()
                        .map(|(c, _)| *c)
                        .collect::<Vec<_>>()
                ));
            }
        }
        for tool in ["qemu-system-x86_64", "qemu-img", "ssh"] {
            if which(tool).is_none() {
                return Some(format!("{tool} is not on PATH"));
            }
        }
        None
    }
}

/// Find an executable on `PATH`.
fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

impl Guest {
    /// Cut an overlay and boot a guest for `node`.
    pub fn boot(cluster: &Cluster, node: &Node, fixture: &Fixture) -> Result<Self, GuestError> {
        let fail = |attempted: &str, because: String| GuestError {
            node: node.name.clone(),
            attempted: attempted.to_string(),
            because,
        };

        std::fs::create_dir_all(&fixture.scratch)
            .map_err(|e| fail("create the scratch directory", e.to_string()))?;
        let disk = fixture.scratch.join(format!("{}.qcow2", node.name));

        // A copy-on-write overlay, not a copy. Three nodes cost one image plus
        // three deltas, which is what fits in the ~14 GB a hosted runner has.
        let overlay = Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                "-F",
                "qcow2",
                "-b",
                &fixture.backing.display().to_string(),
                &disk.display().to_string(),
            ])
            .output()
            .map_err(|e| fail("qemu-img create", e.to_string()))?;
        if !overlay.status.success() {
            return Err(fail(
                "qemu-img create",
                String::from_utf8_lossy(&overlay.stderr).trim().to_string(),
            ));
        }

        // A private, writable copy of the variable store. Sharing one between
        // guests would let the three nodes overwrite each other's boot entries,
        // and the template itself is usually read-only anyway.
        let vars = fixture.scratch.join(format!("{}-vars.fd", node.name));
        std::fs::copy(&fixture.firmware_vars, &vars)
            .map_err(|e| fail("copy the firmware variable store", e.to_string()))?;

        // The bulk device, on the guest that should discover itself as the
        // storage node (§2.3.1). Sparse: qcow2 allocates as it is written, so a
        // device above the threshold costs kilobytes until something uses it,
        // which matters on a runner with 14 GB free (§9.4).
        let bulk = if node.role
            == cluster
                .cluster
                .self_detected_role()
                .map(|r| r.id.as_str())
                .unwrap_or_default()
        {
            let path = fixture.scratch.join(format!("{}-bulk.qcow2", node.name));
            if !path.exists() {
                let gb = cluster.cluster.detection.bulk_disk_min_gb + 1;
                let made = Command::new("qemu-img")
                    .args([
                        "create",
                        "-f",
                        "qcow2",
                        &path.display().to_string(),
                        &format!("{gb}G"),
                    ])
                    .output()
                    .map_err(|e| fail("qemu-img create (bulk)", e.to_string()))?;
                if !made.status.success() {
                    return Err(fail(
                        "qemu-img create (bulk)",
                        String::from_utf8_lossy(&made.stderr).trim().to_string(),
                    ));
                }
            }
            Some(path.display().to_string())
        } else {
            None
        };

        let args = qemu_args(
            cluster,
            node,
            &disk.display().to_string(),
            &fixture.firmware_code.display().to_string(),
            &vars.display().to_string(),
            bulk.as_deref(),
        );
        let process = Command::new("qemu-system-x86_64")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| fail("spawn qemu", e.to_string()))?;

        Ok(Self {
            node: node.name.clone(),
            ssh_port: ssh_port(node),
            disk,
            process: Some(process),
        })
    }

    /// Wait until the guest answers SSH, or give up loudly.
    pub fn wait_for_ssh(&self, timeout_s: u64) -> Result<(), GuestError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
        let mut last = "no attempt completed".to_string();
        while std::time::Instant::now() < deadline {
            match self.exec("true") {
                Ok(_) => return Ok(()),
                Err(e) => last = e.because,
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        Err(GuestError {
            node: self.node.clone(),
            attempted: format!("wait {timeout_s}s for SSH"),
            because: format!("the guest never answered; last attempt said: {last}"),
        })
    }

    /// Run a command in the guest and return its stdout.
    ///
    /// A non-zero exit is an error, not an empty string. A harness that returned
    /// `""` for a failed command would let an assertion on absence pass because
    /// the command never ran.
    pub fn exec(&self, command: &str) -> Result<String, GuestError> {
        let output = Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "LogLevel=ERROR",
                "-p",
                &self.ssh_port.to_string(),
                "root@127.0.0.1",
                command,
            ])
            .output()
            .map_err(|e| GuestError {
                node: self.node.clone(),
                attempted: format!("ssh: {command}"),
                because: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(GuestError {
                node: self.node.clone(),
                attempted: format!("ssh: {command}"),
                because: format!(
                    "exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Whether a command succeeds, where failure is an answer rather than an
    /// error --- `getenforce` returning non-zero, a `ping` that does not reply.
    pub fn succeeds(&self, command: &str) -> bool {
        self.exec(command).is_ok()
    }

    /// Run the health predicate and return its report (§10.1).
    ///
    /// The same binary five consumers use, asked the same question: T1, T2 and
    /// T3 assert on exactly what greenboot and the rollout predicate read.
    pub fn health(&self) -> Result<cluster_health::Report, GuestError> {
        let json = self.exec("/usr/bin/cluster-health check")?;
        serde_json::from_str(&json).map_err(|e| GuestError {
            node: self.node.clone(),
            attempted: "parse the health report".to_string(),
            because: format!("{e}: {json}"),
        })
    }

    /// Stage an upgrade and reboot into it (§10.4).
    pub fn upgrade_to(&self, image: &str) -> Result<(), GuestError> {
        self.exec(&format!("bootc switch --retain {image}"))?;
        self.reboot()
    }

    /// Roll back to the previous deployment and reboot into it (§10.4).
    ///
    /// Tested in both directions because an untested rollback is not a recovery
    /// path, and §13 takes it with no operator present.
    pub fn rollback(&self) -> Result<(), GuestError> {
        self.exec("bootc rollback")?;
        self.reboot()
    }

    /// Reboot and wait for the guest to come back.
    pub fn reboot(&self) -> Result<(), GuestError> {
        // `systemctl reboot` drops the connection, so a non-zero exit here is
        // the expected outcome and is deliberately not treated as a failure.
        let _ = self.exec("systemctl reboot");
        std::thread::sleep(std::time::Duration::from_secs(5));
        self.wait_for_ssh(BOOT_TIMEOUT_S)
    }

    /// Detach a mesh link's device, to exercise §4.2's failover.
    ///
    /// The link goes away as it would if somebody pulled the cable: networkd
    /// withdraws the direct route on carrier loss and the transit route takes
    /// over. That is the whole of the failover mechanism, and detaching is how
    /// it gets exercised without unplugging anything.
    pub fn detach_link(
        &self,
        cluster: &Cluster,
        node: &Node,
        link: &str,
    ) -> Result<(), GuestError> {
        let netdev = mesh_netdevs(cluster, node)
            .into_iter()
            .find(|n| n.id == link)
            .ok_or_else(|| GuestError {
                node: self.node.clone(),
                attempted: format!("detach {link}"),
                because: "this node carries no such link".to_string(),
            })?;
        // Down the interface carrying it: from the routing layer's point of view
        // that is carrier loss, which is what §4.2 responds to.
        self.exec(&format!(
            "ip link set dev $(ls /sys/class/net | grep -v lo | sed -n {}p) down",
            // The netdevs are attached in model order, so the nth mesh link is
            // the nth non-loopback interface.
            netdev_index(cluster, node, &netdev.id)
        ))
        .map(|_| ())
    }

    /// Stop the guest.
    pub fn shutdown(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = self.exec("systemctl poweroff");
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

/// Which non-loopback interface a link is, 1-based, in the order QEMU attaches.
fn netdev_index(cluster: &Cluster, node: &Node, link: &str) -> usize {
    mesh_netdevs(cluster, node)
        .iter()
        .position(|n| n.id == link)
        .map(|at| at + 1)
        .unwrap_or(1)
}

impl Drop for Guest {
    /// A guest that outlived its test is a qemu process holding a port, and the
    /// next run would fail to bind with a message about nothing to do with the
    /// claim under test.
    fn drop(&mut self) {
        self.shutdown();
        let _ = std::fs::remove_file(&self.disk);
    }
}

/// Boot every node and wait for all of them (§10.3).
pub fn boot_mesh(cluster: &Cluster, fixture: &Fixture) -> Result<Vec<Guest>, GuestError> {
    // In ordinal order, because a socket netdev's listener must be up before its
    // connector attaches --- and the lower ordinal listens, being the one that
    // holds the even address of the /31 (§4.1).
    let mut guests = Vec::new();
    for node in cluster.nodes() {
        guests.push(Guest::boot(cluster, &node, fixture)?);
    }
    for guest in &guests {
        guest.wait_for_ssh(BOOT_TIMEOUT_S)?;
    }
    Ok(guests)
}

/// Fail a tier's test that has no fixture to run against.
///
/// Every tier test calls this, and none of them skips. Whether a tier runs at
/// all is decided once, by the driver (`cluster-harness <tier>`), and reported
/// in an exit status --- so a missing fixture stops the tier before a single
/// test reports anything. Reaching this panic means the driver admitted the run
/// and the fixture vanished underneath it, which is worth failing over.
pub fn require_bootable(tier: &str, fixture: &Fixture) {
    if let Some(reason) = fixture.missing() {
        panic!("{tier}: {} ({reason})", Acceleration::SKIP_NOTICE);
    }
}
