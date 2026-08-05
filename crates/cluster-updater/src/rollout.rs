//! Ordering without a lock (`SPEC.md` §13.2).
//!
//! A distributed lock would need a service that survives the reboot of the node
//! holding it --- which means either consensus across three nodes or a single
//! point of failure that is itself one of the three. Both are more control plane
//! than this cluster has.
//!
//! So ordering is a **pure function of observable state**. Each node knows its
//! position from the rendered environment and reads every peer's
//! `:9101/health`. [`admits`] is that function, and it is pure in the strong
//! sense: no clock, no network, no filesystem. That is what makes the property
//! this whole design rests on --- *at most one node is admitted in any
//! consistent state* --- checkable by enumeration rather than by argument.
//!
//! # Three outcomes, not two
//!
//! [`Decision::Wait`] and [`Decision::Halt`] are different and must stay so.
//! Waiting is the normal middle of a rollout: a predecessor is healthy and
//! simply has not reached the target yet. Halting is §13.5 --- an unhealthy
//! peer, a quarantined digest, a breached budget --- and it is a state that
//! alerts and asks for a human.
//!
//! Collapsing them would make the common case indistinguishable from the one
//! that needs attention, and §18's rollout-stalled alert would fire on every
//! normal rollout or on none.

use std::fmt;

use cluster_health::State;

/// What one peer reports, as this node sees it.
///
/// Every observable is an `Option` because a peer that cannot be read is
/// **unknown**, and unknown is neither healthy nor unhealthy. §13.2 halts on
/// unknowns: proceeding would mean acting on an assumption about a machine that
/// is not answering, which is exactly when acting is least safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerReport {
    /// The peer's name.
    pub name: String,
    /// Its position in the rollout sequence, from the model.
    pub position: u32,
    /// Whether it reports itself healthy, or `None` if it could not be read.
    pub healthy: Option<bool>,
    /// The digest it reports having booted.
    pub booted: Option<String>,
    /// Its rollout state.
    pub state: Option<State>,
}

impl PeerReport {
    /// A peer that did not answer within the timeout.
    pub fn unknown(name: impl Into<String>, position: u32) -> Self {
        Self {
            name: name.into(),
            position,
            healthy: None,
            booted: None,
            state: None,
        }
    }
}

/// Everything the predicate is evaluated against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// This node's name.
    pub node: String,
    /// This node's position in the rollout sequence.
    pub position: u32,
    /// The digest this node is running.
    pub booted: String,
    /// The digest `:stable` resolves to.
    pub target: String,
    /// Digests a node rolled back from (§13.4).
    pub quarantined: Vec<String>,
    /// Every peer, in any order.
    pub peers: Vec<PeerReport>,
}

/// What the predicate decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do: this node is already on the target.
    UpToDate,
    /// Admitted. Drain, then apply.
    Apply,
    /// A legitimate not-yet. The rollout is progressing and this node is not
    /// next; the next poll will ask again.
    Wait(String),
    /// A §13.5 halt condition. Recoverable, alerted at six hours, and never
    /// silently reconciled.
    Halt(String),
}

impl Decision {
    /// Whether this decision admits the node.
    pub fn applies(&self) -> bool {
        matches!(self, Self::Apply)
    }

    /// Whether this decision is a halt (§13.5).
    pub fn halts(&self) -> bool {
        matches!(self, Self::Halt(_))
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UpToDate => write!(f, "up to date"),
            Self::Apply => write!(f, "admitted"),
            Self::Wait(why) => write!(f, "waiting: {why}"),
            Self::Halt(why) => write!(f, "halted: {why}"),
        }
    }
}

