//! The `file-tag-release-on-release-tag-ready` rule (backlog 89c95245)
//! — the filing half of cut-a-release's `tag` step, pinned over the
//! shipped rule directory, not a copy.
//!
//! When a release packet's `tag` step becomes READY the dispatcher
//! hands `ops.file_tag_release` the packet kind and step slug it
//! self-filters on; the handler files the tag-release ops-request the
//! forge answers, and complete-release-tag-on-tag-release-answered
//! copies the read-back onto the step. Measured before the rule was
//! written: a `step.ready.task` payload names no workflow kind and no
//! spec_slug, so the rule has no `when` and every task step on the
//! shared topic reaches the handler, which fetches the one packet
//! and leaves every other kind alone — pinned in the handler's own
//! tests; what is pinned HERE is the rule's shape: the topic, the
//! handler, the two args, and that the done topic is not subscribed.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-tag-release-on-release-tag-ready";
const HANDLER: &str = "ops.file_tag_release";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// A `step.ready.task` payload as boss-jobs builds it: job/step ids,
/// kind, subject, assignee, and the step's own metadata — the
/// tenant's `tag` step carries only its procedure text.
fn step_ready() -> serde_json::Value {
    serde_json::json!({
        "job_id": "7d3a9c1e-2b4f-4e6a-9c8d-1f2e3a4b5c6d", "step_id": "s-tag", "kind": "task",
        "subject_kind": "custom", "subject_id": "boss",
        "assignee_id": "emp-david",
        "metadata": { "procedure": "On a checkout fetched from the forge: ..." }
    })
}

#[test]
fn a_task_step_going_ready_hands_the_handler_the_release_kind_and_the_tag_slug() {
    let reg = rule();
    let outcome = match_event(&reg, "step.ready.task", &step_ready(), &NoHelpers);
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
    assert_eq!(get("workflow_kind"), Value::String("cut-a-release".into()));
    assert_eq!(get("step"), Value::String("tag".into()));
}

/// The rule listens to READINESS: the request is filed once, when the
/// founder's approval makes `tag` ready — not again when the
/// completing rule marks it done, and not on any other step kind.
#[test]
fn the_done_topic_and_other_step_kinds_are_not_subscribed() {
    let reg = rule();
    for topic in [
        "step.done.task",
        "step.ready.checklist",
        "step.ready.sign-off",
    ] {
        let outcome = match_event(&reg, topic, &step_ready(), &NoHelpers);
        assert!(
            outcome.matched.is_empty(),
            "{topic} must not fire the filing: {:?}",
            outcome.matched
        );
    }
}

/// The two rules are one loop: the args this rule files under
/// (`release` as the edge, the verb name) are what the completing
/// rule follows — held equal here so a rename on either side is
/// caught at the tree, not on the first release.
#[test]
fn the_filing_and_the_completing_rule_agree_on_the_edge_and_the_verb() {
    let completing = common::authored_rule("complete-release-tag-on-tag-release-answered");
    let closed = serde_json::json!({
        "id": "r1", "kind": "ops-request", "outcome": "answered",
        "closed_on": "2026-09-19", "title": "tag-release", "subject_id": "forge",
        "parent_step_id": null
    });
    let hits = match_event(&completing, "jobs.job.closed", &closed, &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "{hits:?}");
    let args = &hits[0].invocations[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
    };
    assert_eq!(get("link"), Value::String("release".into()));
    assert_eq!(get("verb"), Value::String("tag-release".into()));
    assert_eq!(get("steps"), Value::String("tag".into()));
}
