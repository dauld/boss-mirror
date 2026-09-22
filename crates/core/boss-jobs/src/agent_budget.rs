//! The claim door's budget reservation — design c87fb59b car 3
//! (backlog cb78818d).
//!
//! Car 1 put a `budget_usd` on the step's agent block and projected it
//! onto the packet as `agent_budget_usd`. `agent_runs` judges a run
//! against its actor's `hourly_budget_usd_micros` AT FINISH — a
//! refusal there is a fact the desk can act on, not a gate that stopped
//! the spend (that module's own "honest limit"). This is the gate. At
//! the claim, before the CAS, the door RESERVES the block's budget
//! against the actor's hour: the priced spend of the runs that
//! finished in the last hour ([`spent_in`]) plus what this step may
//! spend must fit under the row's cap, else the claim is refused with
//! the three numbers and the step stays in its queue.
//!
//! `spent + budget <= cap` and not `spent >= cap`
//! ([`boss_core::agent::BudgetDecision::decide`]): the recorder judges
//! a run that already happened, so it asks whether the hour was
//! already exhausted; the door judges a run that has not started, so it
//! asks whether the hour has room for it. Two questions, two rules —
//! both answered as a [`BudgetDecision`] value so the refusal reads the
//! same way in both places.
//!
//! THE OTHER HALF OF THE DOOR — CONCURRENCY (backlog 57c108c2,
//! 2026-09-20). The reservation above measures the hour from
//! `agent_runs`, and that table is written AT FINISH: a run that has
//! been claimed and has not reported yet is priced at nothing, because
//! it has no row. So the money gate cannot bound how many runs an
//! actor has going at once — with nothing finished, `spent` stays 0
//! and the cap admits every claim. That is exactly the failure a
//! PULLER would cause: `boss dispatch --next` fired on an interval
//! against a station holding 47 ready packets would claim all 47, each
//! admitted by a budget that could not yet see the previous one.
//!
//! [`in_flight_runs`] is the measurement the money one structurally
//! cannot make: the actor's own `agent-run` packets that are still
//! OPEN — dispatched, running, not yet reported; the claim proxy this
//! replaced read 7 of 6 against an open-run population of zero and
//! jammed the desk (backlog c314921e). [`Concurrency`]
//! judges it against `agents.max_concurrent_runs`, through the SAME
//! one rule ([`BudgetDecision::decide`]) the recorder uses, so "at the
//! cap" does not come to mean two things in two places (§9a). Until
//! this, that column was read in exactly one place — the recorder,
//! AFTER a run finished, against an `in_flight` measured over finished
//! rows — where it can record a fact and stop nothing (backlog
//! 4b103f0f called it "read by nothing", which was one step too kind).
//!
//! The NUMBER is not here, on purpose. A default in this file would be
//! a policy hidden in the substrate; the cap is a registry row per
//! actor, and a row that declares none is unbounded exactly as an
//! undeclared budget is. This car ships the mechanism; an instance
//! that wants a bound writes it into `agents.max_concurrent_runs`.
//!
//! What it deliberately is NOT: a person's gate. A person has no
//! agents row and runs no model; the budget is the agent's, and a
//! person claiming an agent-blocked step is a person doing the work by
//! hand, which the protocol allows (`executor_for`'s "the human always
//! able to claim"). A row with no cap declared reserves nothing — an
//! unbudgeted agent is admitted with nothing to count down, the
//! boot-guard rule every cap reader here follows.

use std::sync::Arc;

use boss_core::agent::{AgentCaps, AgentLoad, BudgetDecision};
use boss_core::job::{Job, JobStatus};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::agent_runs::{ADMISSION_WINDOW, AgentRun, AgentRunLog, RunFilter};
use crate::agents::{AgentRow, AgentsRegistry};

/// Dollars to the micro-USD the caps are declared in. Half up, so a
/// budget of $0.0000005 is one micro, not none.
pub fn usd_to_micros(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * 1_000_000.0).round() as u64
}

/// The rule. `None` cap admits everything; otherwise the reservation
/// must fit: `spent + budget <= cap`.
pub fn reserve(
    cap_usd_micros: Option<u64>,
    spent_usd_micros: u64,
    budget_usd_micros: u64,
) -> BudgetDecision {
    let Some(cap) = cap_usd_micros else {
        return BudgetDecision::Allow {
            remaining_usd_micros: None,
        };
    };
    let asked = spent_usd_micros.saturating_add(budget_usd_micros);
    if asked > cap {
        return BudgetDecision::Deny {
            reason: format!(
                "hourly budget would be exceeded: {spent_usd_micros} usd_micros spent in the \
                 last hour + {budget_usd_micros} this step may spend = {asked}, over the cap \
                 of {cap}"
            ),
        };
    }
    BudgetDecision::Allow {
        remaining_usd_micros: Some(cap - asked),
    }
}

