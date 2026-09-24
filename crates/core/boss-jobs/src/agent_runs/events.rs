//! The event this module emits, and the one place its payload shape is
//! defined.
//!
//! `agents.run.recorded` is a STATE event in the sense
//! `crate::events` uses the word: the payload is the whole recorded
//! run, so `super::rebuild` reconstructs the `agent_runs` row from it
//! without consulting anything else. Declared in the `event_kinds`
//! registry by the same migration that creates the table — an
//! emitted-but-undeclared kind is the defect
//! `infra/lint/emitted-kinds-are-declared.sh` exists to catch.
//!
//! There was a second, `agents.run.denied` (backlog 7dd9f28c): a run
//! over its actor's cap was refused and left NO row, only that event.
//! Backlog e6b2066f retired the refusal — once a run is priced from
//! what it consumed, about five times the old figure, the refusal would
//! have dropped real runs from the record they are the spend of, and
//! David's direction (2026-09-23) is that a budget is a signal, not a
//! limit. An over-cap run is now a row whose `budget` reads `deny`. The
//! events already on the log stay there; nothing emits the kind.

use boss_core::actor::ActorId;
use boss_core::agent::BudgetDecision;
use boss_core::event::Event;

use super::types::NewAgentRun;

/// An agent run was recorded, with what it cost.
pub const AGENT_RUN_RECORDED: &str = "agents.run.recorded";

/// The run plus the rate card's verdict, as the event carries them.
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
/// `budget` is the budget reading — an `Allow`, or since backlog
/// e6b2066f a `Deny` that refused nothing — and it rides the payload for the
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
}
