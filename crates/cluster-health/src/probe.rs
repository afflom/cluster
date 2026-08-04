//! Gathering what the predicate is evaluated against (`SPEC.md` §10.1).
//!
//! Everything here shells out. That is deliberate: `systemctl`, `bootc`,
//! `getenforce` and `chronyc` are the authorities on their own state, and
//! reimplementing any of them --- parsing D-Bus for unit state, reading
//! `/ostree` for the deployment --- would give this repository a second source
//! for a fact the system already answers.
//!
//! Every failure to *run* a probe becomes a [`ProbeError`], which is the one
//! error R5 sanctions for this crate. No `std::io::Result` reaches a caller: an
//! I/O error here is not information a caller can act on, and "the clock probe
//! could not be executed" is.

use std::process::Command;

use crate::{ProbeError, State};

/// What the predicate is evaluated against.
///
/// A plain data structure with no behaviour, so that a test can construct one
/// directly and the predicate can be exercised without a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observations {
    /// `systemctl is-system-running`.
    pub system_state: String,
    /// Units `systemctl --failed` names.
    pub failed_units: Vec<String>,
    /// The digest `bootc status --json` reports as booted.
    pub booted_digest: String,
    /// What `:stable` resolved to, when this node has looked.
    pub target_digest: Option<String>,
    /// This node's rollout state (§13.2).
    pub state: State,
    /// Peer loopbacks that answered at the full MTU.
    pub mesh_reachable: Vec<String>,
    /// Units `systemctl` reports as active.
    pub active_units: Vec<String>,
    /// Whether `/usr` is mounted read-only.
    pub usr_read_only: bool,
    /// Whether `/var` is writable.
    pub var_writable: bool,
    /// Whether `getenforce` says `Enforcing`.
    pub selinux_enforcing: bool,
    /// AVC denials in the audit log since boot.
    pub avc_denials: u32,
    /// Whether chrony has a synchronised source.
    pub chrony_synchronised: bool,
    /// The current offset.
    pub clock_offset_ms: u64,
}

/// Where observations come from.
///
/// A trait so the predicate can be driven from a fixture in a test and from the
/// machine in production, without the predicate knowing which.
pub trait Probe {
    /// Gather everything §10.1 declares.
    ///
    /// `peers` are the loopbacks to probe and `probe_bytes` the ICMP payload
    /// that fails if the path is not carrying jumbo frames.
    fn observe(&self, peers: &[String], probe_bytes: u32) -> Result<Observations, ProbeError>;
}

/// The real probe: the system, asked about itself.
#[derive(Debug, Clone, Default)]
pub struct SystemProbe {
    /// This node's rollout state, which the updater writes and the probe
    /// reports. It is state about an operation in flight, not about the
    /// machine, so it is passed in rather than discovered.
    pub state: State,
    /// What `:stable` last resolved to, when the updater has looked.
    pub target_digest: Option<String>,
}

impl Probe for SystemProbe {
    fn observe(&self, peers: &[String], probe_bytes: u32) -> Result<Observations, ProbeError> {
        // `is-system-running` exits non-zero when the answer is `degraded`,
        // which is an answer and not a failure --- so the exit status is
        // deliberately ignored and only the output is read.
        let system_state =
            run_allowing_failure("system-running", "systemctl", &["is-system-running"])?;

        let failed_units = lines_of(&run_allowing_failure(
            "no-failed-units",
            "systemctl",
            &[
                "list-units",
                "--failed",
                "--plain",
                "--no-legend",
                "--no-pager",
            ],
        )?)
        .into_iter()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();

        let status = run("booted-digest", "bootc", &["status", "--json"])?;
        let booted_digest = booted_digest_from(&status).ok_or_else(|| ProbeError {
            check: "booted-digest",
            attempted: "bootc status --json".to_string(),
            because: "the status document names no booted image digest".to_string(),
        })?;

        let active_units = lines_of(&run_allowing_failure(
            "quadlets-active",
            "systemctl",
            &[
                "list-units",
                "--state=active",
                "--plain",
                "--no-legend",
                "--no-pager",
            ],
        )?)
        .into_iter()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();

        // `ping -M do` sets DF, so a path that has silently dropped below the
        // mesh MTU reports "message too long" rather than fragmenting and
        // looking fine (§10.1).
        let mut mesh_reachable = Vec::new();
        for peer in peers {
            let size = probe_bytes.to_string();
            let reached = Command::new("ping")
                .args(["-c", "1", "-W", "2", "-M", "do", "-s", &size, peer])
                .output()
                .map_err(|e| ProbeError {
                    check: "mesh-reachable",
                    attempted: format!("ping -M do -s {size} {peer}"),
                    because: e.to_string(),
                })?;
            if reached.status.success() {
                mesh_reachable.push(peer.clone());
            }
        }

        let mounts = read("filesystem-contract", "/proc/mounts")?;
        let usr_read_only = mount_is_read_only(&mounts, "/usr");
        // `/var` is writable by the bootc filesystem contract (§5.2), and the
        // check is a probe rather than an inference because a full or
        // remounted-read-only `/var` is exactly the failure that leaves
        // container graph storage silently broken.
        let var_writable = !mount_is_read_only(&mounts, "/var");

        let selinux_enforcing = run("selinux-enforcing", "getenforce", &[])?.trim() == "Enforcing";
        let avc_denials = lines_of(&run_allowing_failure(
            "selinux-enforcing",
            "journalctl",
            &["--boot", "--grep=avc:  denied", "--no-pager", "--quiet"],
        )?)
        .len() as u32;

        let tracking = run("clock-synchronised", "chronyc", &["tracking"])?;
        let chrony_synchronised = !tracking.contains("Not synchronised")
            && !tracking.contains("Leap status     : Not synchronised");
        let clock_offset_ms = system_time_offset_ms(&tracking).ok_or_else(|| ProbeError {
            check: "clock-synchronised",
            attempted: "chronyc tracking".to_string(),
            because: "no `System time` line in the output".to_string(),
        })?;

        Ok(Observations {
            system_state: system_state.trim().to_string(),
            failed_units,
            booted_digest,
            target_digest: self.target_digest.clone(),
            state: self.state,
            mesh_reachable,
            active_units,
            usr_read_only,
            var_writable,
            selinux_enforcing,
            avc_denials,
            chrony_synchronised,
            clock_offset_ms,
        })
    }
}

