//! Learning which peer is on the far end of a cable (`SPEC.md` §3.3).
//!
//! A node knows it has two mesh ports. It does not know which peer is on either,
//! and it cannot be told: that is a property of how somebody ran the cables.
//!
//! Each port is brought up with IPv6 link-local addressing only --- always
//! available on an Ethernet link, requiring no allocation and no agreement ---
//! and the node announces itself to the link-local all-nodes multicast address
//! **on that port alone**. A direct-attached segment has exactly two endpoints,
//! so the only listener is the peer, and what comes back identifies the machine
//! at the other end of that specific cable.
//!
//! Discovery is not authentication, and §21.13 records that rather than leaving
//! it to be inferred. What follows discovery *is* authenticated: the
//! registration request carries the join secret, so learning a peer's identity
//! is not enough to obtain an ordinal, a role, or an address.

use serde::{Deserialize, Serialize};

use crate::InitError;

/// What a node says about itself on a mesh port.
///
/// The ordinal is optional because a node announces before it has one: the
/// machine that has not registered yet still has to find the registrar, and the
/// registrar is reached across exactly this mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// The stable identifier from `/etc/machine-id` (§2.3.2).
    pub machine_id: String,
    /// This node's ordinal, once the registrar has answered.
    #[serde(default)]
    pub ordinal: Option<u32>,
    /// This node's role, once it is known.
    #[serde(default)]
    pub role: Option<String>,
    /// Whether this node is the registrar, which it works out from its own
    /// disks before any of the above is known (§2.3.1).
    pub is_registrar: bool,
}

impl Announcement {
    /// Encode for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, InitError> {
        serde_json::to_vec(self)
            .map_err(|e| InitError::Discovery(format!("encoding an announcement: {e}")))
    }

    /// Decode a datagram from a peer.
    ///
    /// A malformed datagram is an error and never a default. The only thing on
    /// this segment should be the peer; something that is there and does not
    /// speak this protocol is worth reporting rather than ignoring.
    pub fn decode(bytes: &[u8]) -> Result<Self, InitError> {
        serde_json::from_slice(bytes)
            .map_err(|e| InitError::Discovery(format!("decoding an announcement: {e}")))
    }

    /// Whether this announcement came from us, echoed back.
    ///
    /// Multicast on a link the sender is also joined to comes back. Without this
    /// a node would discover itself as its own peer, derive a link from an
    /// ordinal to itself, and fail addressing with an error about a link to
    /// itself --- true, but three steps away from the cause.
    pub fn is_own(&self, machine_id: &str) -> bool {
        self.machine_id == machine_id
    }
}

/// What the registrar sends back to a machine that asked for a place (§2.3.2).
///
/// Addressed to a machine ID rather than to an address: the requester has no
/// address yet beyond a link-local one, and the whole point of the exchange is
/// to tell it which one to take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// Who this is for.
    pub machine_id: String,
    /// The ordinal the registrar assigned.
    pub ordinal: u32,
    /// The role that goes with it.
    pub role: String,
    /// The join secret, generated on the registrar's first boot and never in
    /// any model file, image or rendered artifact (§12.2).
    pub secret: String,
}

/// Everything that crosses a mesh cable before the mesh has addresses (§3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Message {
    /// "This is who I am."
    Announce(Announcement),
    /// "This is who you are."
    Grant(Grant),
}

impl Message {
    /// Encode for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, InitError> {
        serde_json::to_vec(self)
            .map_err(|e| InitError::Discovery(format!("encoding a message: {e}")))
    }

    /// Decode a datagram.
    pub fn decode(bytes: &[u8]) -> Result<Self, InitError> {
        serde_json::from_slice(bytes)
            .map_err(|e| InitError::Discovery(format!("decoding a message: {e}")))
    }
}

/// What a node has learned about one of its mesh ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The port's kernel name.
    pub interface: String,
    /// What the machine on the other end said.
    pub peer: Announcement,
}

/// The peer that is the registrar, if one of these ports leads to it.
///
/// A node that is not the registrar reaches it across exactly one of its two
/// cables, and which one is not knowable in advance. Two registrars on the mesh
/// is a misassembled fleet --- §2.3.1's predicate was true on two machines ---
/// and it is refused here as well as there, because the two checks catch it at
/// different moments and only one of them is on the machine that can see both.
pub fn registrar_among(found: &[Discovered]) -> Result<Option<&Discovered>, InitError> {
    let registrars: Vec<&Discovered> = found.iter().filter(|d| d.peer.is_registrar).collect();
    match registrars.len() {
        0 => Ok(None),
        1 => Ok(Some(registrars[0])),
        n => Err(InitError::Discovery(format!(
            "{n} of this machine's peers claim to be the registrar. §2.3.1's predicate is \
             true on exactly one machine of a conforming fleet, and a cluster that picked \
             one would put the object store wherever the race landed (§21.11)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement(id: &str, registrar: bool) -> Announcement {
        Announcement {
            machine_id: id.to_string(),
            ordinal: None,
            role: None,
            is_registrar: registrar,
        }
    }

    fn discovered(interface: &str, id: &str, registrar: bool) -> Discovered {
        Discovered {
            interface: interface.to_string(),
            peer: announcement(id, registrar),
        }
    }

    #[test]
    fn an_announcement_survives_the_wire() {
        let mut a = announcement("abc", true);
        a.ordinal = Some(1);
        a.role = Some("storage".into());
        let decoded = Announcement::decode(&a.encode().unwrap()).unwrap();
        assert_eq!(a, decoded);
    }

    /// A node announcing before it has registered has no ordinal to announce,
    /// and that has to round-trip: it is the state every machine is in on the
    /// boot that matters.
    #[test]
    fn a_node_with_no_ordinal_yet_still_announces() {
        let a = announcement("abc", false);
        let decoded = Announcement::decode(&a.encode().unwrap()).unwrap();
        assert_eq!(decoded.ordinal, None);
        assert_eq!(decoded.role, None);
    }

    /// Multicast on a joined link echoes. Without this a node discovers itself
    /// and then fails addressing with an error three steps from the cause.
    #[test]
    fn a_node_recognises_its_own_echo() {
        let a = announcement("mine", false);
        assert!(a.is_own("mine"));
        assert!(!a.is_own("theirs"));
    }

    #[test]
    fn the_registrar_is_found_on_whichever_cable_leads_to_it() {
        let found = [
            discovered("enp3s0f0", "peer-a", false),
            discovered("enp3s0f1", "peer-b", true),
        ];
        let registrar = registrar_among(&found).unwrap().expect("one of them is");
        assert_eq!(registrar.interface, "enp3s0f1");
    }

    /// A machine that is not the registrar and finds none waits rather than
    /// proceeding. §12.1 makes that survivable: it retries with backoff and
    /// joins when the registrar appears.
    #[test]
    fn no_registrar_among_the_peers_is_not_an_error() {
        let found = [discovered("enp3s0f0", "peer-a", false)];
        assert!(registrar_among(&found).unwrap().is_none());
    }

    /// The same refusal as §2.3.1's, caught on the one machine that can see both
    /// claimants at once.
    #[test]
    fn two_registrars_is_a_refusal() {
        let found = [
            discovered("enp3s0f0", "peer-a", true),
            discovered("enp3s0f1", "peer-b", true),
        ];
        let err = registrar_among(&found).expect_err("it refuses");
        assert!(format!("{err}").contains("exactly one machine"));
    }

    #[test]
    fn a_malformed_datagram_is_reported_and_not_ignored() {
        assert!(Announcement::decode(b"not json").is_err());
        assert!(Announcement::decode(b"{}").is_err(), "machine_id is required");
    }
}
