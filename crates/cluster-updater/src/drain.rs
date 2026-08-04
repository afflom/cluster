//! Draining, and the budgets that are never met by force (`SPEC.md` §14).
//!
//! Three strategies, and which one applies is a property of the workload rather
//! than of the node:
//!
//! - **Wait.** A bench job and a CI job both run to completion. `--ephemeral`
//!   runners exit after one job, so draining is a matter of not re-registering
//!   rather than of killing work. Migrating a measurement invalidates it.
//! - **Migrate.** Devcontainers move to `n1`, because a devcontainer's durable
//!   state is its worktree, its declared volumes, and the `devcontainer.json`
//!   that built it --- not its process state.
//! - **Cannot move.** The registry, object store, NFS and control plane are
//!   bound to a disk physically inside `n1`. §14.2 states the window rather than
//!   pretending it away.
//!
//! # A budget is never met by force
//!
//! Exceeding a budget halts the rollout and asks for a human, because the
//! alternative --- killing a four-hour benchmark to install a patch release ---
//! is worse than staying on the old image. The one exception is spelled out in
//! the model rather than here: devcontainer migration's `stop-with-notice`,
//! where the session survives and the process does not.

use std::fmt;

/// What a node is running that a rollout has to deal with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    /// Stable identifier: a session id, a runner name, a unit name.
    pub id: String,
    /// Which budget class it falls under.
    pub class: WorkloadClass,
    /// Declared memory, for the migration capacity cap (§14.3).
    pub memory_gib: u32,
    /// Seconds since this workload was last attached to, for ordering the
    /// migration queue. A devcontainer somebody is typing in right now is the
    /// one that should survive as a container rather than as an archive.
    pub idle_seconds: u64,
}

/// The classes §14.4 budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadClass {
    /// A measurement in flight. Never killed, never migrated.
    BenchJob,
    /// A CI job on an ephemeral runner.
    CiJob,
    /// A devcontainer, which is the only thing that moves.
    Devcontainer,
    /// Bound to `lv_data`, which is a disk physically inside `n1`.
    StorageService,
}

impl WorkloadClass {
    /// The budget class name, as `model/policy.toml` spells it.
    pub const fn budget_class(self) -> &'static str {
        match self {
            Self::BenchJob => "bench-job",
            Self::CiJob => "ci-job",
            Self::Devcontainer => "devcontainer-migration",
            // Nothing drains it, so nothing budgets it: the window is stated
            // (§14.2) rather than bounded.
            Self::StorageService => "total",
        }
    }
}

/// What to do with one workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// Let it finish; stop re-registering the runner that would take another.
    Wait,
    /// Move it to the migration target.
    Migrate {
        /// The node that receives it. Never one in `never_receives` (§2.3).
        to: String,
    },
    /// Stop it, with notice to its owner. The session survives; the process
    /// does not (§14.3).
    StopWithNotice {
        /// Why it is being stopped rather than moved, in the notice's words.
        because: String,
    },
    /// It goes down with the node, and the window is stated (§14.2).
    CannotMove {
        /// The stated unavailability window, rather than a solved problem.
        window: String,
    },
}

/// One workload and what will happen to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Which workload.
    pub workload: Workload,
    /// What happens to it.
    pub strategy: Strategy,
}

/// The plan for draining one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every workload, in the order it will be handled.
    pub steps: Vec<Step>,
}

impl Plan {
    /// The workloads that will move.
    pub fn migrating(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|s| matches!(s.strategy, Strategy::Migrate { .. }))
            .collect()
    }

    /// The workloads that will be stopped with notice.
    pub fn stopping(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|s| matches!(s.strategy, Strategy::StopWithNotice { .. }))
            .collect()
    }

    /// Total declared memory of everything that moves. Never above the cap.
    pub fn migrating_memory_gib(&self) -> u32 {
        self.migrating().iter().map(|s| s.workload.memory_gib).sum()
    }
}

/// Where a drain may send work, and how much of it (§14.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capacity {
    /// The only node that can receive devcontainers.
    pub target: String,
    /// Nodes that receive no migrated workload under any circumstance.
    pub never_receives: Vec<String>,
    /// `n1` has 4 cores and 32 GB and is already running the storage services
    /// and two CI runners. Beyond this, the excess is stopped with notice.
    pub memory_cap_gib: u32,
}

