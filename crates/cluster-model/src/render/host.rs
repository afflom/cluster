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
    vec![sshd(c, node), selinux(c, node), greenboot(c, node)]
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
