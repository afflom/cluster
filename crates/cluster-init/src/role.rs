//! Which role a machine holds, and how it comes to hold it (`SPEC.md` §2.3).
//!
//! One role is worked out from the machine's own disks; the other two are handed
//! out by the machine that worked its own out. Both halves are here, and both
//! are pure functions over what was measured --- the measuring is elsewhere, so
//! that the decisions can be tested without a disk or a network.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::InitError;

/// A block device as the machine reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The kernel path, e.g. `/dev/sdb`.
    pub path: String,
    /// Capacity in bytes.
    pub bytes: u64,
    /// Whether the root filesystem is on it. The boot device is never bulk
    /// storage, however large it is (§2.3.1).
    pub is_boot: bool,
}

/// Whether this machine holds bulk storage, and is therefore the registrar
/// (§2.3.1).
///
/// **Two devices at or above the threshold is a refusal, not a choice.** Which
/// machine *should* hold the data is a decision about the hardware, taken by
/// whoever assembled it. A cluster that silently picked one would put the object
/// store wherever the enumeration order happened to land and report itself
/// healthy; the operator would find out when it filled (§21.11).
pub fn holds_bulk_disk(devices: &[Device], min_gb: u32) -> Result<bool, InitError> {
    let min_bytes = u64::from(min_gb) * 1_000_000_000;
    let bulk: Vec<&Device> = devices
        .iter()
        .filter(|d| !d.is_boot && d.bytes >= min_bytes)
        .collect();
    match bulk.len() {
        0 => Ok(false),
        1 => Ok(true),
        n => Err(InitError::Hardware(format!(
            "{n} non-boot devices are at least {min_gb} GB ({}). Which one should hold the \
             cluster's data is a decision about the hardware, and this refuses to make it \
             rather than picking by enumeration order (§2.3.1, §21.11)",
            bulk.iter()
                .map(|d| d.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// What the registrar has handed out so far (§2.3.2).
///
/// Keyed on machine ID and persisted, so an assignment is made once and survives
/// every subsequent boot in any order. Re-registering returns the existing
/// assignment rather than consuming a new one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// machine ID to assignment.
    #[serde(default)]
    pub assignments: BTreeMap<String, Assignment>,
}

/// One machine's place in the fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    /// Position in the fleet, `1..=fleet_size`.
    pub ordinal: u32,
    /// The role that goes with it.
    pub role: String,
}

impl Registry {
    /// Assign, or return what this machine already has.
    ///
    /// Idempotent on the machine ID. A registrar that handed out a fresh ordinal
    /// on every boot would exhaust the fleet in three reboots, and the machine
    /// that came back would find its own name pointing somewhere else.
    pub fn register(
        &mut self,
        machine_id: &str,
        order: &[String],
        first_free: u32,
        fleet_size: u32,
    ) -> Result<Assignment, InitError> {
        if machine_id.trim().is_empty() {
            return Err(InitError::Registry(
                "a registration with no machine ID: the identity is what makes an assignment \
                 idempotent, and an empty one would take a new ordinal on every boot (§2.3.2)"
                    .into(),
            ));
        }
        if let Some(existing) = self.assignments.get(machine_id) {
            return Ok(existing.clone());
        }

        // The **lowest free** assignable ordinal, not a count of the taken ones.
        //
        // Counting was wrong and a test caught it: release ordinal 2 while 3 is
        // still held, and the count is 1, so the replacement is handed ordinal 3
        // --- which the machine that never left is already using. §17.1 promises
        // a replacement the *same* ordinal, so its names, addresses and update
        // position are the ones the fleet already expects, and that is only true
        // if the search is for a gap rather than for a total.
        let held: std::collections::BTreeSet<u32> =
            self.assignments.values().map(|a| a.ordinal).collect();
        let ordinal = (first_free..=fleet_size)
            .find(|o| !held.contains(o))
            .ok_or_else(|| {
                InitError::Registry(format!(
                    "a machine asked to join a fleet of {fleet_size} in which every ordinal is \
                     held. Releasing one is explicit and never automatic: a node that is \
                     merely off is indistinguishable from one that is gone (§17.1)"
                ))
            })?;
        // The role follows the ordinal, not the order of arrival among those
        // still present. That is what makes a replacement a replacement.
        let role = order
            .get((ordinal - first_free) as usize)
            .ok_or_else(|| {
                InitError::Registry(format!(
                    "ordinal {ordinal} has no role in the hand-out order, which lists {} \
                     (§2.3.2)",
                    order.len()
                ))
            })?;
        let assignment = Assignment {
            ordinal,
            role: role.clone(),
        };
        self.assignments
            .insert(machine_id.to_string(), assignment.clone());
        Ok(assignment)
    }

    /// Free a machine's ordinal so a replacement can take it (§17.1).
    ///
    /// Explicit and never automatic. A node that is merely off --- powered down
    /// for maintenance, or midway through a reboot --- is indistinguishable from
    /// one that is gone, and a registrar that reclaimed on silence would hand a
    /// live node's identity to its replacement while the original was booting.
    pub fn release(&mut self, machine_id: &str) -> Option<Assignment> {
        self.assignments.remove(machine_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(path: &str, gb: u64, is_boot: bool) -> Device {
        Device {
            path: path.to_string(),
            bytes: gb * 1_000_000_000,
            is_boot,
        }
    }

    fn order() -> Vec<String> {
        vec!["compute".to_string(), "testbed".to_string()]
    }

    /// The fleet as assembled: one machine with a 2 TB disk, two without.
    #[test]
    fn the_machine_with_bulk_disk_is_the_storage_node() {
        let storage = [
            device("/dev/nvme0n1", 256, true),
            device("/dev/sda", 256, false),
            device("/dev/sdb", 2000, false),
        ];
        let other = [
            device("/dev/nvme0n1", 256, true),
            device("/dev/sda", 256, false),
        ];
        assert!(holds_bulk_disk(&storage, 1000).unwrap());
        assert!(!holds_bulk_disk(&other, 1000).unwrap());
    }

    /// A large *boot* device is not bulk storage. Without this a fleet built on
    /// 2 TB M.2 modules would have three registrars.
    #[test]
    fn the_boot_device_is_never_bulk_storage() {
        let devices = [device("/dev/nvme0n1", 2000, true)];
        assert!(!holds_bulk_disk(&devices, 1000).unwrap());
    }

    /// §21.11: it refuses rather than choosing.
    #[test]
    fn two_bulk_devices_is_a_refusal_and_not_a_choice() {
        let devices = [
            device("/dev/nvme0n1", 256, true),
            device("/dev/sdb", 2000, false),
            device("/dev/sdc", 4000, false),
        ];
        let err = holds_bulk_disk(&devices, 1000).expect_err("it refuses");
        let text = format!("{err}");
        assert!(text.contains("/dev/sdb") && text.contains("/dev/sdc"));
        assert!(
            text.contains("decision about the hardware"),
            "it says whose decision it is: {text}"
        );
    }

    /// Provisioning order is the only tie-break available between two identical
    /// machines (§2.3.2).
    #[test]
    fn roles_are_handed_out_in_the_order_machines_arrive() {
        let mut r = Registry::default();
        let first = r.register("aaa", &order(), 2, 3).unwrap();
        let second = r.register("bbb", &order(), 2, 3).unwrap();
        assert_eq!(first.role, "compute");
        assert_eq!(first.ordinal, 2);
        assert_eq!(second.role, "testbed");
        assert_eq!(second.ordinal, 3);
    }

    /// The property that makes a reboot safe. Without it three reboots exhaust
    /// the fleet and a returning machine finds its name pointing elsewhere.
    #[test]
    fn re_registering_returns_the_same_assignment() {
        let mut r = Registry::default();
        let first = r.register("aaa", &order(), 2, 3).unwrap();
        let again = r.register("aaa", &order(), 2, 3).unwrap();
        assert_eq!(first, again);
        assert_eq!(r.assignments.len(), 1, "no second ordinal was consumed");
    }

    /// Order of arrival, not order of machine ID: `zzz` first means `zzz` is
    /// compute.
    #[test]
    fn arrival_order_beats_identifier_order() {
        let mut r = Registry::default();
        assert_eq!(r.register("zzz", &order(), 2, 3).unwrap().role, "compute");
        assert_eq!(r.register("aaa", &order(), 2, 3).unwrap().role, "testbed");
    }

    #[test]
    fn a_fourth_machine_is_refused_and_told_why() {
        let mut r = Registry::default();
        r.register("aaa", &order(), 2, 3).unwrap();
        r.register("bbb", &order(), 2, 3).unwrap();
        let err = r.register("ccc", &order(), 2, 3).expect_err("the fleet is full");
        assert!(format!("{err}").contains("Releasing one is explicit"));
    }

    /// §17.1: a replacement takes the freed ordinal, so its names, addresses and
    /// update position are the ones the fleet already expects.
    #[test]
    fn a_replacement_takes_the_released_ordinal() {
        let mut r = Registry::default();
        r.register("aaa", &order(), 2, 3).unwrap();
        let second = r.register("bbb", &order(), 2, 3).unwrap();
        r.release("aaa").expect("it was assigned");
        let replacement = r.register("new-board", &order(), 2, 3).unwrap();
        assert_eq!(replacement.ordinal, 2, "the freed ordinal, not a fourth");
        assert_eq!(replacement.role, "compute");
        assert_eq!(
            r.assignments.get("bbb"),
            Some(&second),
            "the machine that stayed kept its place"
        );
    }

    #[test]
    fn an_empty_machine_id_is_refused() {
        let mut r = Registry::default();
        assert!(r.register("", &order(), 2, 3).is_err());
        assert!(r.register("   ", &order(), 2, 3).is_err());
    }
}