/// Plan a drain.
///
/// Pure: it decides, it does not act. The acting is in `main.rs`, and keeping
/// the decision separable is what lets T2 assert the cap is enforced rather than
/// exceeded silently (§14.3).
pub fn plan(workloads: &[Workload], capacity: &Capacity) -> Plan {
    // Nothing migrates to a node that never receives work. Checked here rather
    // than trusted to the caller, because receiving work would void the
    // isolation guarantee `n3` exists to provide, and a guarantee that depends
    // on nobody making a mistake is not one (§2.3, §14.1).
    let target_forbidden = capacity.never_receives.contains(&capacity.target);

    // Most-recently-attached first. Somebody is typing in the container that was
    // attached a minute ago; the one idle for a week is the one that should
    // become an archive if the cap bites. The tiebreak on id keeps the plan
    // deterministic, so two runs of the same drain choose the same victims.
    let mut devcontainers: Vec<&Workload> = workloads
        .iter()
        .filter(|w| w.class == WorkloadClass::Devcontainer)
        .collect();
    devcontainers.sort_by(|a, b| {
        a.idle_seconds
            .cmp(&b.idle_seconds)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut budget_left = capacity.memory_cap_gib;
    let mut migrating: Vec<&str> = Vec::new();
    for container in &devcontainers {
        if target_forbidden {
            continue;
        }
        if container.memory_gib <= budget_left {
            budget_left -= container.memory_gib;
            migrating.push(&container.id);
        }
    }

    let mut steps = Vec::new();
    for workload in workloads {
        let strategy = match workload.class {
            // Never kill a measurement, and never move one: migrating it
            // invalidates it (§14.1).
            WorkloadClass::BenchJob | WorkloadClass::CiJob => Strategy::Wait,
            WorkloadClass::StorageService => Strategy::CannotMove {
                window: "bound to lv_data, a disk physically inside the storage node (§14.2)"
                    .to_string(),
            },
            WorkloadClass::Devcontainer => {
                if target_forbidden {
                    Strategy::StopWithNotice {
                        because: format!(
                            "{} receives no migrated workload (§2.3)",
                            capacity.target
                        ),
                    }
                } else if migrating.contains(&workload.id.as_str()) {
                    Strategy::Migrate {
                        to: capacity.target.clone(),
                    }
                } else {
                    Strategy::StopWithNotice {
                        because: format!(
                            "the {} GiB migration cap is spent; the session survives, the \
                             process does not (§14.3)",
                            capacity.memory_cap_gib
                        ),
                    }
                }
            }
        };
        steps.push(Step {
            workload: workload.clone(),
            strategy,
        });
    }

    Plan { steps }
}

/// One budget, as `model/policy.toml` declares it (§14.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// The class it applies to.
    pub class: String,
    /// The per-item budget.
    pub seconds: u64,
    /// `halt` or `stop-with-notice`.
    pub on_exceed: OnExceed,
}

/// What happens when a budget is exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExceed {
    /// Halt the rollout and alert. Never kill the work.
    Halt,
    /// Stop the remainder with notice and continue.
    StopWithNotice,
}

impl OnExceed {
    /// Parse the token `model/policy.toml` uses.
    ///
    /// An unrecognised token is `Halt`, not a default of convenience: the model
    /// check rejects one, and if it somehow reached a node, halting is the
    /// outcome that asks a human rather than the one that acts.
    pub fn parse(token: &str) -> Self {
        match token {
            "stop-with-notice" => Self::StopWithNotice,
            _ => Self::Halt,
        }
    }
}

/// The outcome of measuring a drain against its budgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetOutcome {
    /// Inside every budget.
    WithinBudget,
    /// A budget was exceeded and the rollout halts (§13.5).
    Halt {
        /// Which class.
        class: String,
        /// How long it took.
        elapsed_s: u64,
        /// What it was allowed.
        budget_s: u64,
    },
    /// A budget was exceeded and the remainder is stopped with notice.
    StopRemainder {
        /// Which class.
        class: String,
        /// How long it took.
        elapsed_s: u64,
        /// What it was allowed.
        budget_s: u64,
    },
}

impl fmt::Display for BudgetOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WithinBudget => write!(f, "within budget"),
            Self::Halt {
                class,
                elapsed_s,
                budget_s,
            } => write!(
                f,
                "{class} took {elapsed_s}s against a {budget_s}s budget. The rollout halts \
                 and asks for a human: killing the work to meet the budget is worse than \
                 staying on the old image (§14.4)"
            ),
            Self::StopRemainder {
                class,
                elapsed_s,
                budget_s,
            } => write!(
                f,
                "{class} took {elapsed_s}s against a {budget_s}s budget; the remainder is \
                 stopped with notice (§14.4)"
            ),
        }
    }
}

/// Measure elapsed time against a class's budget.
pub fn against_budget(budget: &Budget, elapsed_s: u64) -> BudgetOutcome {
    if elapsed_s <= budget.seconds {
        return BudgetOutcome::WithinBudget;
    }
    match budget.on_exceed {
        OnExceed::Halt => BudgetOutcome::Halt {
            class: budget.class.clone(),
            elapsed_s,
            budget_s: budget.seconds,
        },
        OnExceed::StopWithNotice => BudgetOutcome::StopRemainder {
            class: budget.class.clone(),
            elapsed_s,
            budget_s: budget.seconds,
        },
    }
}

