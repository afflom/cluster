//! `/etc/hosts`, rendered from the fleet's ordinals (`SPEC.md` §4.3).
//!
//! No DNS service: three nodes do not justify a resolver, and a hosts file has
//! no failure mode of its own.
//!
//! **This file is the same on every machine**, which is why it can be rendered
//! at all when the `.network` files beside it cannot. Nothing in it depends on
//! which chassis is reading it: every name maps to an ordinal, every ordinal
//! derives its own loopback (§4.1), and `devcluster` maps to ordinal 1 because
//! the storage role pins that ordinal (§2.3.2). A machine does not need to know
//! which node it is in order to know where all three are.
//!
//! The rule is not "runtime because identity is discovered". A file is rendered
//! when its content is a fact about the *fleet*, and written at boot when it is
//! a fact about *this machine*.
//!
//! There are no management names. Those addresses come from DHCP (§3.2), so
//! there is nothing stable to name and nothing that needs one.

use crate::render::{node_path, Rendered};
use crate::Cluster;

pub(crate) fn render(c: &Cluster) -> Rendered {
    let mut body = String::new();
    body.push_str(
        "# Every name this cluster owns, resolved with no resolver (§4.3).\n\
         #\n\
         # Placed by cluster-hosts.service at boot rather than copied in by the\n\
         # image build: /etc/hosts is bind-mounted and busy under RUN, and a COPY\n\
         # to it is *silently dropped*. Both were observed before this comment\n\
         # existed.\n\n",
    );
    body.push_str("127.0.0.1\tlocalhost\n");
    body.push_str("::1\t\tlocalhost\n\n");

    let entry = c
        .node_with_role(storage_role(c))
        .expect("the model check requires exactly one self-detected role");
    body.push_str(&format!(
        "# The cluster's entry point: the bare name is where the control plane\n\
         # answers, so a client that knows only `{}` can find everything else.\n",
        c.cluster.fleet.entry_name
    ));
    body.push_str(&format!(
        "{}\t{}\n\n",
        entry.loopback, c.cluster.fleet.entry_name
    ));

    body.push_str(
        "# The ordinals. A name tracks the machine that registered into that\n\
         # position and does not move if a role is reassigned --- the whole point\n\
         # of a stable name is that it does not (§4.3).\n",
    );
    for node in c.nodes() {
        body.push_str(&format!(
            "{}\t{}\t{}\n",
            node.loopback, node.fqdn, node.name
        ));
    }

    Rendered::new(node_path("hosts"), vec!["CD-04"], body)
}

/// The role that pins the entry name, which is the self-detected one (§2.3.1).
fn storage_role(c: &Cluster) -> &str {
    c.cluster
        .self_detected_role()
        .map(|r| r.id.as_str())
        .expect("the model check requires exactly one self-detected role")
}
