//! Sorting a machine's ports into classes (`SPEC.md` §3.1).
//!
//! **Supported link modes, never negotiated speed.** A mesh port is down exactly
//! when the peer it waits for has not booted yet, and a down port reports no
//! speed at all --- `/sys/class/net/<if>/speed` returns `-1`. Classifying on that
//! would put every unplugged 10GbE port in the LAN class on a cold fleet, which
//! is precisely the boot this has to survive. Supported modes are a property of
//! the card and are readable whether or not anything is plugged in.
//!
//! The classification itself is pure and testable; reading the modes off a real
//! card is the thin part around it. That split is deliberate: the interesting
//! failure is a rule that sorts wrongly, not a file that fails to open.

use crate::InitError;

/// A port as the kernel presents it, before it has been sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// The kernel's name for it, e.g. `enp3s0f0`. Used only in the `[Match]` of
    /// the unit this produces --- it is not an identity and nothing persists it.
    pub name: String,
    /// The highest speed the driver reports *support* for, in Mbps.
    pub max_supported_mbps: u32,
}

/// Which class a port belongs to (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// 10GBase-T, direct-attached to another node.
    Mesh,
    /// 1GbE, to the switch.
    Lan,
}

/// A machine's ports, sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// Mesh ports, in kernel enumeration order.
    pub mesh: Vec<Port>,
    /// LAN ports, in kernel enumeration order.
    pub lan: Vec<Port>,
}

/// The thresholds and counts a conforming machine is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// Lowest supported speed that puts a port in the mesh class.
    pub mesh_min_mbps: u32,
    /// How many mesh ports a conforming machine presents.
    pub mesh_count: u32,
    /// Lowest supported speed that puts a port in the LAN class.
    pub lan_min_mbps: u32,
    /// How many LAN ports a conforming machine presents, at minimum.
    pub lan_count: u32,
}

/// Sort ports into classes, and refuse a machine that is not conforming.
///
/// **Refusing is the point.** A machine with one mesh port would join the
/// cluster with no redundancy, pass every health check, and behave exactly like
/// a healthy node until the day its one cable failed. Nothing downstream can
/// detect that, so it is detected here and the boot fails instead (§3.1).
pub fn classify(ports: &[Port], t: Thresholds) -> Result<Classified, InitError> {
    let mut mesh = Vec::new();
    let mut lan = Vec::new();
    for port in ports {
        if port.max_supported_mbps >= t.mesh_min_mbps {
            mesh.push(port.clone());
        } else if port.max_supported_mbps >= t.lan_min_mbps {
            lan.push(port.clone());
        }
        // Anything slower than the LAN threshold is not a port this cluster
        // uses. It is left alone rather than configured: an address on a port
        // nobody declared is an address nobody expected.
    }

    if mesh.len() as u32 != t.mesh_count {
        return Err(InitError::Hardware(format!(
            "{} mesh-class port(s), and a conforming machine presents {}. A node that came \
             up with fewer would join with no redundancy and nothing would say so; one that \
             came up with more is wired into a topology this cluster does not have (§3.1)",
            mesh.len(),
            t.mesh_count
        )));
    }
    if (lan.len() as u32) < t.lan_count {
        return Err(InitError::Hardware(format!(
            "{} LAN-class port(s), and a conforming machine presents at least {} (§3.1)",
            lan.len(),
            t.lan_count
        )));
    }
    Ok(Classified { mesh, lan })
}

