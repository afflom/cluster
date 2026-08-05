//! The helpers rendered units invoke (`SPEC.md` §5.5, §9.5, §14.1).
//!
//! Two scripts, both rendered rather than written, because both encode model
//! facts: a runner's labels and whether it is ephemeral, and the ages at which
//! registry storage is collected. A hand-written copy of either would be a
//! second source for something `model/` already declares.
//!
//! # Why the runner loop is a script and not a daemon
//!
//! `--ephemeral` runners exit after one job. Draining is therefore a matter of
//! *not re-registering* rather than of killing work (§14.1), and the loop that
//! re-registers has to be interruptible between jobs and uninterruptible during
//! one. A systemd unit restarting a one-shot registration gives exactly that:
//! stopping the unit lets the current job finish and takes the runner out of
//! rotation, which is what a drain wants and what a long-lived daemon would make
//! hard.

use crate::render::{node_path, Rendered};
use crate::{Cluster, Role};

pub(crate) fn render(c: &Cluster) -> Vec<Rendered> {
    let mut out = vec![sshrc(c)];
    // One script per role that hosts runners, all of them shipped on every
    // machine. The unit that invokes one is gated on the role marker, so the
    // script a machine never runs is a few hundred bytes it never reads (§8.4).
    for role in &c.cluster.role {
        let Some(variant) = c.images.variant_for(&role.id) else {
            continue;
        };
        if !variant.runner.is_empty() {
            out.push(runner_loop(c, role));
        }
    }
    out.push(zot_gc(c));
    out
}

/// Record an attachment on every SSH connection (§15.1).
///
/// `last_attached_at` drives every reclamation threshold, and §15.1 says it is
/// updated on every SSH connection and every tunnel attachment. Without this
/// half a developer who works entirely over `ssh dc-<id>` looks idle to §15.3 and
/// gets archived out from under themselves --- which is the failure the whole
/// dirty exemption exists downstream of.
///
/// `sshrc` runs for every session, non-interactive included, which is why it is
/// this and not a profile script: `scp`, `rsync` and a VS Code server starting
/// are all attachments and none of them sources a profile.
fn sshrc(c: &Cluster) -> Rendered {
    let control = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");

    let body = format!(
        "# Runs for every SSH session, interactive or not (§15.1).\n\
         #\n\
         # scp, rsync and a VS Code server starting are all attachments, and none of\n\
         # them sources a profile --- which is why this is sshrc and not profile.d.\n\
         #\n\
         # Failure is ignored on purpose. A control plane that is rebooting must not\n\
         # stop somebody logging in; the cost is one attachment not recorded, and\n\
         # §15.3's thresholds are in days (§16.5).\n\
         if [ -n \"${{CLUSTER_SESSION:-}}\" ]; then\n  \
           /usr/bin/curl --silent --fail --max-time 2 --request POST \\\n    \
             \"http://{}:8080/api/sessions/${{CLUSTER_SESSION}}/attached\" >/dev/null 2>&1 || true\n\
         fi\n",
        control.loopback
    );

    Rendered::new(node_path("sshrc"), vec!["CD-12", "CG-07"], body)
}

