//! Delegate-subjob (Workflow v2, D7) smoke test.
//!
//! Exercises the two dispatcher rules that drive the spawn → link →
//! resolve loop through the real match-then-dispatch path (the same
//! `Registry::from_toml` → `match_event` → `dispatch` pipeline the NATS
//! runner uses), with `RecordingHandler` standing in for the HTTP
//! handlers so the test asserts *what fired with which resolved args*
//! without a live jobs-api.
//!
//! The rule bodies here mirror `infra/dispatcher/rules/` exactly —
//! if the production arg expressions drift, this test drifts with them.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::handler::{HandlerRegistry, RecordingHandler, dispatch};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

/// The two D7 rules, copied verbatim from infra/dispatcher/rules/.
const D7_RULES: &str = r#"
[[rule]]
name = "spawn-subjob-on-delegate-subjob-step-ready"
on_event = "step.ready.delegate-subjob"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "metadata.subworkflow", subject_kind = "subject_kind", subject = "subject_id", parent_step_id = "step_id" }

[[rule]]
name = "resolve-subjob-on-child-job-closed"
on_event = "jobs.job.closed"
when = "parent_step_id != null"
[[rule.do]]
handler = "jobs.subjob_resolve"
"#;

#[tokio::test]
async fn step_ready_delegate_subjob_fires_spawn_with_resolved_args() {
    let reg = Registry::from_toml(D7_RULES).unwrap();

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
    let reg = Registry::from_toml(D7_RULES).unwrap();

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
    let reg = Registry::from_toml(D7_RULES).unwrap();

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
