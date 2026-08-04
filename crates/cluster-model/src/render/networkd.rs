//! The network policy a node configures itself from (`SPEC.md` §3.1, §3.3, §4).
//!
//! This module used to render nine `.network` files: a management unit, two mesh
//! units and a loopback unit for each of three nodes, every one of them matching
//! on a MAC address out of the model. None of that can be rendered any more, and
//! the reason is worth stating precisely.
//!
//! A `.network` file for a mesh port needs two facts the image cannot have. Its
//! `[Match]` needs the interface name, which depends on which slot the card is
//! in; and its `Address=` needs this machine's ordinal and the ordinal of the
//! peer on the far end of *that specific cable*, which depends on how somebody
//! ran the wiring (§3.1, §3.3). Both are properties of a machine, and one image
//! boots all three (§8.4).
//!
//! What is still R1 is the *policy*: the thresholds that sort a port into a
//! class, the MTUs, the route metrics, the addressing bases and the discovery
//! parameters. Those are facts about the fleet, they are rendered here, they are
//! diff-gated like everything else, and `cluster-init` reads this file rather
//! than carrying any of them as a constant. A daemon with its own copy of the
//! transit metric would be a second source for it, which is what R1 forbids.

use crate::render::{node_path, Rendered};
use crate::Cluster;

/// The rendered policy `cluster-init` reads at boot.
pub const INIT_CONF: &str = "init.conf";

pub(crate) fn render(c: &Cluster) -> Vec<Rendered> {
    vec![policy(c), sysctl(c)]
}

/// Everything `cluster-init` needs and nothing it can work out for itself.
///
/// `key=value`, `#` comments, one section per concern. A rendered file rather
/// than a struct compiled into the binary: the binary is built once and the
/// model changes, and a metric that lived in both would drift in the copy
/// nobody looked at.
fn policy(c: &Cluster) -> Rendered {
    let net = &c.network;
    let mesh = net.mesh_class().expect("the model check requires a mesh class");
    let lan = net.lan_class().expect("the model check requires a lan class");
    let addressing = &net.addressing;
    let discovery = &net.discovery;

    let mut body = String::new();
    body.push_str(
        "# What a node needs to configure itself, and nothing it can work out\n\
         # for itself (§3.1, §3.3, §4.1).\n\
         #\n\
         # Read by cluster-init at boot. It is here rather than compiled into\n\
         # the binary because the binary is built once and the model changes: a\n\
         # route metric that lived in both would drift in the copy nobody read.\n\n",
    );

    body.push_str("# How a port is sorted into a class. Read against the driver's\n");
    body.push_str("# *supported* link modes, never the negotiated speed: a mesh port is\n");
    body.push_str("# down exactly when the peer it waits for has not booted (§3.1).\n");
    body.push_str(&format!("mesh_min_speed_mbps={}\n", mesh.min_speed_mbps));
    body.push_str(&format!("mesh_count={}\n", mesh.count));
    body.push_str(&format!("mesh_mtu={}\n", mesh.mtu));
    body.push_str(&format!("lan_min_speed_mbps={}\n", lan.min_speed_mbps));
    body.push_str(&format!("lan_count={}\n", lan.count));
    body.push_str(&format!("lan_mtu={}\n", lan.mtu));
    body.push_str(&format!("lan_addressing={}\n\n", lan.addressing));

    body.push_str("# The arithmetic turning an ordinal into an address (§4.1). Both ends\n");
    body.push_str("# of a cable compute the same link from the same two ordinals, and the\n");
    body.push_str("# lower ordinal takes the even address.\n");
    body.push_str(&format!("fleet_size={}\n", c.cluster.fleet.size));
    body.push_str(&format!("loopback_base={}\n", addressing.loopback_base));
    body.push_str(&format!(
        "loopback_prefix_len={}\n",
        addressing.loopback_prefix_len
    ));
    body.push_str(&format!("link_base={}\n", addressing.link_base));
    body.push_str(&format!("link_prefix_len={}\n\n", addressing.link_prefix_len));

    body.push_str("# Failover with no daemon: networkd withdraws the direct route on\n");
    body.push_str("# carrier loss and the transit route takes over at one more hop (§4.2).\n");
    body.push_str(&format!(
        "direct_metric={}\n",
        net.routing.direct_metric
    ));
    body.push_str(&format!(
        "transit_metric={}\n\n",
        net.routing.transit_metric
    ));

    body.push_str("# Learning which peer is on the far end of a given cable (§3.3).\n");
    body.push_str(&format!("discovery_group={}\n", discovery.group));
    body.push_str(&format!("discovery_port={}\n", discovery.port));
    body.push_str(&format!("discovery_interval_ms={}\n", discovery.interval_ms));
    body.push_str(&format!("discovery_timeout_s={}\n\n", discovery.timeout_s));

    body.push_str("# Where a node reads its own stable identifier, before it has an\n");
    body.push_str("# ordinal (§2.3.2). Not derived from any MAC.\n");
    body.push_str(&format!("machine_id_path={}\n", c.cluster.identity.source));
    body.push_str(&format!(
        "bulk_disk_min_gb={}\n\n",
        c.cluster.detection.bulk_disk_min_gb
    ));

    body.push_str("# The cluster's names (§4.3).\n");
    body.push_str(&format!("domain={}\n", c.cluster.domain));
    body.push_str(&format!("name_template={}\n", c.cluster.fleet.name_template));
    body.push_str(&format!("entry_name={}\n", c.cluster.fleet.entry_name));
    body.push_str(&format!("hosts_path={}\n", net.hosts.path));
    body.push_str(&format!("hosts_staging_path={}\n\n", net.hosts.staging_path));

    body.push_str("# Which role takes which ordinal, and which are handed out in what\n");
    body.push_str("# order (§2.3). `detect` is `bulk-disk` for the one role a machine\n");
    body.push_str("# works out for itself and `assigned` for the rest.\n");
    for role in &c.cluster.role {
        let ordinal = role
            .ordinal
            .map(|o| o.to_string())
            .unwrap_or_else(|| "-".to_string());
        let order = role
            .assign_order
            .map(|o| o.to_string())
            .unwrap_or_else(|| "-".to_string());
        body.push_str(&format!(
            "role={}:{}:{ordinal}:{order}:{}\n",
            role.id, role.detect, role.update_position
        ));
    }

    Rendered::new(node_path(INIT_CONF), vec!["CD-01", "CD-02", "CD-17"], body)
}

fn sysctl(c: &Cluster) -> Rendered {
    let forward = u8::from(c.network.routing.ip_forward);
    let body = format!(
        "# Forwarding, so that a transit route has something to transit (§4.2).\n\
         # Without this a node in the middle of a failover path drops the packets\n\
         # its own route table told a peer to send it.\n\
         #\n\
         # Identity-free, so it is rendered rather than written at boot: every\n\
         # machine forwards, whichever ordinal it holds.\n\
         net.ipv4.ip_forward={forward}\n"
    );
    Rendered::new(
        node_path("sysctl.d/90-cluster.conf"),
        vec!["CD-02"],
        body,
    )
}
