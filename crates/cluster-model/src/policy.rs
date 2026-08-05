//! The typed shape of `model/policy.toml` (`SPEC.md` §7.1).
//!
//! Every tunable that governs unattended behaviour. Nothing here is a claim:
//! the claims about what these numbers *cause* live in `model/ids.toml`, and
//! this file is only the values.

use serde::Deserialize;

/// `model/policy.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyFile {
    /// The schema tag.
    pub spec: String,
    /// Polling, registries, and the ordering predicate's timings (§13).
    pub rollout: Rollout,
    /// The boot-success deadline and its check path (§13.3).
    pub greenboot: Greenboot,
    /// One row per workload class (§14.4).
    pub drain_budget: Vec<DrainBudget>,
    /// Migration target, capacity cap, and what never receives work (§14.3).
    pub drain: Drain,
    /// Idle thresholds and the dirty exemption (§15.3).
    pub reclaim: Reclaim,
    /// Retention (§5.5).
    pub gc: Gc,
    /// One row per alert (§18).
    pub alert: Vec<Alert>,
    /// Control-plane authorization (§16.2).
    pub auth: Auth,
    /// The devcontainer tunnel (§11.1).
    pub tunnel: Tunnel,
    /// The health predicate's own thresholds (§10.1).
    pub health: Health,
    /// The secrets a booted cluster is given through the browser (§12.2).
    pub secret: Vec<Secret>,
}

/// One secret the operator enrols after the cluster boots (§12.2).
///
/// The row says where a value goes, never what it is. None of these is in this
/// repository or in the image: they arrive once, through the browser client,
/// authenticated by the GitHub App device flow --- the one credential that can
/// be checked without any of the others existing.
#[derive(Debug, Clone, Deserialize)]
pub struct Secret {
    /// Stable identifier, used by the API and the form.
    pub id: String,
    /// What it is, for the operator entering it.
    pub description: String,
    /// Where the value lands on the node. Empty when applying it consumes the
    /// value and keeping it would be a liability rather than a convenience.
    pub path: String,
    /// The mode the file takes, as an octal string.
    pub mode: String,
    /// What happens after it is written: `none`, or a named action.
    pub apply: String,
    /// How the entered value becomes the file: `raw` or `docker-auth`.
    ///
    /// A separate question from [`Secret::path`], and the one that was missing.
    /// An operator enters a credential; most destinations want that credential
    /// and one wants a document built around it. `/etc/containers/auth.json` is
    /// parsed by podman as JSON, so a bare token written there fails every pull
    /// --- unattended, at the next update, a long way from where it was typed.
    pub format: String,
    /// Which registry a `docker-auth` document is keyed by. Empty otherwise.
    pub registry: String,
    /// What stays down until it is given, in the operator's terms.
    pub enables: String,
}

/// How an entered value becomes the file at its destination (§12.2).
pub const SECRET_FORMATS: &[&str] = &["raw", "docker-auth"];

impl Secret {
    /// Whether the value is written to a file at all.
    ///
    /// A Tailscale auth key is redeemed by the command that applies it, and a
    /// redeemed key is of no further use to this node and of some use to whoever
    /// finds it. Not storing it is the point rather than an omission.
    pub fn is_stored(&self) -> bool {
        !self.path.is_empty()
    }

    /// The mode as a number, or `None` when the model does not spell one.
    pub fn mode_bits(&self) -> Option<u32> {
        u32::from_str_radix(self.mode.trim_start_matches("0o"), 8).ok()
    }

    /// The format as the rendered policy spells it.
    ///
    /// `raw`, or `docker-auth@<registry>`. The registry rides in the same field
    /// behind an `@` because a registry may carry a port, and a colon-separated
    /// row cannot then hold it as a field of its own.
    pub fn rendered_format(&self) -> String {
        if self.registry.is_empty() {
            self.format.clone()
        } else {
            format!("{}@{}", self.format, self.registry)
        }
    }
}

