//! Host policy the base image applies (`SPEC.md` §8.1, §8.3, §13.3).
//!
//! These were hard-coded in the base Containerfile, which gave `model/images.toml`
//! and the build two sources for the same three decisions --- exactly what R1
//! forbids, and quietly: an `sshd` that accepted passwords would have satisfied a
//! model that said it did not.
//!
//! The greenboot configuration is here for a different reason. §13.3 gives it a
//! deadline *and* a boot-attempt count, and the count is what bounds how many
//! times a bad image reboots before the previous deployment stands. A deadline
//! rendered without the count is a rollback that might loop.

use crate::render::{section, Rendered};
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    vec![
        sshd(c, node),
        selinux(c, node),
        greenboot(c, node),
        hosts_unit(c, node),
        nftables_dropin(c, node),
    ]
}

/// Install the rendered hosts file at boot (`SPEC.md` §4.3, §5.2).
///
/// # Why this is a unit and not a `COPY`
///
/// `/etc/hosts` cannot be written by a container build. A `RUN` that tries fails
/// --- the runtime bind-mounts the path, so `install` reports the file busy ---
/// and a `COPY` to it is **silently dropped**, which is worse: the image builds
/// clean and ships without the file. Both were observed before this unit
/// existed.
///
/// So the content is shipped where nothing special-cases it, under
/// `/usr/lib/cluster/`, and placed at boot. The value is still a property of the
/// image: the unit copies, it does not compute, and the file it copies is
/// diff-gated like everything else under `generated/`.
///
/// Ordered before `network-pre.target` because every name this cluster resolves
/// is in that file, and a service that started first would resolve a peer
/// through the upstream resolver or not at all.
fn hosts_unit(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# Places {}'s rendered hosts file (§4.3).\n\
         #\n\
         # /etc/hosts cannot be written by a container build: a RUN finds the path\n\
         # bind-mounted and busy, and a COPY to it is silently dropped --- the image\n\
         # builds clean and ships without the file. Both were observed. The content\n\
         # therefore ships at /usr/lib/cluster/hosts, where nothing special-cases\n\
         # it, and is placed here.\n\
         #\n\
         # This is the one thing this repository writes into /etc at runtime, and\n\
         # it is a copy of an image file rather than anything computed on the node:\n\
         # the three-way merge on update sees a value that only ever changes when\n\
         # the image changes (§5.2).\n\n",
        node.name
    ));
    body.push_str(&section(
        "Unit",
        &[
            "Description=Place the rendered hosts file".to_string(),
            // Every name this cluster resolves lives in that file. Anything
            // that started first would reach a peer through the upstream
            // resolver, or not at all.
            "Before=network-pre.target nss-lookup.target".to_string(),
            "Wants=network-pre.target".to_string(),
            "DefaultDependencies=no".to_string(),
            "After=local-fs.target".to_string(),
            "ConditionPathExists=/usr/lib/cluster/hosts".to_string(),
        ],
    ));
    body.push_str(&section(
        "Service",
        &[
            "Type=oneshot".to_string(),
            "RemainAfterExit=yes".to_string(),
            "ExecStart=/usr/bin/install -m 0644 /usr/lib/cluster/hosts /etc/hosts".to_string(),
        ],
    ));
    body.push_str(&section(
        "Install",
        &["WantedBy=sysinit.target".to_string()],
    ));
    let _ = c;
    Rendered::new(
        format!("{}/systemd/cluster-hosts.service", node.name),
        vec!["CD-04", "CD-14"],
        body,
    )
}

