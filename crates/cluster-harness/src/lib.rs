//! QEMU orchestration and the tier collector (`SPEC.md` §9.4, §10.2, §10.3).
//!
//! # The topology is built, not scripted
//!
//! QEMU socket netdevs give point-to-point links with no bridges, taps, or
//! privilege: one guest listens, the other connects, and the pair is a wire.
//! Three pairs reproduce §4.1's `/31` triangle exactly, which is what makes T2 a
//! faithful test of the *rendered* routing configuration rather than of a
//! simplified stand-in.
//!
//! [`qemu_args`] is a pure function from the model to a command line, so the
//! claim that the harness reproduces the declared topology is checkable without
//! starting anything.
//!
//! # A skip is explicit, never a silent fallback
//!
//! GitHub documents nested virtualization on hosted runners as possible but not
//! supported, with `/dev/kvm` availability reported as inconsistent. This
//! harness probes for it and reports absence as an **explicit skip** (§9.4). It
//! never falls back to TCG: a tier that quietly emulated would be slower,
//! differently timed, and --- worst --- would look green.
//!
//! # A hardware claim cannot be discharged by a guest
//!
//! [`collect`] filters the register by tier, and refuses to hand a `CH-`
//! scenario to a simulated run. That is the class rule §19.2 anticipates for the
//! first `CH-` row, enforced rather than left to each tier to remember: a `CH-`
//! claim discharged by a QEMU guest would be a false statement about a physical
//! machine (§21.2).

#![deny(missing_docs)]

pub mod guest;
pub mod image;

use std::path::Path;

use cluster_model::{Cluster, Node};
use repo_model::{class_of, IdRow, Model, Tier};

/// The class that only real hardware can discharge (`SPEC.md` §19.2, §21.2).
pub const HARDWARE_CLASS: &str = "CH";

/// Base port for the socket netdevs that carry the mesh.
///
/// Each link takes one port. They are derived from the link's index in the
/// model rather than assigned here, so adding a link cannot silently collide
/// with one already in use.
pub const MESH_PORT_BASE: u16 = 11_200;

/// Whether the machine running the harness can give a guest real KVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceleration {
    /// `/dev/kvm` is present. The tier runs.
    Kvm,
    /// It is not. The tier is **skipped, explicitly** --- never emulated.
    Absent,
}

impl Acceleration {
    /// Probe for `/dev/kvm`.
    pub fn probe() -> Self {
        Self::probe_at(Path::new("/dev/kvm"))
    }

    /// Probe a given path, so the decision itself is testable.
    pub fn probe_at(path: &Path) -> Self {
        if path.exists() {
            Self::Kvm
        } else {
            Self::Absent
        }
    }

    /// The line printed when a tier does not run.
    ///
    /// Loud, and it names the consequence. A skip that reads like a pass is the
    /// vacuous gate `AGENTS.md` warns about wearing a different hat: the run is
    /// green and nothing was tested.
    pub const SKIP_NOTICE: &'static str = concat!(
        "SKIPPED: /dev/kvm is absent, so no guest was booted and nothing in this ",
        "tier was tested. The harness does not fall back to TCG: a tier that ",
        "quietly emulated would be slower, differently timed, and would look ",
        "green (SPEC.md §9.4). T2 runs on n1, where KVM is guaranteed, so ",
        "nothing is promoted on a skipped tier."
    );

    /// Whether a guest can actually be booted.
    pub const fn can_boot(self) -> bool {
        matches!(self, Self::Kvm)
    }
}

/// One end of a mesh link, as QEMU sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Netdev {
    /// The netdev id, which is the link id from the model.
    pub id: String,
    /// Whether this guest listens or connects. Exactly one end does each.
    pub role: SocketRole,
    /// The TCP port carrying the link.
    pub port: u16,
}

/// Which end of a QEMU socket pair a guest takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketRole {
    /// `listen=:PORT`. Taken by the link's `a` end.
    Listen,
    /// `connect=127.0.0.1:PORT`. Taken by the link's `b` end.
    Connect,
}

impl Netdev {
    /// The `-netdev` argument QEMU takes.
    pub fn argument(&self) -> String {
        match self.role {
            SocketRole::Listen => format!("socket,id={},listen=:{}", self.id, self.port),
            SocketRole::Connect => {
                format!("socket,id={},connect=127.0.0.1:{}", self.id, self.port)
            }
        }
    }
}

