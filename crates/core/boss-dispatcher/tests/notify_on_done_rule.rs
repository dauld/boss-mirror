//! The `notify-on-step-done-marked` rule (first seeded by migration 106,
//! now `infra/dispatcher/rules/notify-on-step-done-marked.toml`) — BOSS
//! alerting its operators that a wait is over.
//!
//! Pins the exact `when` expression the migration ships against the
//! expr engine: `notify_on_done = true` over the ALWAYS-PRESENT
//! top-level payload field (the binder resolves flat identifiers
//! only; this test caught the dotted-path version failing as
//! UnknownIdentifier → PredicateFailed → a dead-letter storm on
//! every step.done). Must match the opted-in payload, NOT match
//! the unmarked majority — one rule, explicitly-marked steps only,
//! is what keeps this from feeding the inbox-noise fire (6bf43b6f).

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal "verbatim from 106" (backlog 94f150f9) —
/// measured equal to the file that day, and a copy all the same.
const RULE: &str = "notify-on-step-done-marked";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

#[test]
fn marked_step_done_matches_and_unmarked_does_not() {
    let reg = rule();

    let marked = serde_json::json!({
        "job_id": "j1", "step_id": "s1", "kind": "task", "notify_on_done": true,
        "metadata": { "authority_role": "platform-admin", "notify_on_done": true }
    });
    let hits = match_event(&reg, "step.done.task", &marked, &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "opted-in step notifies");
    let (k, v) = &hits[0].invocations[0].args[0];
    assert_eq!(k, "id_prefix");
    assert_eq!(
        v,
        &boss_dispatcher::rules::expr::Value::String("done".into()),
        "the dedup prefix rides the rule args"
    );

    let unmarked = serde_json::json!({
        "job_id": "j1", "step_id": "s2", "kind": "task", "notify_on_done": false,
        "metadata": { "authority_role": "platform-admin" }
    });
    let hits = match_event(&reg, "step.done.task", &unmarked, &NoHelpers).matched;
    assert!(hits.is_empty(), "the unmarked majority stays silent");
}