/// Register one ephemeral runner, take one job, exit.
fn runner_loop(c: &Cluster, role: &Role) -> Rendered {
    let variant = c
        .images
        .variant_for(&role.id)
        .expect("the caller checked there is a variant");
    let concurrency = variant.runner.iter().filter_map(|r| r.concurrency).min();
    // Across every variant, not this one's: there is one image and it carries
    // the union of what the roles need (§8.4). The runner is declared once, on
    // the variant that documents it, and both roles that host runners use it.
    let install_dir = c
        .images
        .runner_install_dir()
        .expect("check_images requires the runner to be declared where a role hosts one");

    let mut body = String::new();
    body.push_str(&format!(
        "#!/usr/bin/env bash\n\
         # One ephemeral runner registration for the `{}` role, one job at a time.\n\
         #\n\
         # The unit restarts this after every job, which is what makes draining a\n\
         # matter of not re-registering rather than of killing work: stopping the\n\
         # unit lets the job in flight finish and takes the runner out of rotation\n\
         # (§14.1).\n\
         set -euo pipefail\n\n",
        role.id
    ));

    body.push_str(&format!(
        "# RUNNER_NAME, RUNNER_LABELS and RUNNER_URL come from the unit, which is\n\
         # rendered from the model. The credential is the one thing that is not:\n\
         # it is enrolled through the browser after the cluster boots and lands\n\
         # in a file only root can read, so no token is in an image, in this\n\
         # repository, or in `systemctl show` (§12.2).\n\
         : \"${{RUNNER_NAME:?the unit sets this}}\"\n\
         : \"${{RUNNER_LABELS:?the unit sets this}}\"\n\
         : \"${{RUNNER_URL:?the unit sets this}}\"\n\
         pat=$(cat {pat})\n\n",
        pat = crate::render::RUNNER_PAT
    ));

    if let Some(limit) = concurrency {
        body.push_str(&format!(
            "# One measurement at a time, nothing else (§9.5). A flock rather than a\n\
             # convention in a workflow, because a convention is not observable from\n\
             # the node --- and §18's bench-contention alert is about exactly this.\n\
             exec {{lock}}>/var/lock/cluster-runner\n\
             flock --nonblock --exclusive \"$lock\" || {{\n  \
               echo \"runner: {limit} job(s) already running; not registering\" >&2\n  \
               exit 0\n\
             }}\n\n"
        ));
    }

    body.push_str(&format!(
        "# The runner's own copy. `config.sh` writes its registration beside\n\
         # itself, and {install} is on the read-only system tree --- so each\n\
         # runner works from a copy under /var, which the bootc filesystem\n\
         # contract makes writable and persistent (§5.2).\n\
         work=/var/lib/cluster-runner/$RUNNER_NAME\n\
         if [ ! -x \"$work/config.sh\" ]; then\n  \
           mkdir -p \"$work\"\n  \
           /usr/bin/rsync --archive {install}/ \"$work/\"\n\
         fi\n\
         cd \"$work\"\n\n\
         # Mint a registration token, every time round.\n\
         #\n\
         # A registration token expires in an hour and an ephemeral runner\n\
         # re-registers after every job, so a token written to a node once would\n\
         # work until lunchtime and then stop, on a machine nobody is watching.\n\
         # What is enrolled is a credential that can mint them; this is where it\n\
         # is spent (§9.5, §12.2).\n\
         token=$(/usr/bin/curl --silent --show-error --fail --max-time 30 \\\n  \
           --request POST \\\n  \
           --header \"authorization: Bearer $pat\" \\\n  \
           --header \"accept: application/vnd.github+json\" \\\n  \
           \"https://api.github.com/repos/{repo}/actions/runners/registration-token\" \\\n  \
           | /usr/bin/jq -r .token)\n\
         [ -n \"$token\" ] && [ \"$token\" != null ] || {{\n  \
           echo \"runner: the enrolled credential could not mint a registration token\" >&2\n  \
           exit 1\n\
         }}\n\n\
         # A fresh registration each time. An ephemeral runner is removed by the\n\
         # service after its job, so the previous registration is already gone.\n\
         ./config.sh \\\n  \
           --url \"$RUNNER_URL\" \\\n  \
           --token \"$token\" \\\n  \
           --name \"$RUNNER_NAME\" \\\n  \
           --labels \"$RUNNER_LABELS\" \\\n  \
           --unattended --replace{ephemeral}\n\n\
         # Blocks until one job completes, then exits. The unit restarts us.\n\
         exec ./run.sh\n",
        install = install_dir,
        repo = c.images.signing.repository,
        ephemeral = if variant.runner.iter().all(|r| r.ephemeral) {
            " \\\n  --ephemeral"
        } else {
            ""
        }
    ));

    Rendered::new(
        node_path(format!("libexec/runner-loop-{}", role.id)),
        vec!["CD-12", "CW-02", "CD-22"],
        body,
    )
}

/// Collect registry storage (§5.5).
fn zot_gc(c: &Cluster) -> Rendered {
    let gc = &c.policy.gc;
    let host = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared role");
    let registry = format!("{}:{}", host.loopback, c.images.registries.port);

    let body = format!(
        "#!/usr/bin/env bash\n\
         # Registry collection: untagged manifests older than {} days go.\n\
         #\n\
         # Tagged manifests never do. `:stable` is what every node follows, and a\n\
         # collection that removed the digest a node had not yet pulled would turn\n\
         # §14.2's window into a node that cannot update at all (§5.5).\n\
         set -euo pipefail\n\n\
         # Zot collects on its own schedule from the gcDelay in its configuration,\n\
         # which is rendered from the same threshold. This triggers it rather than\n\
         # reimplementing it: the registry owns its storage, and a second collector\n\
         # walking the same blobs is a second source for what is garbage.\n\
         curl --silent --show-error --fail \\\n  \
           --request POST \"http://{registry}/v2/_zot/ext/gc\" \\\n  \
           || echo 'zot-gc: the registry declined; it collects on its own schedule' >&2\n\n\
         # Container storage on this node, which Zot does not own.\n\
         /usr/bin/podman system prune --force --filter until={}h\n",
        gc.registry_untagged_max_age_days, gc.container_image_max_age_h
    );

    Rendered::new(node_path("libexec/zot-gc"), vec!["CD-12"], body)
}
