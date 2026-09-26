//! The `open-a-packet-when-a-step-loses-its-hold` rule
//! (`infra/dispatcher/rules/open-a-packet-when-a-step-loses-its-hold.toml`,
//! backlog 4bdb8150).
//!
//! A step that holds its assignee's time re-asserts that hold after a
//! racing start, and a third party can have taken the time in the gap.
//! That was a `tracing::warn` nobody read. The step write now records
//! `jobs.step.hold_lost`, and this rule is what makes a person read it:
//! one backlog-item per loss, carrying the step, the window and whose
//! time — the scalars of the event, bound by name — with the event's own
//! id (stamped by `jobs.spawn`) pointing at the reservation rows.
//!
//! The payload below is the shape `boss_jobs::events::step_hold_lost_payload`
//! builds; `rule_payload_contract` holds the rule's bindings to the
//! roster migration 20260926032331 declares for the topic.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "open-a-packet-when-a-step-loses-its-hold";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn hold_lost() -> serde_json::Value {
    serde_json::json!({
        "job_id": "00000000-0000-0000-0000-000000558396",
        "step_id": "11111111-0000-0000-0000-000000000001",
        "assignee_id": "emp-tech",
        "window_start": "2026-09-26T10:00:00+00:00",
        "window_end": "2026-09-26T11:30:00+00:00",
        "held_by_count": 1,
        "held_by": [{ "id": "22222222-0000-0000-0000-000000000002", "reason_ref_id": "another-step" }],
        "_actor": "emp-tech",
    })
}

#[test]
fn a_lost_hold_files_one_backlog_item_naming_the_step_and_its_window() {
    let reg = rule();
    let hits = match_event(&reg, "jobs.step.hold_lost", &hold_lost(), &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "one loss, one packet");
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0].handler, "jobs.spawn");

    let args = &inv[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
    };
    assert_eq!(get("kind"), Value::String("backlog-item".into()));
    assert_eq!(get("subject_kind"), Value::String("custom".into()));
    assert_eq!(get("subject"), Value::String("calendar".into()));
    assert_eq!(
        get("metadata.step_id"),
        Value::String("11111111-0000-0000-0000-000000000001".into())
    );
    assert_eq!(
        get("metadata.job_id"),
        Value::String("00000000-0000-0000-0000-000000558396".into())
    );
    assert_eq!(
        get("metadata.assignee_id"),
        Value::String("emp-tech".into())
    );
    assert_eq!(
        get("metadata.window_start"),
        Value::String("2026-09-26T10:00:00+00:00".into())
    );
    assert_eq!(
        get("metadata.window_end"),
        Value::String("2026-09-26T11:30:00+00:00".into())
    );
    assert_eq!(get("metadata.held_by_count"), Value::Int(1));
    assert_eq!(
        get("metadata.input_channel"),
        Value::String("telemetry/monitoring".into())
    );
}

#[test]
fn no_other_topic_wakes_it() {
    let reg = rule();
    for topic in ["jobs.step.updated", "jobs.job.closed", "step.done.task"] {
        let hits = match_event(&reg, topic, &hold_lost(), &NoHelpers).matched;
        assert!(hits.is_empty(), "{topic} must not file a lost-hold item");
    }
}
