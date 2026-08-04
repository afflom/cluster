//! `cluster-init`: what a machine works out about itself at boot (`SPEC.md`
//! §2.3, §3.1, §3.3, §4.1).
//!
//! Ordered before `systemd-networkd`, because the units it writes are the ones
//! networkd will read. The sequence is:
//!
//! 1. read the rendered policy (`init.conf`), which carries every threshold;
//! 2. classify this machine's ports by supported link speed (§3.1);
//! 3. decide from its own disks whether it is the storage node (§2.3.1);
//! 4. bring the mesh ports up link-local and discover the peer on each (§3.3);
//! 5. self-assign, or ask the registrar for an ordinal and a role (§2.3.2);
//! 6. derive every address from the ordinal and write the units (§4.1);
//! 7. write the role marker, the node environment, and the role's firewall
//!    include, then apply the role's kernel arguments if it has any (§8.4, §8.5).
//!
//! Every step that can decide something wrongly lives in the library beside this
//! file and is tested there. What is here is the ordering and the I/O.
//!
//! **A step that cannot complete fails the boot.** There is no partial mode: a
//! node with one mesh port, two bulk disks, or no ordinal has nothing safe to do
//! next, and a node that started its services anyway would look healthy while
//! being wrong (§3.1, §21.11).

use std::net::Ipv6Addr;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use cluster_init::addressing::Addressing;
use cluster_init::config::Config;
use cluster_init::discovery::{self, Announcement};
use cluster_init::links::{self, Classified, Port, Thresholds};
use cluster_init::net::{self, Wire};
use cluster_init::role::{self, Device, Registry};
use cluster_init::units::{self, Metrics, PeeredPort};
use cluster_init::{
    InitError, POLICY_PATH, REGISTRY_PATH, RUNTIME_DIR, RUNTIME_NETWORK_DIR, SECRET_PATH,
};

