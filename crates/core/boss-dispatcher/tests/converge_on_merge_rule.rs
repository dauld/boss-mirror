//! The `converge-on-merge` rule (first seeded by migration 202609071700,
//! now `infra/dispatcher/rules/converge-on-merge.toml`) — a merged
//! pr-train fires the cluster converge event-driven, instead of waiting
//! for the cluster-deploy-runner timer to poll forge main.
//!
//! Pins the `when` expression against the expr engine: `spec_slug =
//! "merged"` over the ALWAYS-PRESENT top-level payload field. spec_slug
//! was hoisted into step.done for exactly this — the binder resolves
//! flat identifiers only, and an absent one is PredicateFailed -> retry
//! -> a dead-letter storm on every task-step-done (the same trap
//! notify_on_done_rule.rs guards). The rule must match the merged step
//! and NOT the other kind=task steps (ci, deployed, pr, assemble) it
//! shares a topic with, and must resolve the ops-request the ops-runner
//! then executes (host=forge, verb=converge).

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal "verbatim from 202609071700" (backlog 94f150f9)
/// — measured equal to the file that day, and a copy all the same.
const RULE: &str = "converge-on-merge";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn step_done(spec_slug: &str) -> serde_json::Value {
    serde_json::json!({
        "job_id": "t1", "step_id": "s1", "kind": "task",
        "spec_slug": spec_slug, "notify_on_done": true,
        "subject_kind": "custom", "subject_id": "train-1",
        "metadata": { "authority_role": "platform-admin" }
    })
}

#[test]
fn the_merged_step_spawns_a_forge_converge_ops_request() {
    let reg = rule();
    let hits = match_event(&reg, "step.done.task", &step_done("merged"), &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "the merged step fires exactly one converge");

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
    // host + verb ride as flattened metadata.* args that jobs.spawn
    // rebuilds into nested metadata (the design-review-spawn pattern).
    // Assert by VALUE so the check never depends on how the loader
    // renders a dotted arg key.
    let vals: Vec<Value> = args.iter().map(|(_, v)| v.clone()).collect();
    assert!(
        vals.contains(&Value::String("converge".into())),
        "the converge verb rides the spawn; got {args:?}"
    );
}

#[test]
fn the_other_task_steps_do_not_fire_a_converge() {
    let reg = rule();
    for slug in ["ci", "deployed", "pr", "assemble", "collect"] {
        let hits = match_event(&reg, "step.done.task", &step_done(slug), &NoHelpers).matched;
        assert!(
            hits.is_empty(),
            "spec_slug={slug} must NOT fire the converge"
        );
    }
}
