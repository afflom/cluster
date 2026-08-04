//! `bootc` kernel arguments (`SPEC.md` §8.5).
//!
//! bootc reads `kargs.d/*.toml` from inside the image, so the kernel command
//! line is a property of the image a node booted and not of a bootloader
//! configuration something edited afterwards. That is what makes `CB-` able to
//! assert the isolation set is present: the assertion is about the image, and
//! the image is what the gate built.
//!
//! What is constructible here is that the arguments are present and that
//! `/sys/devices/system/cpu/isolated` reflects them. That the environment
//! yields stable measurements is neither constructible nor testable, and §21.1
//! records why.

use crate::render::Rendered;
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Rendered {
    let base = &c.images.base;
    let variant = c.images.variant_for(&node.name);

    let kargs: Vec<String> = variant
        .map(|v| v.all_kargs(base))
        .unwrap_or_else(|| base.content.kargs.clone());

    let mut body = String::new();
    body.push_str(&format!(
        "# Kernel arguments for {}. Read by bootc from inside the image, so the\n\
         # command line is a property of what was booted rather than of what was\n\
         # edited afterwards (§8.5).\n",
        node.name
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
    body.push('\n');

    body.push_str("kargs = [\n");
    for karg in &kargs {
        body.push_str(&format!("  \"{karg}\",\n"));
    }
    body.push_str("]\n");

    Rendered::new(
        format!("{}/kargs.d/10-cluster.toml", node.name),
        vec!["CD-06"],
        body,
    )
}
