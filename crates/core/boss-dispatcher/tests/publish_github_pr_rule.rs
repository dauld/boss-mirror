//! The `publish-github-pr-on-open-pr-ready` rule (first seeded by
//! migration 202609081200, now its file under `infra/dispatcher/rules/`)
//! — the machine half of `publish-to-github` v6.
//!
//! When the protocol's `open-pr` step becomes READY (David approved
//! the publish), the dispatcher spawns an ops-request for host=forge
//! verb=publish-github-pr; the root ops-runner on the forge runs the
//! verb, which builds the snapshot commit, pushes it to the dauld fork
//! and opens the PR, then completes `open-pr` with `pr_url`.
//!
//! Routing rides on the STEP's metadata, not its slug: `step.ready.*`
//! payloads carry `metadata` (the step's `metadata_defaults`, stamped
//! at materialization) and do NOT hoist `spec_slug` the way
//! `step.done.*` does. So the bundle stamps `ops_verb =
//! "publish-github-pr"` on the open-pr step, and the rule's `when`
//! reads `metadata.ops_verb`. A step without the marker resolves to
//! Absent, which is UNEQUAL to the literal — the predicate reads
//! false, not PredicateFailed — so every other `task` step on the
//! shared topic is skipped cleanly rather than dead-lettered.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal "verbatim from 202609081200" (backlog 94f150f9) —
/// measured equal to the file that day, and a copy all the same.
const RULE: &str = "publish-github-pr-on-open-pr-ready";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// A `step.ready.task` payload as boss-jobs builds it: job/step ids,
/// kind, subject, assignee, and the step's own metadata.
fn step_ready(metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "job_id": "p1", "step_id": "s1", "kind": "task",
        "subject_kind": "custom", "subject_id": "github-mirror",
        "assignee_id": null,
        "metadata": metadata
    })
}

#[test]
fn the_open_pr_step_spawns_a_forge_publish_ops_request() {
    let reg = rule();
    let payload = step_ready(serde_json::json!({ "ops_verb": "publish-github-pr" }));
    let outcome = match_event(&reg, "step.ready.task", &payload, &NoHelpers);
    assert!(
        outcome.skipped.is_empty(),
        "the predicate must evaluate, not fail: {:?}",
        outcome.skipped
    );
    let hits = outcome.matched;
    assert_eq!(hits.len(), 1, "the open-pr step fires exactly one publish");

    let args = &hits[0].invocations[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
    };
    assert_eq!(get("kind"), Value::String("ops-request".into()));
    assert_eq!(get("subject_kind"), Value::String("custom".into()));
    assert_eq!(get("subject"), Value::String("forge".into()));
    // host + verb ride as flattened metadata.* args (the converge-on-merge
    // pattern). Assert by VALUE so the check never depends on how the
    // loader renders a dotted arg key.
    let vals: Vec<Value> = args.iter().map(|(_, v)| v.clone()).collect();
    assert!(
        vals.contains(&Value::String("publish-github-pr".into())),
        "the publish verb rides the spawn; got {args:?}"
    );
    assert!(
        vals.contains(&Value::String("forge".into())),
        "the ops-request is addressed to the forge host; got {args:?}"
    );
}

/// Every other `task` step shares the topic. A step with no marker, or
/// a different one, must be a clean non-match — false, never an
/// evaluation error, or the shared subscription would dead-letter on
/// every ordinary task becoming ready.
#[test]
fn other_task_steps_do_not_fire_and_do_not_fail() {
    let reg = rule();
    for metadata in [
        serde_json::json!({}),
        serde_json::json!({ "authority_role": "platform-admin" }),
        serde_json::json!({ "ops_verb": "converge" }),
        serde_json::json!({ "ops_verb": "" }),
    ] {
        let outcome = match_event(
            &reg,
            "step.ready.task",
            &step_ready(metadata.clone()),
            &NoHelpers,
        );
        assert!(
            outcome.matched.is_empty(),
            "metadata={metadata} must NOT fire the publish"
        );
        assert!(
            outcome.skipped.is_empty(),
            "metadata={metadata} must evaluate to false, not fail: {:?}",
            outcome.skipped
        );
    }
}

/// The marker on a DONE step is not the trigger: the rule listens to
/// readiness, so the verb runs once, when David's approval makes the
/// step ready — not again when the machine completes it.
#[test]
fn the_done_topic_is_not_subscribed() {
    let reg = rule();
    let payload = step_ready(serde_json::json!({ "ops_verb": "publish-github-pr" }));
    let outcome = match_event(&reg, "step.done.task", &payload, &NoHelpers);
    assert!(
        outcome.matched.is_empty(),
        "step.done.task must not re-fire the publish"
    );
}