/// Polling and the ordering predicate's timings (§13.1, §13.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Rollout {
    /// How often a node asks what `:stable` resolves to.
    pub poll_interval_s: u64,
    /// Upper bound of the random delay added to each poll. Jitter is what makes
    /// a simultaneous stale read unlikely rather than merely improbable in
    /// theory (§13.2).
    pub poll_jitter_max_s: u64,
    /// How long before committing to the upgrade the predicate is re-evaluated.
    pub recheck_before_apply_s: u64,
    /// How long a peer's health may be stale before it counts as unknown ---
    /// which halts, and is not the same as unhealthy.
    pub peer_health_timeout_s: u64,
    /// Where `cluster-health` is served on the mesh loopback (§10.1).
    pub peer_health_port: u16,
    /// Registry order: local Zot first, GHCR on failure --- which it is, by
    /// design, during the storage node's own reboot (§5.4).
    pub registries: Vec<String>,
    /// Where the node images live.
    pub image_repository: String,
    /// The tag whose digest a node follows.
    pub stable_tag: String,
}

/// The boot-success deadline (§13.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Greenboot {
    /// The boot is declared successful only if `cluster-health` passes within
    /// this many seconds; on failure greenboot rolls back automatically.
    pub deadline_s: u64,
    /// Where the required check is installed.
    pub check_path: String,
    /// Beyond this count the previous deployment stands.
    pub max_boot_attempts: u32,
}

/// One workload class's drain budget (§14.4).
///
/// A budget is never met by force. Exceeding one halts the rollout and asks for
/// a human, because the alternative --- killing a four-hour benchmark to
/// install a patch release --- is worse than staying on the old image.
#[derive(Debug, Clone, Deserialize)]
pub struct DrainBudget {
    /// `bench-job`, `ci-job`, `devcontainer-migration`, or `total`.
    pub class: String,
    /// The per-item budget.
    pub budget_s: u64,
    /// The budget across every item of this class, where one applies.
    #[serde(default)]
    pub total_budget_s: Option<u64>,
    /// `halt` or `stop-with-notice`.
    pub on_exceed: String,
    /// Why the budget is what it is.
    #[serde(default)]
    pub comment: String,
}

/// Migration target and capacity (§14.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Drain {
    /// Beyond this cap the excess is stopped with notice rather than migrated
    /// --- the session survives, the process does not.
    pub migration_memory_cap_gib: u32,
    /// The only node that can receive devcontainers (§1.1).
    pub migration_target: String,
    /// Nodes that receive no migrated workload under any circumstance.
    /// Receiving work would void the isolation guarantee the testbed exists to
    /// provide (§2.3).
    pub never_receives: Vec<String>,
    /// The grace period a container is stopped with.
    pub container_stop_grace_s: u64,
}

/// Idle thresholds, and the exemption that is the point of the policy (§15.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Reclaim {
    /// Age at which the owner is notified and the session is marked idle.
    pub notify_after_days: u32,
    /// Age at which the session is snapshotted and archived. Reversible.
    pub archive_after_days: u32,
    /// Age at which the archive is deleted. Not reversible.
    pub purge_after_days: u32,
    /// Whether a dirty workspace may be purged. It may not: deleting someone's
    /// uncommitted work because a timer expired is a betrayal, and a system
    /// that does it once is never trusted again.
    pub purge_dirty: bool,
    /// How often reclamation runs.
    pub schedule: String,
    /// Reclamation never runs during a rollout, so that a session archived
    /// because it was idle is never confused with one stopped because its host
    /// was updating (§15.4).
    pub suspend_during_rollout: bool,
    /// What takes the snapshot.
    pub snapshot_tool: String,
    /// Where snapshots live.
    pub snapshot_repository: String,
}

