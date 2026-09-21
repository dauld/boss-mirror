//! The two events this module emits, and the one place their payload
//! shapes are defined.
//!
//! `agents.run.recorded` is a STATE event in the sense
//! `crate::events` uses the word: the payload is the whole recorded
//! run, so `super::rebuild` reconstructs the `agent_runs` row from it
//! without consulting anything else. Declared in the `event_kinds`
//! registry by the same migration that creates the table — an
//! emitted-but-undeclared kind is the defect
//! `infra/lint/emitted-kinds-are-declared.sh` exists to catch.
//!
//! `agents.run.denied` is the refusal (backlog 7dd9f28c): a run the
//! budget check turned away leaves NO row, so the event is the only
//! record that it was asked for — which actor, against which cap, in
//! which window, and what the refused run itself cost. It drives no
//! projection; the rebuild reads it for nothing. Declared by the same
//! migration that adds `agent_runs.budget`.

use boss_core::actor::ActorId;
use boss_core::agent::{AgentCaps, AgentLoad, BudgetDecision};
use boss_core::event::Event;

use super::types::{ADMISSION_WINDOW, NewAgentRun};

/// An agent run was recorded, with what it cost.
pub const AGENT_RUN_RECORDED: &str = "agents.run.recorded";

/// An agent run was refused against its actor's budget.
pub const AGENT_RUN_DENIED: &str = "agents.run.denied";

/// The run plus the rate card's verdict, as both events carry them.
///
/// `priced` is `None` when no row covered the model, and it rides the
/// payload rather than being recomputed at replay, because the card is
/// mutable data and the price charged was the price on the day.
fn run_with_price(run: &NewAgentRun, priced: &Option<(u64, String)>) -> serde_json::Value {
    let mut payload = serde_json::to_value(run).unwrap_or_default();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "usd_micros".into(),
            match priced {
                Some((micros, _)) => serde_json::json!(micros),
                // Explicit null, not an absent key: "nothing priced
                // this" is the fact, and a missing key reads as a
                // payload that forgot to say.
                None => serde_json::Value::Null,
            },
        );
        obj.insert(
            "priced_by".into(),
            match priced {
                Some((_, model)) => serde_json::json!(model),
                None => serde_json::Value::Null,
            },
        );
    }
    payload
}

/// Build the event for one recorded run.
///
/// `budget` is the admission decision — always an `Allow` here, since
/// a refused run is not recorded — and it rides the payload for the
/// same reason the price does: the cap is registry data, and a rebuild
/// replays the decision that was made rather than re-judging the run
/// against whatever the row says today.
///
/// `recorded_by` rides as `_actor` exactly as `EventStamp` injects it:
/// it is who FILED the record. The run's own CPU is `actor_id` inside
/// the payload, and the two are different questions.
pub fn run_recorded_event(
    recorded_by: &ActorId,
    run: &NewAgentRun,
    priced: &Option<(u64, String)>,
    budget: &BudgetDecision,
) -> Event {
    let mut payload = run_with_price(run, priced);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "budget".into(),
            serde_json::to_value(budget).unwrap_or_default(),
        );
    }
    let payload = boss_core::publisher::inject_actor(payload, recorded_by);
    Event::new(
        "jobs",
        AGENT_RUN_RECORDED,
        payload,
        boss_clock_client::wall_now(),
    )
}