fn main() -> ExitCode {
    match run() {
        Ok(summary) => {
            println!("cluster-init: {summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Loud, and it names the step. A node that could not work out what
            // it is has nothing safe to do next, and §3.1 makes that a failed
            // boot rather than a degraded one.
            eprintln!("cluster-init: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, InitError> {
    let policy = std::fs::read_to_string(POLICY_PATH)
        .map_err(|e| InitError::Io(format!("reading {POLICY_PATH}: {e}")))?;
    let config = Config::parse(&policy)?;

    let classified = links::classify(&observed_ports()?, thresholds(&config)?)?;

    let machine_id = std::fs::read_to_string(config.string("machine_id_path")?)
        .map_err(|e| InitError::Io(format!("reading the machine id: {e}")))?
        .trim()
        .to_string();

    let is_registrar =
        role::holds_bulk_disk(&observed_devices()?, config.number("bulk_disk_min_gb")?)?;
    let detected = config.self_detected_role()?;
    let addressing = addressing(&config)?;
    let wire = wire(&config)?;

    // The registrar knows its ordinal without asking; every other machine has to
    // find it across a cable (§2.3.1, §2.3.2).
    let (ordinal, role_id, peers) = if is_registrar {
        let ordinal = detected.ordinal.ok_or_else(|| {
            InitError::Config("the self-detected role pins no ordinal (§2.3.2)".into())
        })?;
        let peers = serve_and_discover(
            &config,
            &classified,
            &machine_id,
            ordinal,
            &detected.id,
            &wire,
        )?;
        (ordinal, detected.id.clone(), peers)
    } else {
        let (grant, peers) = join(&classified, &machine_id, &wire)?;
        // The join secret never touches a rendered artifact or an image. It
        // arrives here and goes to a file only this node's root can read (§12.2).
        write_private(Path::new(SECRET_PATH), &grant.secret)?;
        (grant.ordinal, grant.role, peers)
    };

    let name = config.name_of(ordinal)?;
    let short = name
        .split_once('.')
        .map_or(name.as_str(), |(head, _)| head)
        .to_string();
    let loopback = addressing.loopback_of(ordinal)?;

    std::fs::create_dir_all(RUNTIME_NETWORK_DIR)?;
    std::fs::create_dir_all(RUNTIME_DIR)?;

    let written = units::all_units(
        &classified,
        &peers,
        ordinal,
        &addressing,
        config.number("mesh_mtu")?,
        config.number("lan_mtu")?,
        Metrics {
            direct: config.number("direct_metric")?,
            transit: config.number("transit_metric")?,
        },
    )?;
    for (file, body) in &written {
        std::fs::write(Path::new(RUNTIME_NETWORK_DIR).join(file), body)?;
    }

    std::fs::write(
        Path::new(RUNTIME_DIR).join("node.env"),
        units::node_env(
            ordinal,
            &short,
            &role_id,
            config
                .role(&role_id)
                .map(|r| r.update_position)
                .unwrap_or_default(),
            &loopback.to_string(),
        ),
    )?;

    // Exactly one marker, and the stale ones removed first. Every role-gated
    // unit carries `ConditionPathExists=` naming one of these, and two markers
    // would start two roles' services on one machine (§8.4).
    for row in &config.roles {
        let marker = Path::new(RUNTIME_DIR).join(format!("role.{}", row.id));
        if row.id == role_id {
            std::fs::write(&marker, "")?;
        } else if marker.exists() {
            std::fs::remove_file(&marker)?;
        }
    }

    role_firewall_include(&role_id)?;
    apply_role_kargs(&role_id)?;

    Ok(format!(
        "{short} is ordinal {ordinal}, role {role_id}, loopback {loopback}; \
         {} network unit(s) written",
        written.len()
    ))
}

fn thresholds(config: &Config) -> Result<Thresholds, InitError> {
    Ok(Thresholds {
        mesh_min_mbps: config.number("mesh_min_speed_mbps")?,
        mesh_count: config.number("mesh_count")?,
        lan_min_mbps: config.number("lan_min_speed_mbps")?,
        lan_count: config.number("lan_count")?,
    })
}

fn addressing(config: &Config) -> Result<Addressing, InitError> {
    let parse = |key: &str| -> Result<std::net::Ipv4Addr, InitError> {
        let raw = config.string(key)?;
        raw.parse()
            .map_err(|_| InitError::Config(format!("`{key}` is `{raw}`, which is not an address")))
    };
    Ok(Addressing {
        loopback_base: parse("loopback_base")?,
        loopback_prefix_len: config.number("loopback_prefix_len")? as u8,
        link_base: parse("link_base")?,
        link_prefix_len: config.number("link_prefix_len")? as u8,
        fleet_size: config.number("fleet_size")?,
    })
}

/// Every Ethernet port the kernel presents, with the speed its driver reports
/// support for (§3.1).
fn observed_ports() -> Result<Vec<Port>, InitError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        // Virtual interfaces have no `device` symlink. Bridges, veths and the
        // tailnet's tun are not ports this cluster classifies.
        if !entry.path().join("device").exists() {
            continue;
        }
        let output = Command::new("ethtool")
            .arg(&name)
            .output()
            .map_err(|e| InitError::Io(format!("running ethtool on {name}: {e}")))?;
        let text = String::from_utf8_lossy(&output.stdout);
        out.push(Port {
            name,
            max_supported_mbps: links::max_supported_mbps(&text),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Every whole block device, with the one carrying root marked (§2.3.1).
fn observed_devices() -> Result<Vec<Device>, InitError> {
    let output = Command::new("lsblk")
        .args([
            "--noheadings",
            "--nodeps",
            "--bytes",
            "--output",
            "PATH,SIZE,MOUNTPOINTS",
        ])
        .output()
        .map_err(|e| InitError::Io(format!("running lsblk: {e}")))?;
    let text = String::from_utf8_lossy(&output.stdout);

    let root_disk = root_disk()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(path), Some(size)) = (fields.next(), fields.next()) else {
            continue;
        };
        let bytes = size
            .parse()
            .map_err(|_| InitError::Hardware(format!("lsblk reported size `{size}` for {path}")))?;
        out.push(Device {
            is_boot: root_disk.as_deref() == Some(path),
            path: path.to_string(),
            bytes,
        });
    }
    Ok(out)
}

/// The whole device carrying `/`.
fn root_disk() -> Result<Option<String>, InitError> {
    let source = Command::new("findmnt")
        .args(["--noheadings", "--output", "SOURCE", "/"])
        .output()
        .map_err(|e| InitError::Io(format!("running findmnt: {e}")))?;
    let source = String::from_utf8_lossy(&source.stdout).trim().to_string();
    if source.is_empty() {
        return Ok(None);
    }
    let parent = Command::new("lsblk")
        .args(["--noheadings", "--output", "PKNAME", "--paths", &source])
        .output()
        .map_err(|e| InitError::Io(format!("running lsblk: {e}")))?;
    let parent = String::from_utf8_lossy(&parent.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((!parent.is_empty()).then_some(parent))
}

/// Put this role's firewall include where `nftables.conf` includes from.
///
/// nft treats an `include` of a missing file as an error, which is why the
/// renderer emits one per role even when it is empty (§4.4, §8.4).
fn role_firewall_include(role: &str) -> Result<(), InitError> {
    let source = format!("/usr/lib/cluster/nftables-role-{role}.conf");
    let target = Path::new(RUNTIME_DIR).join("nftables-role.conf");
    std::fs::copy(&source, &target).map_err(|e| InitError::Io(format!("placing {source}: {e}")))?;
    Ok(())
}

/// Apply the kernel arguments this role adds, if it adds any (§8.5).
///
/// `bootc loader-entries set-options-for-source` tracks them as their own source
/// in the BLS entry and re-merges them on every upgrade, so they survive an
/// update without being in the image --- which they cannot be, because one image
/// boots all three roles and isolating the storage node's cores would cost half
/// its CPU to no purpose.
///
/// Idempotent: setting the same options twice is a no-op, so this runs on every
/// boot without accumulating anything.
fn apply_role_kargs(role: &str) -> Result<(), InitError> {
    let path = format!("/usr/lib/cluster/role-kargs-{role}.conf");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| InitError::Io(format!("reading {path}: {e}")))?;
    let options = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("options="))
        .unwrap_or_default()
        .trim()
        .to_string();

    let mut command = Command::new("bootc");
    command.args([
        "loader-entries",
        "set-options-for-source",
        "--source",
        "cluster-role",
    ]);
    if !options.is_empty() {
        command.args(["--options", &options]);
    }
    let output = command
        .output()
        .map_err(|e| InitError::Io(format!("running bootc loader-entries: {e}")))?;
    if !output.status.success() {
        return Err(InitError::Io(format!(
            "bootc loader-entries refused the `{role}` kernel arguments: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn wire(config: &Config) -> Result<Wire, InitError> {
    let raw = config.string("discovery_group")?;
    let group: Ipv6Addr = raw
        .parse()
        .map_err(|_| InitError::Config(format!("`discovery_group` is `{raw}`")))?;
    Ok(Wire {
        group,
        port: config.number("discovery_port")? as u16,
        interval: Duration::from_millis(u64::from(config.number("discovery_interval_ms")?)),
        timeout: Duration::from_secs(u64::from(config.number("discovery_timeout_s")?)),
    })
}

/// The registrar's half: announce, hand out places, and learn who is on each
/// cable (§2.3.2, §3.3).
///
/// It serves and discovers in the same pass because they are the same traffic. A
/// machine asking for a place is also telling the registrar which cable it is
/// on, and a second pass would be a second chance for the answer to change.
fn serve_and_discover(
    config: &Config,
    classified: &Classified,
    machine_id: &str,
    ordinal: u32,
    role_id: &str,
    wire: &Wire,
) -> Result<Vec<PeeredPort>, InitError> {
    let secret = load_or_create_secret()?;
    let mut registry: Registry = match std::fs::read_to_string(REGISTRY_PATH) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| InitError::Registry(format!("reading {REGISTRY_PATH}: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
        Err(e) => return Err(InitError::Io(format!("reading {REGISTRY_PATH}: {e}"))),
    };

    let order: Vec<String> = config
        .assigned_roles()
        .iter()
        .map(|r| r.id.to_string())
        .collect();
    let first_free = ordinal + 1;
    let fleet_size = config.number("fleet_size")?;

    let own = Announcement {
        machine_id: machine_id.to_string(),
        ordinal: Some(ordinal),
        role: Some(role_id.to_string()),
        is_registrar: true,
    };

    let mut peers = Vec::new();
    for port in &classified.mesh {
        let until = Instant::now() + wire.timeout;
        let granted = net::serve_grants(&port.name, &own, wire, &secret, until, |id| {
            let assignment = registry.register(id, &order, first_free, fleet_size)?;
            Ok((assignment.ordinal, assignment.role))
        })?;
        // Persist before the next port. A grant that was sent and not recorded
        // would be handed out again to a different machine after a reboot, and
        // two machines would answer to one name.
        write_private(
            Path::new(REGISTRY_PATH),
            &serde_json::to_string_pretty(&registry)
                .map_err(|e| InitError::Registry(e.to_string()))?,
        )?;

        let peer_ordinal = match granted.first() {
            Some(grant) => grant.ordinal,
            // Nothing asked on this cable, so either the machine on it already
            // has a place --- ask it who it is --- or there is no machine on it
            // yet.
            None => match net::discover_peer(&port.name, &own, wire) {
                Ok(found) => discovery_ordinal(&found.peer)?,
                // **Not fatal, and this is the difference between the registrar
                // and everyone else.** The registrar knows its ordinal from its
                // own disks (§2.3.1), so a cable with nothing on the far end
                // costs it that one link's addresses and nothing else. §12.1
                // promises exactly this: a machine powered on before the others
                // comes up and they join it, rather than the first machine
                // refusing to boot because it is first.
                //
                // A machine that is *not* the registrar cannot do this. It has
                // no ordinal without an answer, so `join` below treats the same
                // silence as fatal.
                Err(InitError::Discovery(reason)) => {
                    eprintln!(
                        "cluster-init: {} has no peer yet ({reason}); leaving it \
                         unaddressed. The link takes its addresses when the machine \
                         on the far end registers (§3.3, §12.1)",
                        port.name
                    );
                    continue;
                }
                Err(e) => return Err(e),
            },
        };
        peers.push(PeeredPort {
            port: port.clone(),
            peer_ordinal,
        });
    }
    Ok(peers)
}

/// Every other machine's half: find the registrar, ask for a place, then learn
/// who is on the remaining cable (§2.3.2, §3.3).
fn join(
    classified: &Classified,
    machine_id: &str,
    wire: &Wire,
) -> Result<(discovery::Grant, Vec<PeeredPort>), InitError> {
    let own = Announcement {
        machine_id: machine_id.to_string(),
        ordinal: None,
        role: None,
        is_registrar: false,
    };

    // Which cable reaches the registrar is not knowable in advance, so every
    // mesh port is asked (§3.3).
    let mut found = Vec::new();
    for port in &classified.mesh {
        found.push(net::discover_peer(&port.name, &own, wire)?);
    }
    let registrar = discovery::registrar_among(&found)?.ok_or_else(|| {
        InitError::Registry(
            "no peer on either mesh port is the registrar. A machine holding no bulk disk \
             cannot assign itself, and §2.3.1's predicate was true on no machine this one \
             can reach (§21.11)"
                .into(),
        )
    })?;

    let grant = net::request_place(&registrar.interface, &own, wire)?;

    // Now that this machine has an ordinal, the remaining cable's peer is
    // whoever answered on it.
    let mut peers = Vec::new();
    for discovered in &found {
        let port = classified
            .mesh
            .iter()
            .find(|p| p.name == discovered.interface)
            .ok_or_else(|| {
                InitError::Discovery(format!("{} is not a mesh port", discovered.interface))
            })?;
        peers.push(PeeredPort {
            port: port.clone(),
            peer_ordinal: discovery_ordinal(&discovered.peer)?,
        });
    }
    Ok((grant, peers))
}

/// A peer's ordinal, or a refusal.
///
/// A peer that has not registered yet has no ordinal, and there is no address to
/// put on the cable that reaches it. Waiting is the caller's job; guessing is
/// nobody's.
fn discovery_ordinal(peer: &Announcement) -> Result<u32, InitError> {
    peer.ordinal.ok_or_else(|| {
        InitError::Discovery(format!(
            "the machine on the far end ({}) has no ordinal yet, so this cable has no \
             addresses. Which addresses a link carries follows from which two ordinals it \
             joins (§4.1)",
            peer.machine_id
        ))
    })
}

/// The join secret, generated once on the registrar's first boot (§12.2).
fn load_or_create_secret() -> Result<String, InitError> {
    match std::fs::read_to_string(SECRET_PATH) {
        Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        Ok(_) | Err(_) => {
            let secret = net::generate_secret(32)?;
            write_private(Path::new(SECRET_PATH), &secret)?;
            Ok(secret)
        }
    }
}

/// Write a file only root can read.
///
/// The secret and the registry both go through here. `0600` set *before* the
/// content lands, because a file created world-readable and narrowed afterwards
/// is world-readable for the width of that window.
fn write_private(path: &Path, content: &str) -> Result<(), InitError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| InitError::Io(format!("writing {}: {e}", path.display())))?;
    file.write_all(content.as_bytes())
        .map_err(|e| InitError::Io(format!("writing {}: {e}", path.display())))?;
    Ok(())
}
