//! `cluster-init`: what a machine works out about itself at boot (`SPEC.md`
//! §2.3, §3.1, §3.3, §4.1).
//!
//! **Two halves, and the split is what keeps a boot short.**
//!
//! `cluster-init` runs once, ordered before `systemd-networkd`, because the
//! units it writes are the ones networkd will read:
//!
//! 1. read the rendered policy (`init.conf`), which carries every threshold;
//! 2. classify this machine's ports by supported link speed (§3.1);
//! 3. decide from its own disks whether it is the storage node (§2.3.1);
//! 4. take the ordinal that entitles it to, or ask the registrar for one (§2.3.2);
//! 5. derive the loopback and LAN addresses and write those units (§4.1);
//! 6. write the role marker, the node environment, and the role's firewall
//!    include, then apply the role's kernel arguments (§8.4, §8.5).
//!
//! `cluster-init peers` runs continuously afterwards, ordered before nothing. It
//! addresses each mesh port as the machine on the far end appears.
//!
//! The mesh is the half that has to wait on hardware somebody else has to power
//! on, and a boot must never do that. Doing both in one boot-blocking unit meant
//! the first machine of a fleet sat through the discovery timeout on each of its
//! ports before `sshd` started --- ten minutes of a guest that had booted
//! perfectly and could not be reached, which is the opposite of §12.1's promise
//! that a fleet can be powered on in any order.
//!
//! The one wait that *is* a boot's business belongs to a machine holding no bulk
//! disk: it has no ordinal, no role and no address until the registrar answers,
//! so there is nothing it could usefully do first.
//!
//! Every step that can decide something wrongly lives in the library beside this
//! file and is tested there. What is here is the ordering and the I/O.
//!
//! **A step that cannot complete fails the boot.** There is no partial mode: a
//! node with one mesh port, two bulk disks, or no ordinal has nothing safe to do
//! next, and a node that started its services anyway would look healthy while
//! being wrong (§3.1, §21.11).

use std::collections::BTreeMap;
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
    InitError, APPLIED_KARGS_PATH, POLICY_PATH, REGISTRY_PATH, RUNTIME_DIR, RUNTIME_NETWORK_DIR,
    SECRET_PATH,
};