/// Build the event for one refused run: the run as reported and priced
/// (the money is on the log even though the row is not — conservation),
/// plus everything the decision was made from, so the desk can show
/// which actor was refused, for what, and how long until the window
/// rolls (`window_from` is the cutoff; the window is the hour after
/// it).
pub fn run_denied_event(
    recorded_by: &ActorId,
    run: &NewAgentRun,
    priced: &Option<(u64, String)>,
    caps: AgentCaps,
    load: AgentLoad,
    reason: &str,
) -> Event {
    let mut payload = run_with_price(run, priced);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("reason".into(), serde_json::json!(reason));
        obj.insert(
            "window".into(),
            serde_json::to_value(ADMISSION_WINDOW).unwrap_or_default(),
        );
        obj.insert(
            "window_from".into(),
            serde_json::json!(ADMISSION_WINDOW.cutoff(run.started_at)),
        );
        obj.insert(
            "spent_usd_micros".into(),
            serde_json::json!(load.spent_usd_micros),
        );
        obj.insert("in_flight".into(), serde_json::json!(load.in_flight));
        obj.insert(
            "hourly_budget_usd_micros".into(),
            serde_json::json!(caps.hourly_budget_usd_micros),
        );
        obj.insert(
            "max_concurrent_runs".into(),
            serde_json::json!(caps.max_concurrent_runs),
        );
    }
    let payload = boss_core::publisher::inject_actor(payload, recorded_by);
    Event::new(
        "jobs",
        AGENT_RUN_DENIED,
        payload,
        boss_clock_client::wall_now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runs::types::RunOutcome;
    // "Explicit null, not an absent key" is this payload's stated
    // contract (see `run_recorded_event` above), and
    // `payload[key].is_null()` cannot check it: `Index` answers `Null`
    // for a key nobody wrote (backlog 2e4c200f).
    use boss_testing::assert_explicit_null;

    fn a_run() -> NewAgentRun {
        NewAgentRun {
            run_id: "run-1".into(),
            actor_id: ActorId::agent("claude", "opus-5"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
            // As the adapters hand it in: resolved before the event is
            // built, so the payload always states it.
            model: Some("opus-5".into()),
            tokens: crate::agent_runs::types::TokenUsage::Split {
                input: 10,
                output: 2,
            },
            tool_calls: 3,
            job_id: None,
            branch: Some("feat/x".into()),
            detail: serde_json::json!({"host": "dev-pod"}),
        }
    }

    fn unbudgeted() -> BudgetDecision {
        BudgetDecision::Allow {
            remaining_usd_micros: None,
        }
    }

    #[test]
    fn payload_carries_the_whole_run_plus_the_price() {
        let ev = run_recorded_event(
            &ActorId::agent("claude", "opus-5"),
            &a_run(),
            &Some((123, "opus-5".into())),
            &BudgetDecision::Allow {
                remaining_usd_micros: Some(4_000),
            },
        );
        assert_eq!(ev.kind, AGENT_RUN_RECORDED);
        // The admission decision rides the payload so a rebuild replays
        // it rather than re-judging against today's cap.
        assert_eq!(ev.payload["budget"]["kind"], "allow");
        assert_eq!(ev.payload["budget"]["remaining_usd_micros"], 4_000);
        assert_eq!(ev.source, "jobs");
        assert_eq!(ev.payload["run_id"], "run-1");
        assert_eq!(ev.payload["actor_id"], "claude:opus-5");
        // The model rides as its own key (design 6fda05ae): a rebuild
        // reads it here and never parses the actor id for a new event.
        assert_eq!(ev.payload["model"], "opus-5");
        assert_eq!(ev.payload["tool_calls"], 3);
        // The token shape rides the payload the way the wire states it:
        // the total always present, the split present because it was
        // measured. A rebuild reads exactly this back.
        assert_eq!(ev.payload["input_tokens"], 10);
        assert_eq!(ev.payload["total_tokens"], 12);
        assert_eq!(ev.payload["usd_micros"], 123);
        assert_eq!(ev.payload["priced_by"], "opus-5");
        assert_eq!(ev.payload["detail"]["host"], "dev-pod");
    }

    #[test]
    fn an_unpriced_run_says_null_rather_than_omitting_the_key() {
        let ev = run_recorded_event(
            &ActorId::agent("claude", "mystery"),
            &a_run(),
            &None,
            &unbudgeted(),
        );
        assert_explicit_null!(
            ev.payload,
            "usd_micros",
            "nothing priced this, said out loud"
        );
        assert_explicit_null!(
            ev.payload,
            "priced_by",
            "nothing priced this, said out loud"
        );
    }

    #[test]
    fn a_total_only_run_states_its_total_and_nulls_the_split() {
        let mut run = a_run();
        run.tokens = crate::agent_runs::types::TokenUsage::TotalOnly { total: 142_982 };
        let ev = run_recorded_event(
            &ActorId::agent("claude", "opus-5"),
            &run,
            &None,
            &unbudgeted(),
        );
        assert_eq!(ev.payload["total_tokens"], 142_982);
        assert_explicit_null!(ev.payload, "input_tokens", "the split was not measured");
        assert_explicit_null!(ev.payload, "output_tokens", "the split was not measured");
        assert_explicit_null!(
            ev.payload,
            "usd_micros",
            "this fixture's model declares no blend, so its total is unpriced"
        );
    }

    #[test]
    fn the_filer_rides_as_actor_and_is_not_the_runs_own_cpu() {
        let ev = run_recorded_event(&ActorId::human("emp-032"), &a_run(), &None, &unbudgeted());
        assert_eq!(ev.payload["_actor"], "emp-032");
        assert_eq!(ev.payload["actor_id"], "claude:opus-5");
    }

    /// A refusal carries everything the decision was made from AND the
    /// refused run's own price: a desk can show who was refused, for
    /// what, when the window rolls, and the money stays on the log.
    #[test]
    fn a_refusal_names_actor_window_spend_cap_and_the_refused_runs_cost() {
        let ev = run_denied_event(
            &ActorId::human("emp-032"),
            &a_run(),
            &Some((123, "opus-5".into())),
            AgentCaps {
                hourly_budget_usd_micros: Some(1_000),
                max_concurrent_runs: None,
            },
            AgentLoad {
                spent_usd_micros: 1_000,
                in_flight: 0,
            },
            "hourly budget exhausted",
        );
        assert_eq!(ev.kind, AGENT_RUN_DENIED);
        assert_eq!(ev.source, "jobs");
        assert_eq!(ev.payload["_actor"], "emp-032");
        assert_eq!(ev.payload["actor_id"], "claude:opus-5");
        assert_eq!(ev.payload["run_id"], "run-1");
        assert_eq!(ev.payload["reason"], "hourly budget exhausted");
        assert_eq!(ev.payload["window"]["kind"], "last_hour");
        // The hour before the run STARTED (01:00), not before now.
        assert_eq!(ev.payload["window_from"], "2026-09-10T00:00:00Z");
        assert_eq!(ev.payload["spent_usd_micros"], 1_000);
        assert_eq!(ev.payload["hourly_budget_usd_micros"], 1_000);
        assert_eq!(ev.payload["in_flight"], 0);
        assert_explicit_null!(
            ev.payload,
            "max_concurrent_runs",
            "no concurrency cap declared, said out loud"
        );
        assert_eq!(ev.payload["usd_micros"], 123, "the refused run's cost");
        assert_eq!(ev.payload["total_tokens"], 12);
    }
}