/// The hour-window spend: the priced cost of every run in `runs` — the
/// adapter has already narrowed them to the actor and the window. An
/// unpriced run adds nothing: it cannot add a number it does not have,
/// the same rule `measure_load` applies. Since design 91a9bfe7 a
/// total-only run on a model that declares a blend IS priced and DOES
/// count here — which is the point of that change: the desks were
/// enforcing against 2% of the spend.
pub fn spent_in(runs: &[AgentRun]) -> u64 {
    runs.iter()
        .filter_map(|r| r.usd_micros)
        .fold(0u64, u64::saturating_add)
}

/// What the door measured and decided, for the record either way.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reservation {
    pub actor: String,
    pub spent_usd_micros: u64,
    pub budget_usd_micros: u64,
    pub hourly_budget_usd_micros: Option<u64>,
    pub window_from: DateTime<Utc>,
    pub decision: BudgetDecision,
}

impl Reservation {
    /// The 409 body: every number the refusal rests on, so the reader
    /// never re-derives what the door already measured.
    pub fn refusal_body(&self) -> serde_json::Value {
        serde_json::json!({
            "error": "claim over the agent's hourly budget",
            "actor": self.actor,
            "spent_usd_micros": self.spent_usd_micros,
            "budget_usd_micros": self.budget_usd_micros,
            "hourly_budget_usd_micros": self.hourly_budget_usd_micros,
            "window_from": self.window_from,
            "reason": match &self.decision {
                BudgetDecision::Deny { reason } => reason.clone(),
                BudgetDecision::Allow { .. } => String::new(),
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BudgetDoorError {
    #[error("agents registry: {0}")]
    Agents(#[from] crate::agents::AgentsError),
    #[error("agent runs: {0}")]
    Runs(#[from] crate::agent_runs::AgentRunError),
}

/// The door's two reads: the actor's row (its cap, its model — the
/// model is what the station capability compares) and the actor's
/// runs in the window.
pub struct BudgetDoor {
    pub agents: Arc<dyn AgentsRegistry>,
    pub runs: Arc<dyn AgentRunLog>,
}

impl BudgetDoor {
    /// The claimant's agents row, or `None` for a person or an
    /// unregistered login. `actor_id` is the resolved id the login
    /// door wrote into the header (`agent-claude`, never the address).
    pub async fn agent_row(&self, actor_id: &str) -> Result<Option<AgentRow>, BudgetDoorError> {
        Ok(self
            .agents
            .list()
            .await?
            .into_iter()
            .find(|a| a.id == actor_id))
    }

    /// Reserve `budget_usd` of `row`'s hour, measured at `now`.
    pub async fn reserve(
        &self,
        row: &AgentRow,
        budget_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<Reservation, BudgetDoorError> {
        let window_from = ADMISSION_WINDOW.cutoff(now);
        let runs = self
            .runs
            .list_runs(&RunFilter {
                actor_id: Some(row.id.clone()),
                since: Some(window_from),
                ..RunFilter::default()
            })
            .await?;
        let spent_usd_micros = spent_in(&runs);
        let budget_usd_micros = usd_to_micros(budget_usd);
        let cap = row
            .hourly_budget_usd_micros
            .and_then(|n| u64::try_from(n).ok());
        Ok(Reservation {
            actor: row.id.clone(),
            spent_usd_micros,
            budget_usd_micros,
            hourly_budget_usd_micros: cap,
            window_from,
            decision: reserve(cap, spent_usd_micros, budget_usd_micros),
        })
    }
}

/// Does this step declare an agent run at all? The `agent_model` key
/// car 1 projects onto a materialised step — the same fact that made
/// the packet a member of an `a.<role>.<model>` station and the same
/// one `dispatch::waiting_step` selects on, so what a queue hands out
/// and what this counts are one predicate rather than two opinions.
pub fn declares_an_agent_run(metadata: &serde_json::Value) -> bool {
    metadata.get(crate::agent_spec::MODEL_KEY).is_some()
}

/// How many agent runs `held_by` has IN FLIGHT: its OPEN `agent-run`
/// packets. A run opens at the dispatch and closes at the handback, so
/// this is the population the cap is named after, counted directly.
///
/// IT USED TO COUNT CLAIMED STEPS, AND THAT PROXY DEADLOCKED THE QUEUE
/// (backlog c314921e, measured 2026-09-20). The comment here read "a
/// step goes Active at the claim and leaves it at completion", which is
/// true of a step an agent both claims and completes — and false of the
/// one step kind this gate actually counted. A backlog-item's `build`
/// completes when its CAR closes, and a `ship-a-change` does not close
/// until it is PROVEN in prod, so a claim drained on proof rather than
/// on handback and the cap measured the proof backlog. The end state
/// was cyclic, not merely slow: a car whose own probe wanted a fresh
/// dispatch (`THE CLAIMED STEP NAMES ITS RUN`) held the slot that
/// dispatch needed, so the one event that could prove it was the one
/// event it forbade. Measured at the jam: the cap read 7 of 6 while the
/// open-run population read ZERO — wrong by the whole of it, and in the
/// direction that stops work.
///
/// Counting runs is also the collapse CLAUDE.md §9a asks for: the run
/// is the thing being bounded, so the bound reads the run rather than a
/// second opinion about it. A run whose session died does not hold a
/// slot forever — `agent-run-dies-when-building-is-silent` closes it at
/// twice the step's duration — which is the drain the claim proxy never
/// had.
///
/// `held_by` is a LIST because an actor is spelled more than one way:
/// the registered id and every alias that resolves to it (design
/// 6fda05ae).
pub fn in_flight_runs(open_runs: &[Job], held_by: &[String]) -> u32 {
    let n = open_runs
        .iter()
        .filter(|j| j.status == JobStatus::Open)
        .filter(|j| {
            j.metadata
                .get(RUN_AGENT_KEY)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|a| held_by.iter().any(|h| h == a))
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The key an `agent-run` packet spells its actor under — written by
/// `boss dispatch` when it opens the run.
pub const RUN_AGENT_KEY: &str = "agent";

/// The workflow kind a run packet is filed under.
pub const RUN_KIND: &str = "agent-run";

/// What the concurrency half measured and decided — the same shape
/// [`Reservation`] has, for the same reason: a refusal carries every
/// number it rests on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Concurrency {
    pub actor: String,
    pub in_flight: u32,
    pub max_concurrent_runs: Option<u32>,
    pub decision: BudgetDecision,
}

impl Concurrency {
    /// Judge `in_flight` against the row's cap. A `None` cap admits
    /// everything — the boot-guard rule every cap reader here follows.
    pub fn measure(actor: &str, cap: Option<u32>, in_flight: u32) -> Concurrency {
        Concurrency {
            actor: actor.to_string(),
            in_flight,
            max_concurrent_runs: cap,
            // The ONE rule, with the money half deliberately absent:
            // [`BudgetDoor::reserve`] has already judged that, and
            // asking it again here would report a budget refusal the
            // caller has already passed.
            decision: BudgetDecision::decide(
                AgentCaps {
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: cap,
                },
                AgentLoad {
                    spent_usd_micros: 0,
                    in_flight,
                },
            ),
        }
    }

    /// The 409 body: what the door counted and the cap it counted
    /// against, so the reader never re-derives what the door measured.
    pub fn refusal_body(&self) -> serde_json::Value {
        serde_json::json!({
            "error": "claim past the agent's concurrent-run cap",
            "actor": self.actor,
            "in_flight": self.in_flight,
            "max_concurrent_runs": self.max_concurrent_runs,
            "reason": match &self.decision {
                BudgetDecision::Deny { reason } => reason.clone(),
                BudgetDecision::Allow { .. } => String::new(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reservation_must_fit_under_the_cap_and_no_cap_reserves_nothing() {
        assert!(reserve(None, u64::MAX, u64::MAX).is_allowed());
        // At the cap exactly is admitted: <=, not <.
        assert_eq!(
            reserve(Some(3_000_000), 1_000_000, 2_000_000),
            BudgetDecision::Allow {
                remaining_usd_micros: Some(0)
            }
        );
        let BudgetDecision::Deny { reason } = reserve(Some(3_000_000), 1_000_870, 5_000_000) else {
            panic!("over the cap is a refusal")
        };
        assert!(reason.contains("1000870"), "{reason}");
        assert!(reason.contains("5000000"), "{reason}");
        assert!(reason.contains("3000000"), "{reason}");
        // A cap of zero refuses every positive reservation — declared,
        // not absent.
        assert!(!reserve(Some(0), 0, 1).is_allowed());
        assert!(reserve(Some(0), 0, 0).is_allowed());
        // Overflow saturates rather than wrapping into an admission.
        assert!(!reserve(Some(u64::MAX - 1), u64::MAX, 1).is_allowed());
    }

    #[test]
    fn dollars_become_micros_half_up_and_nothing_below_zero() {
        assert_eq!(usd_to_micros(5.0), 5_000_000);
        assert_eq!(usd_to_micros(0.0000005), 1);
        assert_eq!(usd_to_micros(0.0000004), 0);
        assert_eq!(usd_to_micros(-1.0), 0);
        assert_eq!(usd_to_micros(f64::NAN), 0);
    }

    #[test]
    fn the_refusal_body_carries_every_number() {
        let r = Reservation {
            actor: "agent-claude".into(),
            spent_usd_micros: 1_000_870,
            budget_usd_micros: 5_000_000,
            hourly_budget_usd_micros: Some(3_000_000),
            window_from: "2026-09-18T18:00:00Z".parse().unwrap(),
            decision: reserve(Some(3_000_000), 1_000_870, 5_000_000),
        };
        let body = r.refusal_body();
        assert_eq!(body["actor"], "agent-claude");
        assert_eq!(body["spent_usd_micros"], 1_000_870);
        assert_eq!(body["budget_usd_micros"], 5_000_000);
        assert_eq!(body["hourly_budget_usd_micros"], 3_000_000);
        assert_eq!(body["window_from"], "2026-09-18T18:00:00Z");
        assert!(body["reason"].as_str().unwrap().contains("over the cap"));
    }

    /// A run packet held by `agent`, open or closed.
    fn run(agent: Option<&str>, status: JobStatus) -> Job {
        use boss_core::job::JobId;
        let mut m = serde_json::json!({});
        if let (Some(a), Some(o)) = (agent, m.as_object_mut()) {
            o.insert(RUN_AGENT_KEY.into(), a.into());
        }
        Job {
            id: JobId::new(),
            kind: "agent-run".into(),
            workflow_version: 1,
            subject: boss_core::job::Subject::new("custom", "bosspipeline"),
            title: "run".into(),
            owner_id: "emp-david".into(),
            status,
            priority: boss_core::job::Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata: m,
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    /// THE POPULATION IS THE RUNS (backlog c314921e). Counting claimed
    /// steps measured the proof backlog instead and deadlocked the
    /// queue at 7 of 6 with zero runs actually going.
    #[test]
    fn in_flight_counts_the_actors_open_runs_and_nothing_else() {
        let held_by = vec![
            "agent-claude".to_string(),
            "claude@algedonic.dev".to_string(),
        ];
        let runs = vec![
            // Two runs genuinely going — one under the registered id,
            // one under an alias the registry ties to it.
            run(Some("agent-claude"), JobStatus::Open),
            run(Some("claude@algedonic.dev"), JobStatus::Open),
            // Handed back. Its packet's `build` step may well still be
            // Active — the car has not been proven — but the agent is
            // not running it, which is the whole correction.
            run(Some("agent-claude"), JobStatus::Closed),
            // Somebody else's run.
            run(Some("agent-other"), JobStatus::Open),
            // A run naming no agent belongs to nobody here.
            run(None, JobStatus::Open),
        ];
        assert_eq!(in_flight_runs(&runs, &held_by), 2);
        assert_eq!(in_flight_runs(&[], &held_by), 0);
    }

    #[test]
    fn the_cap_refuses_at_it_and_an_undeclared_cap_admits_everything() {
        // Under the cap is admitted.
        assert!(
            Concurrency::measure("agent-claude", Some(2), 1)
                .decision
                .is_allowed()
        );
        // AT the cap refuses: the claim would make it cap+1.
        let at = Concurrency::measure("agent-claude", Some(2), 2);
        assert!(!at.decision.is_allowed());
        let body = at.refusal_body();
        assert_eq!(body["actor"], "agent-claude");
        assert_eq!(body["in_flight"], 2);
        assert_eq!(body["max_concurrent_runs"], 2);
        assert!(
            body["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("2 of 2 runs in flight"),
            "{body}"
        );
        // A cap of zero refuses every run — declared, not absent.
        assert!(
            !Concurrency::measure("agent-claude", Some(0), 0)
                .decision
                .is_allowed()
        );
        // No cap declared is unbounded, the boot-guard rule.
        let none = Concurrency::measure("agent-claude", None, 47);
        assert!(none.decision.is_allowed());
        assert_eq!(
            none.refusal_body()["max_concurrent_runs"],
            serde_json::Value::Null
        );
    }
}