/// §13.2's ordering predicate.
///
/// A node at position `i` applies an update only when all of:
///
/// - `target != booted` --- a new digest exists
/// - `target` is not quarantined (§13.4)
/// - for every `j < i`: peer `j` reports `booted == target` **and** healthy
/// - for every `j > i`: peer `j` reports healthy
/// - no peer reports `draining` or `updating`
///
/// By construction this is true for exactly one node in any consistent state,
/// and [`crate::tests`] checks that by enumeration rather than asserting it.
pub fn admits(o: &Observation) -> Decision {
    // A new digest must exist. Checked first because it is the only condition
    // whose absence is not a rollout state at all --- there is no rollout.
    if o.target == o.booted {
        return Decision::UpToDate;
    }

    // §13.4: a node that rolled back POSTs the failed digest as quarantined, and
    // quarantine is a precondition here so no other node attempts it. Because the testbed
    // moves first, a bad image is normally caught by the node whose failure
    // costs least.
    if o.quarantined.iter().any(|d| d == &o.target) {
        return Decision::Halt(format!(
            "target {} is quarantined: a node rolled back from it (§13.4)",
            short(&o.target)
        ));
    }

    // Peers are evaluated in rollout order so that the reason reported is the
    // earliest one, which is the one an operator needs first.
    let mut peers: Vec<&PeerReport> = o.peers.iter().collect();
    peers.sort_by_key(|p| p.position);

    for peer in &peers {
        // An unknown halts. It is not "assume healthy" and it is not "assume
        // unhealthy": it is the absence of an answer, and §13.2's guarantee of
        // at-most-one holds only over states that were actually observed.
        let Some(healthy) = peer.healthy else {
            return Decision::Halt(format!(
                "{} could not be read. Its health is unknown, and proceeding would mean \
                 acting on an assumption about a machine that is not answering (§13.2)",
                peer.name
            ));
        };

        // §13.5: any peer unhealthy at the start of a stage halts. A cluster
        // updated on top of an unnoticed fault is worse than a halted rollout.
        if !healthy {
            return Decision::Halt(format!(
                "{} is unhealthy. A cluster updated on top of an unnoticed fault is not a \
                 recoverable state; a halted rollout is (§13.5)",
                peer.name
            ));
        }

        // No peer mid-flight. This is the condition that does the work a lock
        // would: two nodes cannot both be admitted while either is draining or
        // updating.
        match peer.state {
            Some(State::Draining) | Some(State::Updating) => {
                return Decision::Wait(format!(
                    "{} is {}",
                    peer.name,
                    peer.state.unwrap_or(State::Idle).as_str()
                ));
            }
            None => {
                return Decision::Halt(format!("{} reports no rollout state (§13.2)", peer.name));
            }
            Some(State::Idle) => {}
        }

        // Predecessors must already be on the target. This is the ordering
        // itself: the testbed first, because a failure there costs a measurement window
        // rather than the pipeline; the storage node last, because it carries the machinery
        // needed to diagnose a bad update (§2.3).
        if peer.position < o.position {
            let on_target = peer.booted.as_deref() == Some(o.target.as_str());
            if !on_target {
                return Decision::Wait(format!(
                    "{} is at position {} and is not yet on {}",
                    peer.name,
                    peer.position,
                    short(&o.target)
                ));
            }
        }
    }

    Decision::Apply
}