fn main() -> ExitCode {
    // `cluster-init` at boot, `cluster-init peers` continuously afterwards.
    //
    // The split is what keeps a boot short. Peer discovery waits on machines
    // that may not be powered on yet, and the registrar has nothing to wait
    // *for*: it knows its ordinal from its own disks. Doing both in one
    // boot-blocking unit meant the first machine of a fleet sat through the
    // whole discovery timeout on each mesh port before `sshd` could start ---
    // ten minutes of a guest that had booted perfectly and could not be reached,
    // which is the opposite of what §12.1 promises.
    let peers_only = std::env::args().nth(1).as_deref() == Some("peers");
    match if peers_only { peers() } else { run() } {
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

    // The registrar knows its ordinal without asking, so it does not wait here
    // at all --- the mesh ports take their addresses in `cluster-peers`, as the
    // machines on the far end register. Every other machine genuinely cannot
    // proceed without an answer: it has no ordinal, no role and no address, so
    // it waits, bounded by the discovery timeout (§2.3.1, §2.3.2, §12.1).
    let (ordinal, role_id) = if is_registrar {
        let ordinal = detected.ordinal.ok_or_else(|| {
            InitError::Config("the self-detected role pins no ordinal (§2.3.2)".into())
        })?;
        // Create the join secret now, so it exists before anything asks for it.
        load_or_create_secret()?;
        (ordinal, detected.id.clone())
    } else {
        let grant = join_for_ordinal(&classified, &machine_id, &wire)?;
        // The join secret never touches a rendered artifact or an image. It
        // arrives here and goes to a file only this node's root can read (§12.2).
        write_private(Path::new(SECRET_PATH), &grant.secret)?;
        (grant.ordinal, grant.role)
    };
    // No mesh addresses yet: which ordinal is on which cable is `cluster-peers`'
    // question, and it is the one question that has to wait for another machine.
    let peers: Vec<PeeredPort> = Vec::new();

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
/// **Only when they have changed.** That call *stages a new deployment*, so
/// running it unconditionally would stage one on every boot of every machine ---
/// including the two roles whose set is empty, to remove a source that was never
/// there. What was applied last is recorded beside the registry, and a boot that
/// would set the same thing does nothing at all.
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

    let applied = Path::new(APPLIED_KARGS_PATH);
    let last = std::fs::read_to_string(applied).unwrap_or_default();
    if last.trim() == options {
        return Ok(());
    }

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
    // Recorded only after it succeeded, so a failed apply is retried rather than
    // remembered as done.
    write_private(applied, &options)?;
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

/// The continuous half: keep the mesh addressed as machines appear (§3.3, §12.1).
///
/// Run by `cluster-peers.service` after networkd, restarted for ever. It is
/// deliberately *not* ordered before anything: waiting on a machine that may not
/// be powered on yet is exactly what must never block a boot, and the first
/// machine of a fleet is necessarily alone.
///
/// Each pass does one thing per mesh port: if this node is the registrar it
/// answers whatever asks, and either way it learns which ordinal is on the far
/// end and writes that port's `.network` unit. A port whose peer has not
/// appeared is left unaddressed and asked again next pass.
fn peers() -> Result<String, InitError> {
    let policy = std::fs::read_to_string(POLICY_PATH)
        .map_err(|e| InitError::Io(format!("reading {POLICY_PATH}: {e}")))?;
    let config = Config::parse(&policy)?;
    let classified = links::classify(&observed_ports()?, thresholds(&config)?)?;
    let addressing = addressing(&config)?;
    let wire = wire(&config)?;

    // What this node already worked out about itself, at boot.
    let env = std::fs::read_to_string(Path::new(RUNTIME_DIR).join("node.env"))
        .map_err(|e| InitError::Io(format!("reading the node environment: {e}")))?;
    let value = |key: &str| -> Option<String> {
        env.lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
            .map(str::to_string)
    };
    let ordinal: u32 = value("CLUSTER_ORDINAL")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            InitError::Config(
                "the node environment carries no ordinal, so cluster-init has not run \
                 (§2.3.2)"
                    .into(),
            )
        })?;
    let role_id = value("CLUSTER_ROLE")
        .ok_or_else(|| InitError::Config("the node environment carries no role (§2.3)".into()))?;
    let machine_id = std::fs::read_to_string(config.string("machine_id_path")?)
        .map_err(|e| InitError::Io(format!("reading the machine id: {e}")))?
        .trim()
        .to_string();
    let is_registrar = config
        .self_detected_role()
        .map(|r| r.id == role_id)
        .unwrap_or(false);

    let own = Announcement {
        machine_id: machine_id.clone(),
        ordinal: Some(ordinal),
        role: Some(role_id.clone()),
        is_registrar,
    };

    let secret = if is_registrar {
        load_or_create_secret()?
    } else {
        String::new()
    };
    let order: Vec<String> = config
        .assigned_roles()
        .iter()
        .map(|r| r.id.to_string())
        .collect();
    let fleet_size = config.number("fleet_size")?;
    let first_free = config
        .self_detected_role()
        .ok()
        .and_then(|r| r.ordinal)
        .unwrap_or(1)
        + 1;

    let metrics = Metrics {
        direct: config.number("direct_metric")?,
        transit: config.number("transit_metric")?,
    };
    let mesh_mtu = config.number("mesh_mtu")?;

    let mut known: BTreeMap<String, u32> = BTreeMap::new();
    for (index, port) in classified.mesh.iter().enumerate() {
        if known.contains_key(&port.name) {
            continue;
        }

        // One short window per port per pass. Short, because a pass that waited
        // the whole discovery timeout on a fleet of one would answer nothing for
        // five minutes and then do it again --- and this unit restarts for ever,
        // so patience belongs in the restart rather than in the wait.
        let window = wire.interval * 4;
        let brief = Wire {
            timeout: window,
            ..wire.clone()
        };

        if is_registrar {
            let mut registry: Registry = match std::fs::read_to_string(REGISTRY_PATH) {
                Ok(text) => serde_json::from_str(&text)
                    .map_err(|e| InitError::Registry(format!("reading {REGISTRY_PATH}: {e}")))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
                Err(e) => return Err(InitError::Io(format!("reading {REGISTRY_PATH}: {e}"))),
            };
            let until = Instant::now() + window;
            let granted = net::serve_grants(&port.name, &own, &brief, &secret, until, |id| {
                let assignment = registry.register(id, &order, first_free, fleet_size)?;
                Ok((assignment.ordinal, assignment.role))
            })?;
            // Persisted before the address is written. A grant that was sent and
            // not recorded would be handed out again to a different machine after
            // a reboot, and two machines would answer to one name.
            write_private(
                Path::new(REGISTRY_PATH),
                &serde_json::to_string_pretty(&registry)
                    .map_err(|e| InitError::Registry(e.to_string()))?,
            )?;
            if let Some(grant) = granted.first() {
                known.insert(port.name.clone(), grant.ordinal);
            }
        }

        if !known.contains_key(&port.name) {
            match net::discover_peer(&port.name, &own, &brief) {
                Ok(found) => {
                    if let Some(peer) = found.peer.ordinal {
                        known.insert(port.name.clone(), peer);
                    }
                }
                // Nothing on this cable yet. Not an error: the machine on the
                // far end may not have been powered on, which is the case §12.1
                // exists to survive.
                Err(InitError::Discovery(_)) => continue,
                Err(e) => return Err(e),
            }
        }

        let Some(peer_ordinal) = known.get(&port.name).copied() else {
            continue;
        };
        let peered = PeeredPort {
            port: port.clone(),
            peer_ordinal,
        };
        let units = units::mesh_units(&[peered], ordinal, &addressing, mesh_mtu, metrics)?;
        for (_, body) in &units {
            std::fs::write(
                Path::new(RUNTIME_NETWORK_DIR).join(units::mesh_unit_name(index)),
                body,
            )?;
        }
    }

    if !known.is_empty() {
        // networkd reads what is under /run, so it has to be told the set
        // changed. A reload rather than a restart: restarting would drop every
        // address on the machine to add one.
        let reload = Command::new("networkctl")
            .arg("reload")
            .output()
            .map_err(|e| InitError::Io(format!("running networkctl reload: {e}")))?;
        if !reload.status.success() {
            return Err(InitError::Io(format!(
                "networkctl reload refused the new units: {}",
                String::from_utf8_lossy(&reload.stderr).trim()
            )));
        }
    }

    Ok(format!(
        "{} of {} mesh port(s) addressed",
        known.len(),
        classified.mesh.len()
    ))
}

/// Ask the registrar for a place, waiting on whichever cable reaches it.
///
/// This is the one wait a boot legitimately blocks on: a machine holding no bulk
/// disk has no ordinal, no role and no address until the registrar answers, so
/// there is nothing it could usefully do first (§2.3.2). It is bounded by the
/// discovery timeout, which §12.1 sets longer than a cold boot.
fn join_for_ordinal(
    classified: &Classified,
    machine_id: &str,
    wire: &Wire,
) -> Result<discovery::Grant, InitError> {
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
        match net::discover_peer(&port.name, &own, wire) {
            Ok(peer) => found.push(peer),
            // A cable with nothing on it is not an answer, and not yet a
            // failure: the registrar may be on the other one.
            Err(InitError::Discovery(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    let registrar = discovery::registrar_among(&found)?.ok_or_else(|| {
        InitError::Registry(
            "no peer on either mesh port is the registrar. A machine holding no bulk disk \
             cannot assign itself, and §2.3.1's predicate was true on no machine this one \
             can reach (§21.11)"
                .into(),
        )
    })?;

    net::request_place(&registrar.interface, &own, wire)
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
