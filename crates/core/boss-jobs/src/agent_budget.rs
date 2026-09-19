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
//! What it deliberately is NOT: a person's gate. A person has no
//! agents row and runs no model; the budget is the agent's, and a
//! person claiming an agent-blocked step is a person doing the work by
//! hand, which the protocol allows (`executor_for`'s "the human always
//! able to claim"). A row with no cap declared reserves nothing — an
//! unbudgeted agent is admitted with nothing to count down, the
//! boot-guard rule every cap reader here follows.

use std::sync::Arc;

use boss_core::agent::BudgetDecision;
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
}
