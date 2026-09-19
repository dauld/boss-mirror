//! The `complete-publish-pr-step-on-publish-github-pr-answered` rule
//! (backlog f47861a5) — the answer half of publish-to-github's
//! machine step, pinned over the shipped rule directory, not a copy.
//!
//! Measured 2026-09-19 on publish 254177e2: the filing rule spawned
//! ops-request c98a782f the moment David approved, the forge answered
//! it within the minute with `publish-github-pr: FAILED — pushing
//! publish/2026-09-18 to the forge … dubious ownership …`, the request
//! closed `answered` (the verb ran), and nothing read the answer: the
//! open-pr step sat ready for five hours and the yard drew the publish
//! like one in progress. This rule is the reader. What is pinned HERE
//! is its shape — the topic, the selection, the handler, and the args
//! the handler's own tests exercise against c98a782f's exact output.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "complete-publish-pr-step-on-publish-github-pr-answered";
const HANDLER: &str = "jobs.complete_linked_step";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// A `jobs.job.closed` marker as boss-jobs emits it: the packet's id,
/// kind and outcome — and no metadata, which is why the verb rides the
/// handler's args rather than the rule's `when`.
fn closed(kind: &str, outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "c98a782f-adb4-40a9-860a-456063cfe66a",
        "kind": kind,
        "outcome": outcome,
        "closed_on": "2026-09-18",
        "parent_step_id": null,
    })
}

#[test]
fn an_answered_ops_request_hands_the_handler_the_publish_edge_and_the_failure_mode() {
    let reg = rule();
    let outcome = match_event(
        &reg,
        "jobs.job.closed",
        &closed("ops-request", "answered"),
        &NoHelpers,
    );
    assert!(
        outcome.skipped.is_empty(),
        "the rule must evaluate, not fail: {:?}",
        outcome.skipped
    );
    let hits = outcome.matched;
    assert_eq!(hits.len(), 1, "exactly one rule fires: {hits:?}");
    assert_eq!(hits[0].invocations.len(), 1);
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, HANDLER);
    let get = |k: &str| {
        inv.args
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing; got {:?}", inv.args))
    };
    // Only this verb's answers: every answered ops-request fires the
    // topic, and the closed marker carries no metadata to select on.
    assert_eq!(get("verb"), Value::String("publish-github-pr".into()));
    // The edge the filing rule now writes on the request, and the one
    // step on the far end this rule may complete.
    assert_eq!(get("link"), Value::String("for_publish".into()));
    assert_eq!(get("steps"), Value::String("open-pr".into()));
    // The answer line is the verb's `opened <url>` (or the reuse of an
    // already-open PR), and the step's one required field is the url.
    let Value::String(pattern) = get("verdict_pattern") else {
        panic!("verdict_pattern is a string");
    };
    assert!(
        pattern.contains("(?P<pr_url>"),
        "the pattern names the pr_url group: {pattern}"
    );
    assert!(
        !pattern.contains('\\'),
        "a backslash does not survive boss-expr's string lexer (ops_judge.rs): {pattern}"
    );
    let Value::String(done) = get("done_metadata") else {
        panic!("done_metadata is a string");
    };
    let done: serde_json::Value =
        serde_json::from_str(&done).expect("done_metadata is a JSON object");
    assert_eq!(done["pr_url"], "{pr_url}");
    // A verb that printed FAILED must not leave the step untouched —
    // the five silent hours were exactly that. The handler's failure
    // mode annotates the open step and files the alert.
    assert_eq!(
        get("on_failure"),
        Value::String("annotate-and-alert".into())
    );
}

/// A refused request (outside the allowlist) and any other kind's
/// close are not this rule's: false, never an evaluation error, or
/// every close on the shared topic would dead-letter.
#[test]
fn other_closes_do_not_fire_and_do_not_fail() {
    let reg = rule();
    for (kind, outcome) in [
        ("ops-request", "refused"),
        ("publish-to-github", "pr-opened"),
        ("ship-a-change", "merged"),
    ] {
        let out = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &NoHelpers);
        assert!(
            out.matched.is_empty(),
            "{kind}/{outcome} must NOT fire the rule"
        );
        assert!(
            out.skipped.is_empty(),
            "{kind}/{outcome} must evaluate to false, not fail: {:?}",
            out.skipped
        );
    }
}
