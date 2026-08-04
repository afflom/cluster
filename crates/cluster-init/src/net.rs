//! The sockets peer discovery and registration run over (`SPEC.md` §3.3, §2.3.2).
//!
//! IPv6 link-local multicast, scoped to one interface. Always available on an
//! Ethernet link, requiring no allocation and no agreement --- which matters,
//! because this is what runs *before* the mesh has addresses, and it is how a
//! machine finds out which addresses to take.
//!
//! A direct-attached segment has exactly two endpoints, so a datagram sent with
//! an interface scope reaches the peer and nothing else. That is a property of
//! §4.1's topology, and it is the same property §4.4 relies on when it trusts
//! the mesh in full.
//!
//! Everything that *decides* anything is in [`crate::discovery`] and
//! [`crate::role`], tested without a socket. What is here is the sending and the
//! receiving.

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::time::{Duration, Instant};

use crate::discovery::{Announcement, Discovered, Grant, Message};
use crate::InitError;

/// How the sockets are addressed, read from the rendered policy.
#[derive(Debug, Clone)]
pub struct Wire {
    /// The link-local all-nodes group.
    pub group: Ipv6Addr,
    /// The UDP port.
    pub port: u16,
    /// How often an unpeered link re-announces.
    pub interval: Duration,
    /// How long to wait before reporting a link unpeered. Longer than a cold
    /// boot, because the peer may not have been powered on yet (§12.1).
    pub timeout: Duration,
}

/// The kernel's index for an interface, which is what scopes a link-local
/// address to one cable.
pub fn interface_index(name: &str) -> Result<u32, InitError> {
    let path = format!("/sys/class/net/{name}/ifindex");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| InitError::Io(format!("reading {path}: {e}")))?;
    text.trim()
        .parse()
        .map_err(|_| InitError::Io(format!("{path} does not hold an index")))
}

/// A socket bound to the discovery port and joined to the group on one
/// interface.
pub fn bind(interface: &str, wire: &Wire) -> Result<UdpSocket, InitError> {
    let index = interface_index(interface)?;
    // Bound to the unspecified address rather than to a link-local one: the
    // link-local address may not be assigned yet at the moment this runs, and
    // the scope on every send and the membership on this join are what confine
    // the traffic to the cable.
    let socket = UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, wire.port, 0, 0))
        .map_err(|e| InitError::Io(format!("binding the discovery port on {interface}: {e}")))?;
    socket
        .join_multicast_v6(&wire.group, index)
        .map_err(|e| InitError::Io(format!("joining {} on {interface}: {e}", wire.group)))?;
    socket
        .set_read_timeout(Some(wire.interval))
        .map_err(|e| InitError::Io(format!("setting a read timeout: {e}")))?;
    Ok(socket)
}

/// Send one message to the group, scoped to this interface.
pub fn send(socket: &UdpSocket, interface: &str, wire: &Wire, message: &Message) -> Result<(), InitError> {
    let index = interface_index(interface)?;
    let to = SocketAddrV6::new(wire.group, wire.port, 0, index);
    socket
        .send_to(&message.encode()?, to)
        .map_err(|e| InitError::Io(format!("announcing on {interface}: {e}")))?;
    Ok(())
}

/// Announce on one port until the peer answers, or the timeout expires.
///
/// Re-announcing rather than announcing once: the peer may not have booted yet,
/// and §12.1 promises that a machine powered on before the others joins when
/// they appear rather than failing.
pub fn discover_peer(
    interface: &str,
    own: &Announcement,
    wire: &Wire,
) -> Result<Discovered, InitError> {
    let socket = bind(interface, wire)?;
    let deadline = Instant::now() + wire.timeout;
    let mut buffer = [0u8; 4096];

    while Instant::now() < deadline {
        send(&socket, interface, wire, &Message::Announce(own.clone()))?;
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
                let Ok(Message::Announce(peer)) = Message::decode(&buffer[..len]) else {
                    // A grant, or something that does not parse. Neither is an
                    // answer to this question; the loop keeps asking.
                    continue;
                };
                // Multicast on a joined link echoes. Without this a node
                // discovers itself, derives a link from an ordinal to itself,
                // and fails three steps from the cause.
                if peer.is_own(&own.machine_id) {
                    continue;
                }
                return Ok(Discovered {
                    interface: interface.to_string(),
                    peer,
                });
            }
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(InitError::Io(format!("receiving on {interface}: {e}"))),
        }
    }

    Err(InitError::Discovery(format!(
        "no peer answered on {interface} within {}s. A mesh port with nothing on the far \
         end has no address to take: which addresses a cable carries follows from which \
         machine is on it (§3.3, §4.1)",
        wire.timeout.as_secs()
    )))
}

