//! Delegate-subjob (Workflow v2, D7) smoke test.
//!
//! Exercises the two dispatcher rules that drive the spawn → link →
//! resolve loop through the real match-then-dispatch path (the same
//! `Registry::from_toml` → `match_event` → `dispatch` pipeline the NATS
//! runner uses), with `RecordingHandler` standing in for the HTTP
//! handlers so the test asserts *what fired with which resolved args*
//! without a live jobs-api.
//!
//! The rules are READ from `infra/dispatcher/rules/`, so a change to a
//! production arg expression reaches this test. Until 2026-09-14 they
//! were inlined here under a note that they "mirror the directory
//! exactly" — a sync comment, not a mechanism (CLAUDE.md §9a; backlog
//! 94f150f9). Both copies measured equal to their files that day.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::handler::{HandlerRegistry, RecordingHandler, dispatch};
use boss_dispatcher::rules::registry::match_event;
use serde_json::json;

mod common;

/// The two D7 rules, each read from its own file under
/// `infra/dispatcher/rules/` — the spawn half for the `step.ready`
/// tests, the resolve half for the `jobs.job.closed` ones; the two
/// listen on different topics, so no test needs both in one registry.
const SPAWN_RULE: &str = "spawn-subjob-on-delegate-subjob-step-ready";
const RESOLVE_RULE: &str = "resolve-subjob-on-child-job-closed";

#[tokio::test]
async fn step_ready_delegate_subjob_fires_spawn_with_resolved_args() {
    let reg = common::authored_rule(SPAWN_RULE);

    // The `step.ready.delegate-subjob` marker payload, mirroring what
    // boss-jobs `emit_step_ready` publishes: job/step ids, subject
    // identity from the parent Job, and the step metadata carrying the
    // child Workflow to spawn.
    let payload = json!({
        "job_id": "parent-job-1",
        "step_id": "parent-step-1",
        "kind": "delegate-subjob",
        "subject_kind": "asset",
        "subject_id": "SYS-42",
        "metadata": { "subworkflow": "equipment-repair" }
    });

    let matched = match_event(&reg, "step.ready.delegate-subjob", &payload, &NoHelpers).matched;
    assert_eq!(matched.len(), 1, "exactly the spawn rule should match");

    let spawn = RecordingHandler::new("jobs.spawn");
    let mut hreg = HandlerRegistry::new();
    hreg.register(spawn.clone());

    let results = dispatch(
        &matched,
        &hreg,
        "evt-ready-1",
        "step.ready.delegate-subjob",
        &payload,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].outcome.is_ok());

    let calls = spawn.calls().await;
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c.rule_name, "spawn-subjob-on-delegate-subjob-step-ready");
    // Args resolved off the payload: child kind from step metadata,
    // subject passed through from the parent Job, parent step id for the
    // embedded_job linkage. Order is not significant (the args round-trip
    // through a JSON object), so compare as a key→value map.
    let got: std::collections::BTreeMap<String, Value> = c.args.iter().cloned().collect();
    let want: std::collections::BTreeMap<String, Value> = [
        ("kind".to_string(), Value::String("equipment-repair".into())),
        ("subject_kind".to_string(), Value::String("asset".into())),
        ("subject".to_string(), Value::String("SYS-42".into())),
        (
            "parent_step_id".to_string(),
            Value::String("parent-step-1".into()),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn job_closed_for_delegated_child_fires_subjob_resolve() {
    let reg = common::authored_rule(RESOLVE_RULE);

    // A child Job's close marker that carries the delegate back-link.
    let payload = json!({
        "id": "child-job-1",
        "closed_on": "2026-06-04",
        "outcome": "completed",
        "parent_step_id": "parent-step-1"
    });

    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
    assert_eq!(
        matched.len(),
        1,
        "the resolve rule should match a delegated close"
    );

    let resolve = RecordingHandler::new("jobs.subjob_resolve");
    let mut hreg = HandlerRegistry::new();
    hreg.register(resolve.clone());

    let results = dispatch(&matched, &hreg, "evt-close-1", "jobs.job.closed", &payload)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].outcome.is_ok());

    let calls = resolve.calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].rule_name, "resolve-subjob-on-child-job-closed");
    assert_eq!(calls[0].triggering_event_id, "evt-close-1");
}

#[tokio::test]
async fn job_closed_for_ordinary_job_does_not_fire_resolve() {
    let reg = common::authored_rule(RESOLVE_RULE);

    // An ordinary (non-delegated) Job close: the marker still carries
    // the `parent_step_id` key but its value is null, so the
    // `parent_step_id != null` gate rules the resolve rule out.
    let payload = json!({
        "id": "ordinary-job-1",
        "closed_on": "2026-06-04",
        "outcome": "completed",
        "parent_step_id": null
    });

    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
    assert!(
        matched.is_empty(),
        "resolve must not fire for a non-delegated Job close"
    );
}