impl Strategy {
    /// The notice text, for a strategy that carries one.
    pub fn to_notice(self) -> String {
        match self {
            Self::StopWithNotice { because } => because,
            Self::CannotMove { window } => window,
            Self::Wait => "waiting for the workload to finish".to_string(),
            Self::Migrate { to } => format!("migrating to {to}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devcontainer(id: &str, memory_gib: u32, idle_seconds: u64) -> Workload {
        Workload {
            id: id.to_string(),
            class: WorkloadClass::Devcontainer,
            memory_gib,
            idle_seconds,
        }
    }

    fn capacity() -> Capacity {
        Capacity {
            target: "n1".to_string(),
            never_receives: vec!["n3".to_string()],
            memory_cap_gib: 12,
        }
    }

    /// `CU-04`: exceeding a budget halts, and never kills the work.
    #[test]
    fn an_exceeded_budget_halts_rather_than_killing_work_cu_04() {
        let bench = Budget {
            class: "bench-job".to_string(),
            seconds: 14_400,
            on_exceed: OnExceed::Halt,
        };
        assert_eq!(against_budget(&bench, 14_400), BudgetOutcome::WithinBudget);

        let outcome = against_budget(&bench, 14_401);
        assert!(matches!(outcome, BudgetOutcome::Halt { .. }));
        assert!(outcome.to_string().contains("worse than"));

        // And a four-hour benchmark that overruns is still not killed: the plan
        // for a bench job is Wait regardless of how long it has taken.
        let plan = plan(
            &[Workload {
                id: "bench-1".to_string(),
                class: WorkloadClass::BenchJob,
                memory_gib: 8,
                idle_seconds: 0,
            }],
            &capacity(),
        );
        assert_eq!(plan.steps[0].strategy, Strategy::Wait);
    }

    /// `CU-05`: migration respects the cap, and the excess is stopped with
    /// notice rather than migrated.
    #[test]
    fn the_migration_cap_is_enforced_not_exceeded_cu_05() {
        let workloads = vec![
            devcontainer("a", 8, 10),
            devcontainer("b", 6, 20),
            devcontainer("c", 4, 30),
        ];
        let plan = plan(&workloads, &capacity());

        assert!(
            plan.migrating_memory_gib() <= 12,
            "the cap must be enforced, not exceeded silently (§14.3): {} GiB",
            plan.migrating_memory_gib()
        );

        // Most-recently-attached first: `a` (idle 10s) then `c` (4 GiB) fits the
        // remaining budget where `b` (6 GiB) does not.
        let moving: Vec<&str> = plan
            .migrating()
            .iter()
            .map(|s| s.workload.id.as_str())
            .collect();
        assert_eq!(moving, vec!["a", "c"]);

        // The excess survives as a session; only its process does not.
        let stopped: Vec<&str> = plan
            .stopping()
            .iter()
            .map(|s| s.workload.id.as_str())
            .collect();
        assert_eq!(stopped, vec!["b"]);
        assert!(plan.stopping()[0]
            .strategy
            .clone()
            .to_notice()
            .contains("the session survives"));
    }

    /// `CU-06`: nothing is ever migrated to a node that never receives work.
    #[test]
    fn nothing_migrates_to_a_reserved_node_cu_06() {
        let forbidden = Capacity {
            target: "n3".to_string(),
            never_receives: vec!["n3".to_string()],
            memory_cap_gib: 12,
        };
        let plan = plan(&[devcontainer("a", 1, 0)], &forbidden);

        assert!(
            plan.migrating().is_empty(),
            "receiving work voids the isolation guarantee n3 exists to provide (§2.3)"
        );
        assert_eq!(plan.stopping().len(), 1);
    }

    /// A storage service goes down with its node, and the plan says so rather
    /// than omitting it.
    #[test]
    fn a_storage_service_cannot_move_cu_04() {
        let plan = plan(
            &[Workload {
                id: "zot".to_string(),
                class: WorkloadClass::StorageService,
                memory_gib: 2,
                idle_seconds: 0,
            }],
            &capacity(),
        );
        assert!(matches!(
            plan.steps[0].strategy,
            Strategy::CannotMove { .. }
        ));
    }

    /// The plan is deterministic: two runs choose the same containers, so a
    /// retried drain does not stop a different set than the one it started.
    #[test]
    fn the_plan_is_deterministic_cu_05() {
        let workloads = vec![
            devcontainer("z", 7, 100),
            devcontainer("y", 7, 100),
            devcontainer("x", 7, 100),
        ];
        let first = plan(&workloads, &capacity());
        let second = plan(&workloads, &capacity());
        assert_eq!(first, second);
        assert_eq!(first.migrating().len(), 1, "only one 7 GiB fits under 12");
    }
}
