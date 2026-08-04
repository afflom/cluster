//! `bootc` kernel arguments (`SPEC.md` §8.5).
//!
//! bootc reads `kargs.d/*.toml` from inside the image, so what is here is a
//! property of the image a node booted and not of a bootloader configuration
//! something edited afterwards.
//!
//! **Only the arguments every role takes are here.** One image boots all three
//! (§8.4), so an `isolcpus=` in this file would isolate two of the storage
//! node's four cores to no purpose. The testbed's isolation arguments are
//! applied after the role is known, with
//! `bootc loader-entries set-options-for-source --source cluster-role`, and the
//! set that will be applied is rendered beside this file so it is a model fact
//! rather than a string built inside a binary.
//!
//! What is constructible is that the arguments are present and that
//! `/sys/devices/system/cpu/isolated` reflects them. That the environment
//! yields stable measurements is neither constructible nor testable, and §21.1
//! records why.

use crate::render::{node_path, Rendered};
use crate::Cluster;

pub(crate) fn render(c: &Cluster) -> Rendered {
    let base = &c.images.base;

    let mut body = String::new();
    body.push_str(
        "# The kernel arguments every node takes, whatever role it holds. Read by\n\
         # bootc from inside the image, so the command line is a property of what\n\
         # was booted rather than of what was edited afterwards (§8.5).\n\
         #\n\
         # Role-conditional arguments are deliberately absent. One image boots all\n\
         # three roles, so an isolcpus= here would isolate the storage node's cores\n\
         # too; the testbed's set is in role-kargs.conf and is applied by\n\
         # cluster-init once the role is known.\n\n",
    );

    body.push_str("kargs = [\n");
    for karg in &base.content.kargs {
        body.push_str(&format!("  \"{karg}\",\n"));
    }
    body.push_str("]\n");

    Rendered::new(node_path("kargs.d/10-cluster.toml"), vec!["CD-06"], body)
}

/// The arguments each role applies after it knows which one it is (§8.5).
///
/// Rendered rather than assembled in `cluster-init`, for the same reason the
/// route metrics are: a set of kernel arguments built inside a binary is a set
/// nothing diff-gates, and the one thing worse than isolating the wrong cores is
/// isolating them from a string no gate ever read.
pub(crate) fn role_kargs(c: &Cluster) -> Vec<Rendered> {
    let mut out = Vec::new();
    for role in &c.cluster.role {
        let variant = c.images.variant_for(&role.id);
        let kargs: Vec<String> = variant.map(|v| v.kargs.clone()).unwrap_or_default();

        let mut body = String::new();
        body.push_str(&format!(
            "# The kernel arguments a machine holding the `{}` role applies once it\n\
             # knows it holds it (§8.5).\n\
             #\n\
             # Applied with `bootc loader-entries set-options-for-source --source\n\
             # cluster-role`, which tracks them as their own source in the BLS entry\n\
             # and re-merges them on every upgrade. A node that stops holding this\n\
             # role sets the same source empty and drops them.\n",
            role.id
        ));
        if let Some(isolation) = variant.and_then(|v| v.isolation.as_ref()) {
            body.push_str(&format!(
                "#\n\
                 # CPUs {} are isolated, SMT is off, and C-states are constrained. Each of\n\
                 # those is a constructible fact and CB- carries it. That the result yields\n\
                 # stable measurements is not, and §21.1 records why.\n",
                isolation.isolated_cpus
            ));
        }
        if kargs.is_empty() {
            body.push_str(
                "#\n\
                 # This role adds nothing. Rendered anyway: cluster-init reads one of\n\
                 # these per role, and an absent file would be indistinguishable from a\n\
                 # role whose arguments nobody rendered.\n",
            );
        }
        body.push('\n');
        body.push_str(&format!("options={}\n", kargs.join(" ")));

        out.push(Rendered::new(
            node_path(format!("role-kargs-{}.conf", role.id)),
            vec!["CD-06", "CD-19"],
            body,
        ));
    }
    out
}