/// Run a command that must succeed.
fn run(check: &'static str, program: &str, args: &[&str]) -> Result<String, ProbeError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| ProbeError {
            check,
            attempted: format!("{program} {}", args.join(" ")),
            because: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(ProbeError {
            check,
            attempted: format!("{program} {}", args.join(" ")),
            because: format!(
                "exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run a command whose non-zero exit is itself an answer.
///
/// `systemctl is-system-running` exits 1 when the system is `degraded`. Treating
/// that as an unrunnable probe would turn the most common real failure into an
/// unknown, and §13.2 halts on unknowns --- so a degraded node would stall the
/// rollout instead of reporting itself unhealthy.
fn run_allowing_failure(
    check: &'static str,
    program: &str,
    args: &[&str],
) -> Result<String, ProbeError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| ProbeError {
            check,
            attempted: format!("{program} {}", args.join(" ")),
            because: e.to_string(),
        })?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn read(check: &'static str, path: &str) -> Result<String, ProbeError> {
    std::fs::read_to_string(path).map_err(|e| ProbeError {
        check,
        attempted: format!("read {path}"),
        because: e.to_string(),
    })
}

fn lines_of(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// The booted image digest from `bootc status --json`.
///
/// Parsed with a string scan rather than a JSON dependency: this crate ships in
/// every image, and one field of one document does not justify pulling a parser
/// into the base of the fleet.
pub fn booted_digest_from(status: &str) -> Option<String> {
    let key = "\"imageDigest\"";
    let at = status.find(key)?;
    let rest = &status[at + key.len()..];
    let open = rest.find('"')?;
    let after = &rest[open + 1..];
    let close = after.find('"')?;
    Some(after[..close].to_string())
}

/// Whether a mount point carries `ro` in its options.
pub fn mount_is_read_only(mounts: &str, target: &str) -> bool {
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(_source) = fields.next() else {
            return false;
        };
        let Some(mount_point) = fields.next() else {
            return false;
        };
        let Some(_fstype) = fields.next() else {
            return false;
        };
        let Some(options) = fields.next() else {
            return false;
        };
        mount_point == target && options.split(',').any(|o| o == "ro")
    })
}

/// The absolute `System time` offset from `chronyc tracking`, in milliseconds.
pub fn system_time_offset_ms(tracking: &str) -> Option<u64> {
    for line in tracking.lines() {
        let Some(rest) = line.trim().strip_prefix("System time") else {
            continue;
        };
        let value = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        let seconds: f64 = value.split_whitespace().next()?.parse().ok()?;
        return Some((seconds.abs() * 1000.0).round() as u64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_booted_digest_is_read_from_the_status_document() {
        let status = r#"{"status":{"booted":{"image":{"imageDigest":"sha256:abc123"}}}}"#;
        assert_eq!(booted_digest_from(status).as_deref(), Some("sha256:abc123"));
        assert_eq!(booted_digest_from("{}"), None);
    }

    #[test]
    fn a_read_only_mount_is_recognised() {
        let mounts = "\
/dev/nvme0n1p3 /usr xfs ro,relatime 0 0
/dev/nvme0n1p3 /var xfs rw,relatime 0 0
";
        assert!(mount_is_read_only(mounts, "/usr"));
        assert!(!mount_is_read_only(mounts, "/var"));
        // A mount point that is not listed is not read-only, and the caller
        // treats an absent `/usr` as a failed contract by way of the pairing in
        // the predicate rather than by a default here.
        assert!(!mount_is_read_only(mounts, "/boot"));
    }

    /// `rw` must not be matched by a substring test: `relatime` contains no
    /// `ro`, but a naive `contains("ro")` would find one in `errors=remount-ro`.
    #[test]
    fn mount_options_are_matched_whole() {
        let mounts = "/dev/sda1 /var ext4 rw,errors=remount-ro 0 0\n";
        assert!(!mount_is_read_only(mounts, "/var"));
    }

    #[test]
    fn a_clock_offset_is_read_in_milliseconds() {
        let tracking = "\
Reference ID    : C0A81401 (gateway)
System time     : 0.000305108 seconds slow of NTP time
Leap status     : Normal
";
        assert_eq!(system_time_offset_ms(tracking), Some(0));

        let drifted = "System time     : 1.250000000 seconds fast of NTP time\n";
        assert_eq!(system_time_offset_ms(drifted), Some(1250));

        // A negative offset is still an offset: the check is on magnitude.
        let behind = "System time     : -0.400000000 seconds\n";
        assert_eq!(system_time_offset_ms(behind), Some(400));

        assert_eq!(system_time_offset_ms("Leap status : Normal"), None);
    }
}