/// The first twelve characters after `sha256:`.
fn short(digest: &str) -> String {
    let bare = digest.strip_prefix("sha256:").unwrap_or(digest);
    bare.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "sha256:0000";
    const NEW: &str = "sha256:1111";

    fn peer(name: &str, position: u32, booted: &str) -> PeerReport {
        PeerReport {
            name: name.to_string(),
            position,
            healthy: Some(true),
            booted: Some(booted.to_string()),
            state: Some(State::Idle),
        }
    }

    fn observation(node: &str, position: u32, booted: &str, peers: Vec<PeerReport>) -> Observation {
        Observation {
            node: node.to_string(),
            position,
            booted: booted.to_string(),
            target: NEW.to_string(),
            quarantined: Vec::new(),
            peers,
        }
    }

    /// The three nodes, with the positions the model declares (§2.3).
    const FLEET: [(&str, u32); 3] = [("node3", 1), ("node2", 2), ("node1", 3)];

    /// Build the whole cluster's view from an assignment of digests, and ask
    /// each node what it would do.
    fn decisions(booted: [&str; 3]) -> Vec<(&'static str, Decision)> {
        FLEET
            .iter()
            .enumerate()
            .map(|(i, (name, position))| {
                let peers = FLEET
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(j, (pname, pposition))| peer(pname, *pposition, booted[j]))
                    .collect();
                (
                    *name,
                    admits(&observation(name, *position, booted[i], peers)),
                )
            })
            .collect()
    }

    /// `CU-01`: at most one node is admitted in any consistent state.
    ///
    /// Checked by enumerating every assignment of old/new across three nodes
    /// rather than by argument. §13.2 says "by construction this is true for
    /// exactly one node in any consistent state", and a claim of that shape is
    /// exactly the kind that is true of the design and false of the code.
    #[test]
    fn at_most_one_node_is_admitted_cu_01() {
        let mut admitted_somewhere = 0usize;
        for a in [OLD, NEW] {
            for b in [OLD, NEW] {
                for c in [OLD, NEW] {
                    let booted = [a, b, c];
                    let decisions = decisions(booted);
                    let admitted: Vec<&str> = decisions
                        .iter()
                        .filter(|(_, d)| d.applies())
                        .map(|(n, _)| *n)
                        .collect();
                    assert!(
                        admitted.len() <= 1,
                        "state {booted:?} admits {admitted:?}; two nodes rebooting at once \
                         is what the predicate exists to prevent (§13.2)"
                    );
                    if admitted.len() == 1 {
                        admitted_somewhere += 1;
                    }
                }
            }
        }
        // And the predicate is not vacuously safe by admitting nobody, ever.
        assert!(
            admitted_somewhere > 0,
            "no state admits any node, which would be a predicate that never updates"
        );
    }

    /// The sequence actually runs to completion, one node at a time, in the
    /// declared order. A predicate that admits at most one node but stalls
    /// forever would pass the test above.
    #[test]
    fn the_rollout_runs_in_declared_order_cu_01() {
        let mut booted = [OLD, OLD, OLD];
        let mut order = Vec::new();

        for _ in 0..FLEET.len() {
            let decisions = decisions(booted);
            let (name, _) = decisions
                .iter()
                .find(|(_, d)| d.applies())
                .unwrap_or_else(|| panic!("no node admitted from {booted:?}"));
            order.push(*name);
            let at = FLEET.iter().position(|(n, _)| n == name).expect("in fleet");
            booted[at] = NEW;
        }

        assert_eq!(
            order,
            vec!["node3", "node2", "node1"],
            "§2.3's update positions"
        );
        assert!(
            decisions(booted)
                .iter()
                .all(|(_, d)| *d == Decision::UpToDate),
            "the rollout must terminate"
        );
    }

    /// `CU-02`: a quarantined target is never applied.
    #[test]
    fn a_quarantined_target_is_never_applied_cu_02() {
        let mut o = observation(
            "node3",
            1,
            OLD,
            vec![peer("node2", 2, OLD), peer("node1", 3, OLD)],
        );
        assert!(admits(&o).applies(), "the control must be admitted");

        o.quarantined = vec![NEW.to_string()];
        let decision = admits(&o);
        assert!(decision.halts(), "{decision}");
        assert!(decision.to_string().contains("quarantined"));
    }

    /// `CU-03`: an unknown peer halts rather than proceeding.
    #[test]
    fn an_unknown_peer_halts_the_rollout_cu_03() {
        let mut o = observation(
            "node3",
            1,
            OLD,
            vec![peer("node2", 2, OLD), peer("node1", 3, OLD)],
        );
        assert!(admits(&o).applies());

        // Unreadable is not unhealthy, and neither is it healthy.
        o.peers[0] = PeerReport::unknown("node2", 2);
        let decision = admits(&o);
        assert!(decision.halts(), "{decision}");
        assert!(decision.to_string().contains("unknown"));

        // An explicitly unhealthy peer also halts, by a different clause and
        // with a different reason (§13.5).
        o.peers[0] = PeerReport {
            healthy: Some(false),
            ..peer("node2", 2, OLD)
        };
        let decision = admits(&o);
        assert!(decision.halts(), "{decision}");
        assert!(decision.to_string().contains("unhealthy"));
    }

    /// A peer mid-flight makes this node wait, not halt. The distinction is
    /// what keeps §18's rollout-stalled alert meaningful.
    #[test]
    fn a_peer_mid_flight_is_a_wait_not_a_halt_cu_01() {
        let mut o = observation(
            "node2",
            2,
            OLD,
            vec![peer("node3", 1, NEW), peer("node1", 3, OLD)],
        );
        assert!(admits(&o).applies());

        o.peers[0].state = Some(State::Updating);
        let decision = admits(&o);
        assert!(
            !decision.halts(),
            "a normal rollout must not alert: {decision}"
        );
        assert!(matches!(decision, Decision::Wait(_)));
    }

    /// A successor's digest is not a precondition: only predecessors must be on
    /// the target, and a later node still on the old image is the normal state
    /// of every rollout before its turn.
    #[test]
    fn a_successor_need_not_be_on_the_target_cu_01() {
        let o = observation(
            "node3",
            1,
            OLD,
            vec![peer("node2", 2, OLD), peer("node1", 3, OLD)],
        );
        assert!(admits(&o).applies());
    }

    #[test]
    fn a_node_already_on_the_target_does_nothing_cu_01() {
        let o = observation(
            "node3",
            1,
            NEW,
            vec![peer("node2", 2, OLD), peer("node1", 3, OLD)],
        );
        assert_eq!(admits(&o), Decision::UpToDate);
    }
}
