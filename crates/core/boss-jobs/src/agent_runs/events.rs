//! The one event this module emits, and the one place its payload
//! shape is defined.
//!
//! `agents.run.recorded` is a STATE event in the sense
//! `crate::events` uses the word: the payload is the whole recorded
//! run, so `super::rebuild` reconstructs the `agent_runs` row from it
//! without consulting anything else. Declared in the `event_kinds`
//! registry by the same migration that creates the table — an
//! emitted-but-undeclared kind is the defect
//! `infra/lint/emitted-kinds-are-declared.sh` exists to catch.

use boss_core::actor::ActorId;
use boss_core::event::Event;

use super::types::NewAgentRun;

/// An agent run was recorded, with what it cost.
pub const AGENT_RUN_RECORDED: &str = "agents.run.recorded";

/// Build the event for one recorded run.
///
/// `priced` is the rate card's verdict — `None` when no row covered the
/// model — and it rides the payload rather than being recomputed at
/// replay, because the card is mutable data and the price charged was
/// the price on the day.
///
/// `recorded_by` rides as `_actor` exactly as `EventStamp` injects it:
/// it is who FILED the record. The run's own CPU is `actor_id` inside
/// the payload, and the two are different questions.
pub fn run_recorded_event(
    recorded_by: &ActorId,
    run: &NewAgentRun,
    priced: &Option<(u64, String)>,
) -> Event {
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

    fn a_run() -> NewAgentRun {
        NewAgentRun {
            run_id: "run-1".into(),
            actor_id: ActorId::agent("claude", "opus-5"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
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

    #[test]
    fn payload_carries_the_whole_run_plus_the_price() {
        let ev = run_recorded_event(
            &ActorId::agent("claude", "opus-5"),
            &a_run(),
            &Some((123, "opus-5".into())),
        );
        assert_eq!(ev.kind, AGENT_RUN_RECORDED);
        assert_eq!(ev.source, "jobs");
        assert_eq!(ev.payload["run_id"], "run-1");
        assert_eq!(ev.payload["actor_id"], "claude:opus-5");
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
        let ev = run_recorded_event(&ActorId::agent("claude", "mystery"), &a_run(), &None);
        assert!(ev.payload.get("usd_micros").is_some());
        assert!(ev.payload["usd_micros"].is_null());
        assert!(ev.payload["priced_by"].is_null());
    }

    #[test]
    fn a_total_only_run_states_its_total_and_nulls_the_split() {
        let mut run = a_run();
        run.tokens = crate::agent_runs::types::TokenUsage::TotalOnly { total: 142_982 };
        let ev = run_recorded_event(&ActorId::agent("claude", "opus-5"), &run, &None);
        assert_eq!(ev.payload["total_tokens"], 142_982);
        assert!(ev.payload["input_tokens"].is_null());
        assert!(ev.payload["output_tokens"].is_null());
        assert!(ev.payload["usd_micros"].is_null());
    }

    #[test]
    fn the_filer_rides_as_actor_and_is_not_the_runs_own_cpu() {
        let ev = run_recorded_event(&ActorId::human("emp-032"), &a_run(), &None);
        assert_eq!(ev.payload["_actor"], "emp-032");
        assert_eq!(ev.payload["actor_id"], "claude:opus-5");
    }
}