/// Retention (§5.5).
#[derive(Debug, Clone, Deserialize)]
pub struct Gc {
    /// How often garbage collection runs.
    pub schedule: String,
    /// `podman system prune --filter until=`.
    pub container_image_max_age_h: u64,
    /// How long an untagged manifest survives a Zot GC.
    pub registry_untagged_max_age_days: u32,
    /// `SystemMaxUse=` for journald.
    pub journald_max_use: String,
    /// Prometheus retention, accepted as lossy (§5.6).
    pub prometheus_retention_days: u32,
    /// Current and rollback, enforced by bootc.
    pub ostree_deployments_retained: u32,
    /// The only irreplaceable artifact the cluster produces, and it is small.
    pub measurement_output_pruned: bool,
}

/// One alert (§18).
#[derive(Debug, Clone, Deserialize)]
pub struct Alert {
    /// Stable identifier.
    pub id: String,
    /// What fires it, in the register's words.
    pub condition: String,
    /// How long the condition must hold. Zero means immediately.
    pub for_s: u64,
    /// `critical`, `warning`, or `info`.
    pub severity: String,
}

/// Control-plane authorization (§16.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Auth {
    /// How long a validated token's login is cached before `GET /user` is called
    /// again. Revocation lag is bounded by this and it is a real window;
    /// shortening it costs a round trip on every request instead.
    pub token_cache_ttl_s: u64,
    /// How long a call to the identity provider may take before giving up. A
    /// control plane blocking on a slow validation would make an unreachable
    /// GitHub look like an unreachable the storage node.
    pub validation_timeout_s: u64,
    /// The exact origin the Pages copy is served from.
    ///
    /// Exact rather than a wildcard: this API drives a cluster, and `*` would
    /// let any page a browser visits use a token that browser already holds.
    pub allowed_origin: String,
}

/// The devcontainer tunnel (§11.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Tunnel {
    /// How long the supervisor waits before its first restart.
    pub restart_backoff_initial_s: u64,
    /// The ceiling it backs off to. Backoff rather than a tight loop: a tunnel
    /// failing because its token expired would otherwise spin against the
    /// identity provider until something rate-limited it.
    pub restart_backoff_max_s: u64,
    /// The prefix a session's tunnel name takes, matching the SSH alias so one
    /// identifier addresses a session on both paths.
    pub name_prefix: String,
    /// How a tunnel name becomes a URL.
    ///
    /// Deliberately free of any host component: that is what makes the URL
    /// survive a migration, which is the property the tunnel path was chosen
    /// for (§14.3).
    pub url_template: String,
    /// What a forwarded port defaults to. Private: a development server bound
    /// inside a container is not a thing to publish by accident.
    pub default_port_visibility: String,
}

impl Tunnel {
    /// The tunnel name for a session.
    pub fn name_for(&self, session: &str) -> String {
        format!("{}{session}", self.name_prefix)
    }

    /// The URL a session is addressed by.
    ///
    /// A pure function of the session id and the folder. No node appears in it
    /// and none can: the container re-registers under the same name wherever it
    /// lands, so the URL is unchanged by a migration (§14.3).
    pub fn url_for(&self, session: &str, folder: &str) -> String {
        self.url_template
            .replace("{name}", &self.name_for(session))
            .replace("{folder}", folder)
    }
}

/// The health predicate's thresholds (§10.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    /// Where the predicate is served on the mesh loopback.
    pub port: u16,
    /// 9000 minus 20 bytes of IP header and 8 of ICMP, so the probe fails if
    /// anything on the path is not carrying jumbo frames.
    pub mesh_mtu_probe_bytes: u32,
    /// Above this the clock check fails.
    pub chrony_max_offset_ms: u64,
}

impl PolicyFile {
    /// Look up a drain budget by class.
    pub fn budget(&self, class: &str) -> Option<&DrainBudget> {
        self.drain_budget.iter().find(|b| b.class == class)
    }

    /// Look up an alert by id.
    pub fn alert(&self, id: &str) -> Option<&Alert> {
        self.alert.iter().find(|a| a.id == id)
    }
}