/// The mesh netdevs one guest takes, in link order.
///
/// A `/31` has exactly two ends, so exactly one guest listens on each link and
/// exactly one connects. Deriving the roles from the model's `a`/`b` rather than
/// assigning them here means the wire cannot disagree with the addressing:
/// whoever holds the even address is the one that listens.
pub fn mesh_netdevs(cluster: &Cluster, node: &Node) -> Vec<Netdev> {
    cluster
        .network
        .link
        .iter()
        .enumerate()
        .filter_map(|(index, link)| {
            let role = if link.a == node.name {
                SocketRole::Listen
            } else if link.b == node.name {
                SocketRole::Connect
            } else {
                return None;
            };
            Some(Netdev {
                id: link.id.clone(),
                role,
                port: MESH_PORT_BASE + index as u16,
            })
        })
        .collect()
}

/// The full QEMU command line for one guest.
///
/// OVMF supplies UEFI, because bootc images boot via EFI and a BIOS guest would
/// be testing a boot path no node uses. A user-mode netdev provides management
/// and outbound, so a guest can reach a registry without the harness needing a
/// bridge or any privilege at all.
pub fn qemu_args(cluster: &Cluster, node: &Node, disk: &str, ovmf: &str) -> Vec<String> {
    let mut args = vec![
        "-machine".to_string(),
        "q35,accel=kvm".to_string(),
        "-cpu".to_string(),
        "host".to_string(),
        "-m".to_string(),
        "4096".to_string(),
        "-smp".to_string(),
        "2".to_string(),
        "-nographic".to_string(),
        // Matches `console=ttyS1,115200` in the image kargs and the COM2/SOL
        // redirection the firmware table declares (§2.4, §8.1).
        "-serial".to_string(),
        "mon:stdio".to_string(),
        "-drive".to_string(),
        format!("if=pflash,format=raw,readonly=on,file={ovmf}"),
        // One base qcow2 with per-node copy-on-write overlays. Hosted runners
        // provide roughly 14 GB free, which does not hold three bootc disk
        // images (§9.4).
        "-drive".to_string(),
        format!("file={disk},format=qcow2,if=virtio"),
    ];

    for netdev in mesh_netdevs(cluster, node) {
        args.push("-netdev".to_string());
        args.push(netdev.argument());
        args.push("-device".to_string());
        args.push(format!("virtio-net-pci,netdev={}", netdev.id));
    }

    // Management and outbound, with no bridge, tap, or privilege.
    args.push("-netdev".to_string());
    args.push(format!("user,id=mgmt,hostfwd=tcp::{}-:22", ssh_port(node)));
    args.push("-device".to_string());
    args.push("virtio-net-pci,netdev=mgmt".to_string());

    args
}

/// The host port forwarded to a guest's SSH, derived from its rollout position
/// so two guests cannot collide.
pub fn ssh_port(node: &Node) -> u16 {
    2_200 + node.update_position as u16
}