/// Point `nftables.service` at the rendered ruleset (§4.4).
///
/// A drop-in rather than a copy into `/etc/sysconfig/`: the ruleset is an image
/// file and reading it from `/usr` keeps it that way. One fewer thing in `/etc`
/// is one fewer thing the update merge has an opinion about (§5.2).
fn nftables_dropin(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();
    body.push_str(&format!(
        "# {} loads its ruleset from the image rather than from /etc (§4.4).\n\
         #\n\
         # The rendered file is diff-gated under generated/; reading it where it was\n\
         # shipped means the running ruleset and the reviewed one cannot differ by\n\
         # anything that happened on the node.\n\n",
        node.name
    ));
    body.push_str(&section(
        "Service",
        &[
            // Cleared first: a drop-in appends, and without this the packaged
            // ExecStart would run too and load whatever /etc/sysconfig held.
            "ExecStart=".to_string(),
            "ExecStart=/usr/sbin/nft -f /usr/lib/cluster/nftables.conf".to_string(),
        ],
    ));
    let _ = c;
    Rendered::new(
        format!("{}/systemd/nftables.service.d/10-cluster.conf", node.name),
        vec!["CD-03", "CD-14"],
        body,
    )
}

/// Key-only SSH (§8.1).
///
/// A password-accepting `sshd` on a node that reboots unattended is an
/// invitation, and there is no operator present to notice.
fn sshd(c: &Cluster, node: &Node) -> Rendered {
    let s = &c.images.base.sshd;
    let no = |allowed: bool| if allowed { "yes" } else { "no" };

    let body = format!(
        "# Key-only SSH on {}. A password-accepting sshd on a node that reboots\n\
         # unattended is an invitation, and there is no operator present to notice\n\
         # (§8.1).\n\
         #\n\
         # Rendered rather than written into the Containerfile: the model declares\n\
         # these, and a build that also declared them would give one decision two\n\
         # sources --- with the failure being an sshd that accepts passwords under a\n\
         # model that says it does not.\n\n\
         PasswordAuthentication {}\n\
         KbdInteractiveAuthentication {}\n\
         PermitRootLogin {}\n",
        node.name,
        no(s.password_authentication),
        no(s.kbd_interactive_authentication),
        s.permit_root_login
    );
    Rendered::new(
        format!("{}/sshd_config.d/10-cluster.conf", node.name),
        vec!["CD-14"],
        body,
    )
}

/// SELinux, enforcing, and it stays that way (§8.3).
fn selinux(c: &Cluster, node: &Node) -> Rendered {
    let s = &c.images.base.selinux;
    let body = format!(
        "# SELinux on {}. Enforcing, targeted, and it stays that way (§8.3).\n\
         #\n\
         # The custom module is compiled at {} time and shipped in\n\
         # /usr/share/selinux/. Nothing compiles policy at runtime: root is\n\
         # read-only, and a policy build on a running node would be a write to a\n\
         # filesystem that does not take writes.\n\n\
         SELINUX={}\n\
         SELINUXTYPE={}\n",
        node.name, s.compile_at, s.mode, s.policy_type
    );
    Rendered::new(
        format!("{}/selinux/config", node.name),
        vec!["CD-14", "CB-03"],
        body,
    )
}

/// greenboot's deadline and its attempt count (§13.3).
fn greenboot(c: &Cluster, node: &Node) -> Rendered {
    let g = &c.policy.greenboot;
    let body = format!(
        "# greenboot on {}. The boot is declared successful only if the health\n\
         # predicate passes within {}s; on failure the previous ostree deployment is\n\
         # restored automatically and the node reboots into it (§13.3).\n\
         #\n\
         # The attempt count is what bounds the loop. A deadline without one is a\n\
         # node that rolls back, boots the previous deployment, and --- if that is\n\
         # also unhealthy for an unrelated reason --- keeps trying. Past {} attempts\n\
         # the previous deployment stands and the alert is the operator's signal.\n\n\
         GREENBOOT_MAX_BOOT_ATTEMPTS={}\n\
         GREENBOOT_HEALTHCHECK_TIMEOUT={}\n",
        node.name, g.deadline_s, g.max_boot_attempts, g.max_boot_attempts, g.deadline_s
    );
    Rendered::new(format!("{}/greenboot.conf", node.name), vec!["CD-14"], body)
}

/// A `[Service]` section, re-exported so the module can build units if it grows
/// to need them.
#[allow(dead_code)]
fn unit_section(name: &str, lines: &[String]) -> String {
    section(name, lines)
}