/// Ask the registrar for a place, and wait for the grant (§2.3.2).
///
/// Sent on the interface the registrar was discovered on, which is the one cable
/// that reaches it. A grant for a different machine is ignored rather than
/// consumed: on a triangle the registrar answers two machines over two cables,
/// and the answers are addressed by machine ID for exactly that reason.
pub fn request_place(
    interface: &str,
    own: &Announcement,
    wire: &Wire,
) -> Result<Grant, InitError> {
    let socket = bind(interface, wire)?;
    let deadline = Instant::now() + wire.timeout;
    let mut buffer = [0u8; 4096];

    while Instant::now() < deadline {
        send(&socket, interface, wire, &Message::Announce(own.clone()))?;
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
                let Ok(Message::Grant(grant)) = Message::decode(&buffer[..len]) else {
                    continue;
                };
                if grant.machine_id != own.machine_id {
                    continue;
                }
                return Ok(grant);
            }
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(InitError::Io(format!("receiving on {interface}: {e}"))),
        }
    }

    Err(InitError::Registry(format!(
        "the registrar did not answer on {interface} within {}s (§2.3.2)",
        wire.timeout.as_secs()
    )))
}

/// Serve grants on one interface until the deadline, assigning as machines ask.
///
/// Run by the registrar on each of its mesh ports. `assign` is
/// [`crate::role::Registry::register`] behind a closure, so the persistence and
/// the socket stay apart and the decision is testable without either.
pub fn serve_grants<F>(
    interface: &str,
    own: &Announcement,
    wire: &Wire,
    secret: &str,
    until: Instant,
    mut assign: F,
) -> Result<Vec<Grant>, InitError>
where
    F: FnMut(&str) -> Result<(u32, String), InitError>,
{
    let socket = bind(interface, wire)?;
    let mut buffer = [0u8; 4096];
    let mut granted = Vec::new();

    while Instant::now() < until {
        // The registrar announces too. A machine that has not registered is
        // listening for exactly this to learn which cable reaches it.
        send(&socket, interface, wire, &Message::Announce(own.clone()))?;
        match socket.recv_from(&mut buffer) {
            Ok((len, from)) => {
                let Ok(Message::Announce(peer)) = Message::decode(&buffer[..len]) else {
                    continue;
                };
                if peer.is_own(&own.machine_id) || peer.ordinal.is_some() {
                    continue;
                }
                let (ordinal, role) = assign(&peer.machine_id)?;
                let grant = Grant {
                    machine_id: peer.machine_id.clone(),
                    ordinal,
                    role,
                    secret: secret.to_string(),
                };
                // Unicast back to where it came from. The grant carries the
                // join secret, and a secret on a multicast group is a secret
                // handed to everything on the segment --- which is only the peer
                // today, and is the sort of assumption that stops being true
                // quietly.
                reply(&socket, from, &Message::Grant(grant.clone()))?;
                granted.push(grant);
            }
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(InitError::Io(format!("receiving on {interface}: {e}"))),
        }
    }
    Ok(granted)
}

fn reply(socket: &UdpSocket, to: SocketAddr, message: &Message) -> Result<(), InitError> {
    socket
        .send_to(&message.encode()?, to)
        .map_err(|e| InitError::Io(format!("replying to {to}: {e}")))?;
    Ok(())
}

/// A read that expired rather than failed. Both kinds appear on a socket with a
/// read timeout, and only one of them means "nothing yet".
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// A join secret, from the kernel's random source (§12.2).
///
/// Generated on the registrar's first boot and stored `0600` on the data volume.
/// It appears in no model file, no image, no rendered artifact and no
/// repository: a shared secret committed to a repository is a shared secret with
/// everyone who can read it, and this one is public (§9.1).
pub fn generate_secret(bytes: usize) -> Result<String, InitError> {
    let raw = std::fs::read("/dev/urandom")
        .map(|mut v| {
            v.truncate(bytes);
            v
        })
        .map_err(|e| InitError::Io(format!("reading the kernel's random source: {e}")))?;
    if raw.len() < bytes {
        return Err(InitError::Io(format!(
            "the kernel's random source returned {} bytes of {bytes}",
            raw.len()
        )));
    }
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timeout is "nothing yet"; anything else is a failure worth reporting.
    #[test]
    fn a_timeout_is_distinguished_from_a_failure() {
        assert!(is_timeout(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(is_timeout(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )));
        assert!(!is_timeout(&std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        )));
    }

    /// Long enough that guessing is not a strategy, and hex so it survives an
    /// environment file without quoting.
    #[test]
    fn a_generated_secret_is_hex_and_full_length() {
        let secret = generate_secret(32).expect("the kernel has randomness");
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Two calls differ. A constant would pass every other test here and be
    /// worthless.
    #[test]
    fn two_secrets_differ() {
        assert_ne!(
            generate_secret(32).unwrap(),
            generate_secret(32).unwrap(),
            "a secret that repeats is not one"
        );
    }
}
