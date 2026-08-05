//! What a machine works out about itself at boot (`SPEC.md` §2.3, §3.1, §3.3,
//! §4.1).
//!
//! One image is installed on all three machines (§8.4). An installed machine
//! differs from its neighbours only in the hardware it contains and the order in
//! which it was powered on, and this crate is what turns those two differences
//! into an ordinal, a role, a set of addresses and a name.
//!
//! # What is here, and what is deliberately not
//!
//! Every *decision* is here and every *number* is not. The thresholds that sort
//! a port into a class, the route metrics, the addressing bases, the discovery
//! parameters and the role table are read from `generated/node/init.conf`, which
//! is rendered from `model/` and diff-gated like every other artifact. A metric
//! compiled into this binary and also written in the model would be two sources
//! for one fact, and the one that drifted would be the one nobody read.
//!
//! # Why so much of it is pure
//!
//! The interesting failures are decisions, not syscalls: a classifier that sorts
//! a down port wrongly, a registrar that hands out an ordinal twice, an
//! addressing rule whose two ends disagree. Those are pure functions over what
//! was measured, and they are tested as such --- without a disk, a card or a
//! network. The I/O around them is thin on purpose.

#![deny(missing_docs)]

pub mod addressing;
pub mod boot;
pub mod config;
pub mod discovery;
pub mod links;
pub mod net;
pub mod role;
pub mod units;

/// A failure to work out what this machine is.
///
/// Every variant is sanctioned in `model/ids.toml` under R5. None of them is
/// recoverable in the sense of "carry on anyway": a node that could not classify
/// its ports, could not obtain an ordinal, or found two bulk disks has nothing
/// safe to do next, and §3.1 and §21.11 both say the boot fails instead.
#[derive(Debug)]
pub enum InitError {
    /// The rendered policy is missing a key or malformed.
    Config(String),
    /// The machine is not the shape a conforming one is (§2.1, §3.1, §2.3.1).
    Hardware(String),
    /// An ordinal could not be derived or is outside the fleet (§4.1).
    Addressing(String),
    /// The registrar refused, or could not be reached in time (§2.3.2).
    Registry(String),
    /// A peer could not be found on a mesh port within the timeout (§3.3).
    Discovery(String),
    /// A file could not be read or written.
    Io(String),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) => write!(f, "the rendered policy: {m}"),
            Self::Hardware(m) => write!(f, "this machine: {m}"),
            Self::Addressing(m) => write!(f, "addressing: {m}"),
            Self::Registry(m) => write!(f, "the registrar: {m}"),
            Self::Discovery(m) => write!(f, "peer discovery: {m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for InitError {}

impl From<std::io::Error> for InitError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Where the runtime-written network units go.
///
/// `systemd-networkd` searches this before `/usr/lib/systemd/network/`, so a
/// unit written here wins over anything shipped. Nothing persists: a role and an
/// ordinal are re-derived on every boot, and a file that survived a reboot could
/// outvote the machine it describes (§8.4).
pub const RUNTIME_NETWORK_DIR: &str = "/run/systemd/network";

/// Where the role marker, the node environment and the role's firewall include
/// go.
pub const RUNTIME_DIR: &str = "/run/cluster";

/// Where the image puts the rendered tree this binary reads.
pub const POLICY_DIR: &str = "/usr/lib/cluster";

/// The rendered policy this binary reads.
pub const POLICY_PATH: &str = "/usr/lib/cluster/init.conf";

/// Where the registrar persists what it has handed out (§2.3.2).
///
/// On the data volume, not under `/run`: an assignment is made once and must
/// survive every subsequent boot in any order. It is the one piece of state in
/// this crate that is deliberately *not* re-derived --- because re-deriving it
/// is exactly what would hand a live node's identity to its replacement.
pub const REGISTRY_PATH: &str = "/var/lib/cluster/registry.json";

/// Where the join secret lives, `0600` (§12.2).
///
/// Generated on the registrar's first boot from the kernel's random source and
/// handed to each node when it registers. It appears in no model file, no image,
/// no rendered artifact and no repository.
pub const SECRET_PATH: &str = "/var/lib/cluster/join.secret";

/// What `bootc loader-entries` was last told, so it is told again only on a
/// change (§8.5).
///
/// That call stages a new deployment. Making it unconditionally would stage one
/// on every boot of every machine --- including the two roles whose set is empty,
/// to remove a source that was never set.
pub const APPLIED_KARGS_PATH: &str = "/var/lib/cluster/role-kargs.applied";
