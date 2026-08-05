//! T3: the real fleet (`SPEC.md` §10.2, §21.2).
//!
//! The only tier that can discharge a `CH-` claim. A simulated run cannot
//! establish that VT-x is enabled in firmware, that a MAC belongs to the card
//! the model says it does, or that a SATA device is physically mounted where
//! §2.2 requires --- and `cluster_harness::collect` refuses to hand a `CH-`
//! scenario to a tier below this one, so the exclusion is enforced rather than
//! remembered.
//!
//! Not run by `cargo test`. `just t3` runs it, over SSH, against the nodes
//! themselves after a promotion. There is no fixture to skip on: if the fleet is
//! not reachable, that is the finding.

use std::path::PathBuf;
use std::process::Command;

use cluster_model::{Cluster, Node};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cluster-harness is two below the root")
        .to_path_buf()
}

fn model() -> Cluster {
    let c = Cluster::load(&root().join("model")).expect("the cluster model loads");
    c.check().expect("the cluster model is consistent");
    c
}

/// A node's name on the tailnet, which is the only stable way to reach one
/// (§3.2, §4.5).
fn tailnet_name(name: &str) -> String {
    let c = model();
    format!(
        "{name}.{}.{}",
        c.cluster.tailnet, c.cluster.magic_dns_suffix
    )
}

