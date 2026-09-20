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
//! [`in_flight_claims`] is the measurement the money one structurally
//! cannot make: the actor's own steps that are ACTIVE and declare an
//! agent block — claimed, running, not yet finished. [`Concurrency`]
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
use boss_core::job::{Step, StepStatus};
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
/// unpriced run (a total-only token count) adds nothing: it cannot add
/// a number it does not have, the same rule `measure_load` applies.
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

/// How many agent runs `held_by` has IN FLIGHT: its steps that are
/// Active and declare an agent block. Active is the whole test — a
/// step goes Active at the claim and leaves it at completion — so this
/// counts precisely the runs handed out and not yet finished, which is
/// the population `agent_runs` cannot hold because it is written at
/// finish.
///
/// `held_by` is a LIST because an actor is spelled more than one way:
/// the registered id and every alias that resolves to it (design
/// 6fda05ae). The claim CAS rewrites an alias holder to the registered
/// id, but a step nominated before that landed still carries the
/// alias, and a bound that missed those would under-refuse.
pub fn in_flight_claims(steps: &[Step], held_by: &[String]) -> u32 {
    let n = steps
        .iter()
        .filter(|s| s.status == StepStatus::Active)
        .filter(|s| {
            s.assignee_id
                .as_deref()
                .is_some_and(|a| held_by.iter().any(|h| h == a))
        })
        .filter(|s| declares_an_agent_run(&s.metadata))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

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

    /// A step in whatever state the case needs. Only the four fields
    /// `in_flight_claims` reads carry meaning here.
    fn step(status: StepStatus, assignee: Option<&str>, agent: bool) -> Step {
        use boss_core::job::{JobId, StepId};
        Step {
            id: StepId::from_uuid(uuid::Uuid::new_v4()),
            job_id: JobId::from_uuid(uuid::Uuid::new_v4()),
            kind: "task".into(),
            title: "build".into(),
            spec_slug: Some("build".into()),
            assignee_id: assignee.map(str::to_string),
            status,
            sort_order: 0,
            blocked_by: vec![],
            sign_offs_required: Vec::new(),
            assurance_required: None,
            sign_offs: Vec::new(),
            fields: Vec::new(),
            completed_on: None,
            completed_by: None,
            completed_at: None,
            metadata: if agent {
                serde_json::json!({crate::agent_spec::MODEL_KEY: "opus-5[1m]"})
            } else {
                serde_json::json!({})
            },
            notes: None,
            step_plugin_version: 0,
            embedded_job: None,
        }
    }

    #[test]
    fn in_flight_counts_the_actors_active_agent_claims_and_nothing_else() {
        let held_by = vec![
            "agent-claude".to_string(),
            "claude@algedonic.dev".to_string(),
        ];
        let steps = vec![
            // Two runs genuinely in flight — one held under the
            // registered id, one under an alias the CAS had not yet
            // rewritten.
            step(StepStatus::Active, Some("agent-claude"), true),
            step(StepStatus::Active, Some("claude@algedonic.dev"), true),
            // Ready and claimed is not running: the queue read hands
            // these out, and counting them would refuse the claim the
            // door is judging.
            step(StepStatus::Ready, Some("agent-claude"), true),
            // Somebody else's run.
            step(StepStatus::Active, Some("agent-other"), true),
            // The actor's own hand work: active, theirs, no agent
            // block — not a run, so not a run in flight.
            step(StepStatus::Active, Some("agent-claude"), false),
            // Unassigned.
            step(StepStatus::Active, None, true),
        ];
        assert_eq!(in_flight_claims(&steps, &held_by), 2);
        assert_eq!(in_flight_claims(&[], &held_by), 0);
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
