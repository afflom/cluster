//! `/etc/hosts`, rendered from the node table (`SPEC.md` §4.3).
//!
//! There is no DNS service. Three nodes with static addresses do not justify a
//! resolver, and a hosts file has no failure mode of its own --- which matters
//! more than it looks: during `n1`'s update window (§14.2) a resolver on `n1`
//! would take name resolution down with it, and every mesh name would stop
//! answering at precisely the moment the other two nodes are checking on it.

use crate::render::Rendered;
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Rendered {
    let suffixes = &c.network.hosts;
    let mut body = String::new();

    body.push_str(&format!(
        "# Name resolution for {}. No DNS service: a hosts file has no failure\n\
         # mode of its own, and during n1's update window a resolver on n1 would\n\
         # take every mesh name down with it (§4.3, §14.2).\n\n",
        node.name
    ));

    body.push_str("127.0.0.1\tlocalhost localhost.localdomain\n");
    body.push_str("::1\t\tlocalhost localhost.localdomain\n\n");

    body.push_str("# Mesh loopbacks. Every mesh service binds one of these (§4.1).\n");
    // Every node carries the short name as an alias, its own included: anything
    // that reads `hostname` and looks it up should get the mesh address rather
    // than 127.0.0.1, which is what a service binding "its own name" would
    // otherwise get.
    for n in &c.cluster.node {
        body.push_str(&format!(
            "{}\t{}.{} {}\n",
            n.loopback, n.name, suffixes.mesh_suffix, n.name
        ));
    }

    body.push_str("\n# Management plane.\n");
    for n in &c.cluster.node {
        let addr = n
            .mgmt_address
            .split_once('/')
            .map_or(n.mgmt_address.as_str(), |(a, _)| a);
        body.push_str(&format!("{addr}\t{}.{}\n", n.name, suffixes.mgmt_suffix));
    }

    Rendered::new(format!("{}/hosts", node.name), vec!["CD-04"], body)
}