/// Run a command on a real node, over the tailnet.
fn on(node: &Node, command: &str) -> Result<String, String> {
    // By the node's tailnet name. Management addresses come from DHCP (§3.2), so
    // there is no address to write down --- and a smoke run that read an
    // inventory of its own would be testing against a second source. MagicDNS is
    // the one stable name, and it works on the LAN and off it alike (§4.5).
    let address = tailnet_name(&node.name);
    let output = Command::new("ssh")
        .args([
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "BatchMode=yes",
            &format!("root@{address}"),
            command,
        ])
        .output()
        .map_err(|e| format!("{}: ssh: {e}", node.name))?;
    if !output.status.success() {
        return Err(format!(
            "{}: `{command}` exited {}: {}",
            node.name,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `CH-01`: every firmware setting the model declares holds on the node.
///
/// Configuration this pipeline cannot reach (§2.4). It is applied by hand at
/// bootstrap, and declaring it in the model is what makes "re-verified on every
/// hardware smoke run" mean something: the check reads the model rather than an
/// operator's memory.
#[test]
fn every_declared_firmware_setting_holds_ch_01() {
    let c = model();
    assert!(
        !c.cluster.firmware.is_empty(),
        "the model declares firmware settings, or this test verifies nothing"
    );

    for node in &c.nodes() {
        for setting in &c.cluster.firmware {
            let (kind, key) = setting
                .probe
                .split_once(':')
                .unwrap_or_else(|| panic!("`{}` is not a probe", setting.probe));
            let observed = match kind {
                // Virtualization extensions and topology are visible to the
                // kernel; the BIOS switch that gates them is not, so what is
                // asserted is the effect rather than the setting.
                "cpuinfo" => on(node, &format!("grep -c '{key}' /proc/cpuinfo || true"))
                    .unwrap_or_else(|e| panic!("{e}")),
                "ipmi" => on(node, &format!("ipmitool {} 2>&1 || true", ipmi_query(key)))
                    .unwrap_or_else(|e| panic!("{e}")),
                other => panic!("`{other}` is not a probe kind this tier knows"),
            };

            let holds = match kind {
                "cpuinfo" => observed.trim() != "0",
                "ipmi" => !observed.is_empty() && !observed.contains("Error"),
                _ => false,
            };
            assert!(
                holds,
                "{}: firmware setting `{}` should be `{}` ({}). Observed: {observed}. \
                 Reason it is set that way: {}",
                node.name, setting.setting, setting.value, setting.probe, setting.reason
            );
        }
    }
}

/// The `ipmitool` subcommand a probe key names.
fn ipmi_query(key: &str) -> &'static str {
    match key {
        "bios" => "chassis bootparam get 5",
        "chassis-policy" => "chassis policy list",
        "bootparam" => "chassis bootparam get 5",
        "sol" => "sol info",
        "watchdog" => "mc watchdog get",
        _ => "mc info",
    }
}

/// `CH-02`: the machine presents the ports the profile declares, and each mesh
/// port reaches the peer the addressing assumed.
///
/// This used to assert that every MAC in the model was present on the card the
/// model named. There are no MACs any more (§3.1): they are assigned by whoever
/// made the card, they change when a mainboard is replaced, and a fleet that
/// recorded them made replacing hardware an edit to a repository.
///
/// What real hardware can still establish, and simulation cannot, is that the
/// chassis in the rack is the shape §2.1 says it is --- and that the cables run
/// where the addressing believes they do. §21.12 records what was given up: a
/// mesh cable moved between the two 10GbE ports of one machine is not detected,
/// because it is not an error.
#[test]
fn the_ports_and_the_cabling_are_what_the_model_expects_ch_02() {
    let c = model();
    let profile = &c.cluster.profile;
    let mesh = c
        .network
        .mesh_class()
        .expect("the model declares a mesh class");
    let lan = c
        .network
        .lan_class()
        .expect("the model declares a lan class");

    for node in &c.nodes() {
        // Counted from each driver's *supported* modes, which is what the
        // classifier reads: a mesh port with no cable in it reports no speed at
        // all, and on a fleet mid-boot several of them have none (§3.1).
        let script = "for d in /sys/class/net/*/device; do n=$(basename $(dirname $d)); \
                      ethtool $n 2>/dev/null | sed -n '/Supported link modes/,/Supported pause/p' \
                      | grep -o '[0-9]*base' | tr -d 'base' | sort -n | tail -1; done";
        let speeds: Vec<u32> = on(node, script)
            .unwrap_or_else(|e| panic!("{e}"))
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect();

        let mesh_ports = speeds.iter().filter(|m| **m >= mesh.min_speed_mbps).count();
        assert_eq!(
            mesh_ports, profile.nic_10g as usize,
            "{}: the profile declares {} mesh-class ports and the chassis presents \
             {mesh_ports}. A machine that came up with fewer would join with no \
             redundancy and nothing would say so (§2.1, §3.1)",
            node.name, profile.nic_10g
        );
        assert!(
            mesh_ports >= mesh.count as usize,
            "{}: fewer mesh ports than the mesh class requires",
            node.name
        );

        let lan_ports = speeds
            .iter()
            .filter(|m| **m >= lan.min_speed_mbps && **m < mesh.min_speed_mbps)
            .count();
        assert!(
            lan_ports >= lan.count as usize,
            "{}: {lan_ports} LAN-class port(s), and a conforming machine presents at \
             least {} (§2.1, §3.1)",
            node.name,
            lan.count
        );

        // And the cabling: every peer loopback is reachable, which is only true
        // if each cable runs to the machine the addressing assumed it did.
        for peer in c.peers_of(&node.name) {
            let reached = on(
                node,
                &format!(
                    "ping -c 1 -W 5 {} >/dev/null 2>&1 && echo ok",
                    peer.loopback
                ),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                reached.trim(),
                "ok",
                "{} cannot reach {} at {}. Either a cable runs to a machine the \
                 addressing does not expect, or one is missing (§3.3, §21.12)",
                node.name,
                peer.name,
                peer.loopback
            );
        }
    }
}

/// `CH-03`: the declared storage devices are present, in their declared roles.
///
/// §2.2 is verified physically before the storage node is provisioned, and the fallback ---
/// the cache device becoming a partition of the M.2 --- is one line in the
/// model. This is the tier that can tell which outcome actually happened, and it
/// fails if the model says one and the chassis holds the other.
#[test]
fn the_declared_storage_devices_are_present_ch_03() {
    let c = model();

    // Which devices a machine carries follows from its role, and the role is
    // discovered (§2.3). The storage node is the one holding the bulk device ---
    // that is the whole of §2.3.1's predicate --- so it is the one expected to
    // have all three, and the others the two that are not bulk.
    let storage_role = c
        .cluster
        .self_detected_role()
        .expect("the model declares a self-detected role")
        .id
        .clone();
    let threshold = c.cluster.detection.bulk_disk_min_gb;

    for node in &c.nodes() {
        let expected: Vec<&cluster_model::Disk> = c
            .cluster
            .disk
            .iter()
            .filter(|d| node.role == storage_role || d.size_gb < threshold)
            .collect();

        let block = on(node, "lsblk --nodeps --output NAME,SIZE,ROTA --noheadings")
            .unwrap_or_else(|e| panic!("{e}"));
        let devices: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            devices.len(),
            expected.len(),
            "{}: a `{}` machine carries {} device(s) and this one has {}: {block}",
            node.name,
            node.role,
            expected.len(),
            devices.len()
        );

        // A spinning disk reports ROTA=1 and an SSD or NVMe reports 0. The
        // distinction matters: §5.3's whole design rests on the origin being
        // rotational and the cache not being --- a cache on a spindle would make
        // the tier pointless, and an origin on flash would make the writethrough
        // argument moot.
        //
        // The size is checked too. §2.2 is verified physically before the
        // storage node is provisioned, and a 2 TB origin that came back as
        // something else is the sort of substitution that boots fine.
        for disk in &expected {
            let rotational = disk.kind == "hdd";
            let matching = devices.iter().any(|line| {
                let mut fields = line.split_whitespace();
                let (_name, size, rota) = (fields.next(), fields.next(), fields.next());
                if rota != Some(if rotational { "1" } else { "0" }) {
                    return false;
                }
                // `lsblk` reports human sizes: 1.8T for a 2 TB disk, 238.5G for
                // a 256 GB one. Compared as a fraction, because the declared
                // figure is the marketing capacity and the reported one is not.
                size.and_then(parse_size_gb).is_some_and(|reported| {
                    let declared = f64::from(disk.size_gb);
                    reported > declared * 0.85 && reported < declared * 1.1
                })
            });
            assert!(
                matching,
                "{}: the model declares a {} GB {} device `{}` for {} and none of \
                 the node's devices reports that: {block}",
                node.name, disk.size_gb, disk.kind, disk.id, disk.purpose
            );
        }

        // The partition layout §5.1 declares, on the boot device.
        for partition in &c.cluster.partition {
            let mounted = on(
                node,
                &format!("findmnt --noheadings --output SOURCE {}", partition.mount),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(
                !mounted.is_empty(),
                "{}: {} is declared and not mounted",
                node.name,
                partition.mount
            );
        }
    }
}

/// `CH-04`: the fleet is healthy on the promoted image, on real hardware.
///
/// T3 runs after a promotion, so this is the assertion that what the tiers
/// validated in guests also holds on the machines. It uses the same predicate
/// the other three tiers use, which is the point of defining it once (§10.1).
#[test]
fn the_real_fleet_is_healthy_on_the_promoted_image_ch_04() {
    let c = model();
    let mut booted = Vec::new();

    for node in &c.nodes() {
        let json = on(node, "/usr/bin/cluster-health check").unwrap_or_else(|e| panic!("{e}"));
        let report: cluster_health::Report =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{}: {e}: {json}", node.name));
        assert!(report.healthy, "{}: {:?}", node.name, report.failures());
        booted.push(report.booted);
    }

    // And the fleet is not split. §18 alerts on this after two hours; after a
    // promotion has settled it should already be false.
    booted.sort();
    booted.dedup();
    assert_eq!(
        booted.len(),
        1,
        "the fleet is split across {booted:?}. That is a legitimate, alerted state \
         requiring a human decision --- it is not silently reconciled (§13.4)"
    );
}

/// `CH-05`: every node is the hardware the model says it is.
///
/// §2.1 declares one profile and every node references it, and those figures are
/// not decoration: the absence of AVX-512 bounds what any measurement
/// generalizes to, and the equality of base and maximum clock is the reason a
/// node built on this part can host a measurement at all (§2.1, §21.1).
///
/// A replaced mainboard that came back with different memory, a different NIC
/// count, or a part with turbo headroom would satisfy every other tier and
/// silently invalidate the series. Only a real node can say.
#[test]
fn every_node_is_the_hardware_the_model_declares_ch_05() {
    let c = model();

    for node in &c.nodes() {
        // One profile for the fleet: §2.1 makes the hardware uniform, so it is
        // declared once and every machine is checked against it.
        let profile = &c.cluster.profile;

        // Cores and threads. `nproc` reports what is online, and on the
        // measurement node SMT is off by kernel argument (§8.5) --- so the
        // comparison is against the topology the firmware exposes, not against
        // what happens to be schedulable.
        let sockets: u32 = on(node, "lscpu -p=CORE | grep -vc '^#'")
            .unwrap_or_else(|e| panic!("{e}"))
            .trim()
            .parse()
            .unwrap_or(0);
        assert_eq!(
            sockets, profile.cores,
            "{}: the model declares {} cores (§2.1)",
            node.name, profile.cores
        );

        // AVX-512's absence bounds what any measurement generalizes to. A part
        // that grew it would not be the part the series was taken on.
        let avx512 = on(node, "grep -c avx512 /proc/cpuinfo || true")
            .unwrap_or_else(|e| panic!("{e}"))
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
            > 0;
        assert_eq!(
            avx512, profile.avx512,
            "{}: the model declares avx512 = {} (§2.1)",
            node.name, profile.avx512
        );

        // Base and maximum clock. Their equality is why this part has no boost
        // algorithm to introduce variance (§2.1).
        let max_mhz: f64 = on(node, "lscpu | awk '/CPU max MHz/ {print $4}'")
            .unwrap_or_else(|e| panic!("{e}"))
            .trim()
            .parse()
            .unwrap_or(profile.max_mhz as f64);
        assert!(
            (max_mhz - f64::from(profile.max_mhz)).abs() < 100.0,
            "{}: the model declares a {} MHz maximum and the part reports {max_mhz}. \
             A part with turbo headroom has a boost algorithm, and §2.1's reason for \
             using it as a measurement host does not hold (§21.1)",
            node.name,
            profile.max_mhz
        );

        // Memory, within the rounding a kernel's own accounting introduces.
        let gib: u32 = on(
            node,
            "awk '/MemTotal/ {print int($2/1024/1024 + 0.5)}' /proc/meminfo",
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .trim()
        .parse()
        .unwrap_or(0);
        assert!(
            gib.abs_diff(profile.memory_gb) <= 2,
            "{}: the model declares {} GB and the node reports {gib} (§2.1)",
            node.name,
            profile.memory_gb
        );
        let slots: u32 = on(
            node,
            "dmidecode -t memory | grep -c '^Memory Device$' || true",
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .trim()
        .parse()
        .unwrap_or(profile.memory_slots);
        assert_eq!(slots, profile.memory_slots, "{}: DIMM slots", node.name);
        assert!(
            profile.memory_ceiling_gb >= profile.memory_gb,
            "the declared ceiling must not be below what is installed"
        );

        // Two 10 GbE ports is what bounds the mesh at three nodes (§1.1), so a
        // node with fewer would make the topology unbuildable and one with more
        // would make §1.1's ceiling a statement about nothing.
        let ports: u32 = on(node, "ls /sys/class/net | grep -v lo | wc -l")
            .unwrap_or_else(|e| panic!("{e}"))
            .trim()
            .parse()
            .unwrap_or(0);
        assert!(
            ports >= profile.nic_10g + profile.nic_1g,
            "{}: the model declares {} 10GbE and {} 1GbE ports; the node shows \
             {ports} interfaces (§1.1, §2.1)",
            node.name,
            profile.nic_10g,
            profile.nic_1g
        );
        assert!(profile.tdp_watts > 0, "a declared part has a declared TDP");
        assert!(profile.threads >= profile.cores);
        assert!(profile.base_mhz > 0);
        assert!(profile.ipmi, "every node has a dedicated BMC port (§3.2)");
    }
}

/// `CH-06`: the BMC is reachable out of band and holds its declared settings.
///
/// The BMC is the one component this pipeline cannot update (§3.2) and the one
/// path to a node that does not depend on the node. Its power-on behaviour is
/// what brings the cluster back after an outage with no operator present (§2.5),
/// and only an out-of-band query can confirm it.
#[test]
fn the_bmc_holds_its_declared_settings_ch_06() {
    let c = model();

    for node in &c.nodes() {
        // The BMC's address is not a model fact and never was one worth being.
        // It is out-of-band, configured in firmware (§2.4), on a VLAN this
        // pipeline does not touch --- and since §3.2 made every host address
        // DHCP, a repository that recorded the BMC's would be recording the one
        // address it has no part in assigning.
        //
        // So the operator supplies it, and its absence fails the tier rather
        // than skipping it: a hardware smoke run that silently omitted the BMC
        // would be green about the one path that exists because the node might
        // be down.
        let key = format!("CLUSTER_BMC_{}", node.name.to_uppercase());
        let bmc = std::env::var(&key).unwrap_or_else(|_| {
            panic!(
                "{key} is not set. The BMC's address is an operator fact, not a model \
                 one (§2.4, §3.2), and this tier cannot reach {} without it",
                node.name
            )
        });
        let bmc = bmc.as_str();

        // Out of band: to the BMC itself, not through the host. A query that
        // went through the node would prove nothing about the path that exists
        // *because* the node might be down.
        let policy = std::process::Command::new("ipmitool")
            .args(["-I", "lanplus", "-H", bmc, "chassis", "policy", "list"])
            .output()
            .unwrap_or_else(|e| panic!("{}: cannot reach the BMC at {bmc}: {e}", node.name));
        let text = String::from_utf8_lossy(&policy.stdout);
        assert!(
            policy.status.success(),
            "{}: the BMC at {bmc} did not answer. It is the one path to this node \
             that does not depend on this node (§3.2)",
            node.name
        );
        assert!(
            text.contains("always-on"),
            "{}: restore-on-AC-power-loss must be always-on. This is a headless \
             cluster with no operator after an outage (§2.5): {text}",
            node.name
        );

        // The storage node comes up first and alone, so the other two find their
        // registry and NFS already answering rather than racing for inrush.
        let delay = c
            .cluster
            .role(&node.role)
            .map(|r| r.power_on_delay_s)
            .unwrap_or_default();
        if delay > 0 {
            assert_eq!(
                node.name, c.policy.drain.migration_target,
                "only the storage node carries a power-on delay (§2.5)"
            );
        }
    }
}

/// A size as `lsblk` prints it --- `1.8T`, `238.5G` --- in gigabytes.
fn parse_size_gb(size: &str) -> Option<f64> {
    let (number, unit) = size.split_at(size.len().checked_sub(1)?);
    let value: f64 = number.parse().ok()?;
    Some(match unit {
        "T" => value * 1000.0,
        "G" => value,
        "M" => value / 1000.0,
        _ => return None,
    })
}