/// The highest supported speed in `ethtool`'s output for one interface.
///
/// Parses the `Supported link modes:` block, whose entries look like
/// `10000baseT/Full`. The negotiated `Speed:` line is deliberately not read ---
/// see this module's header for why.
pub fn max_supported_mbps(ethtool_output: &str) -> u32 {
    let mut best = 0;
    let mut in_block = false;
    for line in ethtool_output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Supported link modes:") {
            in_block = true;
        } else if in_block && trimmed.contains(':') {
            // The block ends at the next `Key: value` line. Continuation lines
            // carry only modes, which is what makes this terminable at all.
            in_block = false;
        }
        if !in_block {
            continue;
        }
        for token in trimmed.split_whitespace() {
            let Some(digits) = token.split("base").next() else {
                continue;
            };
            if !token.contains("base") {
                continue;
            }
            if let Ok(mbps) = digits.parse::<u32>() {
                best = best.max(mbps);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> Thresholds {
        Thresholds {
            mesh_min_mbps: 10000,
            mesh_count: 2,
            lan_min_mbps: 1000,
            lan_count: 1,
        }
    }

    fn port(name: &str, mbps: u32) -> Port {
        Port {
            name: name.to_string(),
            max_supported_mbps: mbps,
        }
    }

    /// The board this cluster is built on: two 10GbE, two 1GbE (§2.1).
    #[test]
    fn a_conforming_machine_sorts_into_two_and_two() {
        let ports = [
            port("enp3s0f0", 10000),
            port("enp3s0f1", 10000),
            port("eno1", 1000),
            port("eno2", 1000),
        ];
        let out = classify(&ports, thresholds()).expect("it conforms");
        assert_eq!(out.mesh.len(), 2);
        assert_eq!(out.lan.len(), 2, "the spare is classified, not configured");
    }

    /// The failure this exists for. One mesh port passes every health check and
    /// behaves like a healthy node until its one cable fails.
    #[test]
    fn one_mesh_port_fails_the_boot() {
        let ports = [port("enp3s0f0", 10000), port("eno1", 1000)];
        let err = classify(&ports, thresholds()).expect_err("it does not conform");
        assert!(
            format!("{err}").contains("no redundancy"),
            "the error says what it costs: {err}"
        );
    }

    #[test]
    fn no_lan_port_fails_the_boot() {
        let ports = [port("enp3s0f0", 10000), port("enp3s0f1", 10000)];
        assert!(classify(&ports, thresholds()).is_err());
    }

    /// A port slower than every class is left alone. Configuring it would put an
    /// address on a port the model never declared.
    #[test]
    fn a_port_below_every_threshold_is_left_alone() {
        let ports = [
            port("enp3s0f0", 10000),
            port("enp3s0f1", 10000),
            port("eno1", 1000),
            port("usb0", 100),
        ];
        let out = classify(&ports, thresholds()).expect("it conforms");
        assert_eq!(out.lan.len(), 1, "the 100 Mbps adapter is in no class");
    }

    /// The whole reason this reads *supported* modes: on a cold fleet every mesh
    /// port is down, and a classifier keyed on negotiated speed would sort all
    /// four of these into the LAN class and fail the boot of a perfectly good
    /// machine.
    #[test]
    fn a_down_10g_port_still_classifies_as_mesh() {
        let down = "\
Settings for enp3s0f0:
	Supported ports: [ TP ]
	Supported link modes:   100baseT/Full
	                        1000baseT/Full
	                        10000baseT/Full
	Supported pause frame use: Symmetric
	Speed: Unknown!
	Duplex: Unknown! (255)
	Link detected: no
";
        assert_eq!(max_supported_mbps(down), 10000);
    }

    #[test]
    fn a_1g_card_reports_1g() {
        let out = "\
Settings for eno1:
	Supported link modes:   10baseT/Half 10baseT/Full
	                        100baseT/Half 100baseT/Full
	                        1000baseT/Full
	Speed: 1000Mb/s
	Link detected: yes
";
        assert_eq!(max_supported_mbps(out), 1000);
    }

    /// The `Speed:` line must not leak into the answer. `Speed: 10000Mb/s`
    /// contains no `base`, and a parser that fell back to it would defeat the
    /// point of reading supported modes at all.
    #[test]
    fn the_negotiated_speed_line_is_not_read() {
        let out = "\
Settings for eno1:
	Supported link modes:   1000baseT/Full
	Speed: 10000Mb/s
";
        assert_eq!(
            max_supported_mbps(out),
            1000,
            "the negotiated speed is not what the card supports"
        );
    }
}