/// Which registered claims a tier may discharge.
///
/// Two rules, and the second is the class rule §19.2 anticipates:
///
/// - a tier discharges the claims registered at that tier;
/// - **no simulated tier ever collects a `CH-` claim**, whatever the register
///   says, because a hardware claim discharged by a QEMU guest would be a false
///   statement about a physical machine (§21.2).
///
/// The second rule is belt and braces on purpose. `check-model` already refuses
/// a `CH-` row registered below T3, so this filter should never have anything to
/// remove --- and if it ever does, the register and the collector disagree, which
/// is worth failing over rather than quietly reconciling.
pub fn collect(model: &Model, tier: Tier) -> Vec<&IdRow> {
    model
        .ids
        .id
        .iter()
        .filter(|row| row.tier == tier)
        .filter(|row| tier == Tier::T3 || class_of(&row.id) != Some(HARDWARE_CLASS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cluster() -> Cluster {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crates/cluster-harness is two below the root")
            .to_path_buf();
        let c = Cluster::load(&root.join("model")).expect("the cluster model loads");
        c.check().expect("the cluster model is consistent");
        c
    }

    /// `CM-04`: no simulated tier collects a hardware claim.
    #[test]
    fn a_simulated_tier_collects_no_hardware_claim_cm_04() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("two below the root")
            .to_path_buf();
        let model = Model::load(&root.join("model")).expect("the model loads");

        for tier in [Tier::T0, Tier::T1, Tier::T2] {
            for row in collect(&model, tier) {
                assert_ne!(
                    class_of(&row.id),
                    Some(HARDWARE_CLASS),
                    "{} was collected at {}, but only real nodes can establish a \
                     hardware claim (§19.2, §21.2)",
                    row.id,
                    tier.as_str()
                );
            }
        }

        // And every registered claim is collected by exactly one tier, so the
        // filter above is not passing because it collects nothing.
        for row in &model.ids.id {
            let collected: Vec<&str> = [Tier::T0, Tier::T1, Tier::T2, Tier::T3]
                .into_iter()
                .filter(|t| collect(&model, *t).iter().any(|r| r.id == row.id))
                .map(|t| t.as_str())
                .collect();
            assert_eq!(
                collected.len(),
                1,
                "{} is collected by {collected:?}; each claim is discharged at one tier",
                row.id
            );
        }
    }

    /// The harness reproduces §4.1's triangle: one listener and one connector
    /// per link, and every link carried exactly once.
    #[test]
    fn the_socket_pairs_reproduce_the_declared_triangle_cm_04() {
        let c = cluster();
        let mut listens: Vec<(String, u16)> = Vec::new();
        let mut connects: Vec<(String, u16)> = Vec::new();

        for node in &c.cluster.node {
            let netdevs = mesh_netdevs(&c, node);
            assert_eq!(
                netdevs.len(),
                2,
                "{} must carry two mesh links; each node has two 10 GbE ports (§1.1)",
                node.name
            );
            for netdev in netdevs {
                match netdev.role {
                    SocketRole::Listen => listens.push((netdev.id, netdev.port)),
                    SocketRole::Connect => connects.push((netdev.id, netdev.port)),
                }
            }
        }

        assert_eq!(listens.len(), c.network.link.len());
        assert_eq!(connects.len(), c.network.link.len());
        // Exactly one of each end per link, on the same port: a socket pair is a
        // wire only if both ends agree on which wire it is.
        for link in &c.network.link {
            let listening: Vec<&(String, u16)> =
                listens.iter().filter(|(id, _)| id == &link.id).collect();
            let connecting: Vec<&(String, u16)> =
                connects.iter().filter(|(id, _)| id == &link.id).collect();
            assert_eq!(listening.len(), 1, "link {} needs one listener", link.id);
            assert_eq!(connecting.len(), 1, "link {} needs one connector", link.id);
            assert_eq!(
                listening[0].1, connecting[0].1,
                "link {}'s ends must agree on the port",
                link.id
            );
        }

        // No two links share a port.
        let mut ports: Vec<u16> = listens.iter().map(|(_, p)| *p).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), c.network.link.len());
    }

    /// The command line boots UEFI and forwards a distinct SSH port per guest.
    #[test]
    fn the_command_line_boots_uefi_and_forwards_ssh_cm_04() {
        let c = cluster();
        let mut ssh_ports = Vec::new();
        for node in &c.cluster.node {
            let args = qemu_args(&c, node, "n.qcow2", "/usr/share/OVMF/OVMF_CODE.fd");
            let line = args.join(" ");
            assert!(line.contains("pflash"), "OVMF supplies UEFI (§10.3)");
            assert!(
                line.contains("accel=kvm"),
                "never a silent TCG fallback (§9.4)"
            );
            assert!(line.contains(&format!("hostfwd=tcp::{}-:22", ssh_port(node))));
            ssh_ports.push(ssh_port(node));
        }
        ssh_ports.sort_unstable();
        ssh_ports.dedup();
        assert_eq!(
            ssh_ports.len(),
            c.cluster.node.len(),
            "ports must not collide"
        );
    }

    /// `/dev/kvm`'s absence is an explicit skip that names the consequence.
    #[test]
    fn an_absent_accelerator_is_an_explicit_skip_cm_04() {
        assert_eq!(
            Acceleration::probe_at(Path::new("/definitely/not/a/device")),
            Acceleration::Absent
        );
        assert!(!Acceleration::Absent.can_boot());

        let notice = Acceleration::SKIP_NOTICE;
        assert!(notice.starts_with("SKIPPED"));
        assert!(notice.contains("nothing in this tier was tested"));
        assert!(notice.contains("TCG"));
        // And it says why nothing is promoted on a skipped tier, which is the
        // fact that makes the skip acceptable rather than merely admitted.
        assert!(notice.contains("nothing is promoted"));
    }
}
