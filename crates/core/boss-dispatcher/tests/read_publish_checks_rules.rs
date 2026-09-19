//! The two rules that run publish-to-github v7's `read-checks` step
//! (backlog 321f1409, 2026-09-19), pinned over the shipped rule
//! directory, not a copy:
//!
//! - `read-publish-checks-on-read-checks-ready` files the forge's
//!   read-publish-checks request the moment the step goes ready (open-pr
//!   completed, the mirror PR is open), carrying the publish packet's id
//!   as `for_publish` — the open-pr filing rule's shape one step later;
//! - `complete-publish-read-checks-on-read-publish-checks-answered`
//!   reads the verb's answer line back onto the step, and on a non-zero
//!   exit annotates the still-open step and files the alert.
//!
//! Measured before either existed: the mirror's CodeQL ran after the
//! packet closed, PR #238 was merged over 64 unread alerts and #239
//! stalled on 109, and nothing in the record said so.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{MatchedInvocation, Registry, match_event};

mod common;

const FILING: &str = "read-publish-checks-on-read-checks-ready";
const ANSWER: &str = "complete-publish-read-checks-on-read-publish-checks-answered";

fn rule(name: &str) -> Registry {
    common::authored_rule(name)
}

fn arg(inv: &MatchedInvocation, k: &str) -> Value {
    inv.args
        .iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {:?}", inv.args))
}

/// A `step.ready.task` payload as boss-jobs builds it.
fn step_ready(metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "job_id": "p1", "step_id": "s1", "kind": "task",
        "subject_kind": "custom", "subject_id": "github-mirror",
        "assignee_id": null,
        "metadata": metadata
    })
}

/// A `jobs.job.closed` marker: id, kind and outcome — no metadata.
fn closed(kind: &str, outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "3d9d5f58-adb4-40a9-860a-456063cfe66a",
        "kind": kind, "outcome": outcome,
        "closed_on": "2026-09-19", "parent_step_id": null,
    })
}

#[test]
fn the_read_checks_step_spawns_a_forge_read_request_carrying_the_publish_edge() {
    let reg = rule(FILING);
    let payload = step_ready(serde_json::json!({ "ops_verb": "read-publish-checks" }));
    let outcome = match_event(&reg, "step.ready.task", &payload, &NoHelpers);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1, "exactly one spawn");
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    assert_eq!(arg(inv, "kind"), Value::String("ops-request".into()));
    assert_eq!(arg(inv, "subject"), Value::String("forge".into()));
    let vals: Vec<Value> = inv.args.iter().map(|(_, v)| v.clone()).collect();
    assert!(vals.contains(&Value::String("read-publish-checks".into())));
    assert!(vals.contains(&Value::String("forge".into())));
    assert_eq!(
        arg(inv, "metadata.for_publish"),
        Value::String("p1".into()),
        "the request names the publish packet the answer rule follows back"
    );
}

/// The open-pr marker, no marker, and the done topic are all clean
/// non-matches — the two machine steps share one topic and must never
/// file each other's request.
#[test]
fn other_task_steps_and_the_done_topic_do_not_fire_the_read() {
    let reg = rule(FILING);
    for metadata in [
        serde_json::json!({}),
        serde_json::json!({ "ops_verb": "publish-github-pr" }),
        serde_json::json!({ "authority_role": "platform-admin" }),
    ] {
        let out = match_event(
            &reg,
            "step.ready.task",
            &step_ready(metadata.clone()),
            &NoHelpers,
        );
        assert!(out.matched.is_empty(), "metadata={metadata} must not fire");
        assert!(
            out.skipped.is_empty(),
            "metadata={metadata}: {:?}",
            out.skipped
        );
    }
    let payload = step_ready(serde_json::json!({ "ops_verb": "read-publish-checks" }));
    let out = match_event(&reg, "step.done.task", &payload, &NoHelpers);
    assert!(
        out.matched.is_empty(),
        "step.done.task must not re-file the read"
    );
}

#[test]
fn an_answered_read_request_hands_the_handler_the_edge_the_fields_and_the_failure_mode() {
    let reg = rule(ANSWER);
    let out = match_event(
        &reg,
        "jobs.job.closed",
        &closed("ops-request", "answered"),
        &NoHelpers,
    );
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    assert_eq!(out.matched.len(), 1);
    let inv = &out.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    assert_eq!(
        arg(inv, "verb"),
        Value::String("read-publish-checks".into())
    );
    assert_eq!(arg(inv, "link"), Value::String("for_publish".into()));
    assert_eq!(arg(inv, "steps"), Value::String("read-checks".into()));
    let Value::String(pattern) = arg(inv, "verdict_pattern") else {
        panic!("verdict_pattern is a string");
    };
    for group in ["conclusion", "alerts", "rules"] {
        assert!(
            pattern.contains(&format!("(?P<{group}>")),
            "the pattern names the {group} group: {pattern}"
        );
    }
    assert!(
        !pattern.contains('\\'),
        "a backslash does not survive boss-expr's string lexer (ops_judge.rs): {pattern}"
    );
    // The answer line read-publish-checks.sh prints, verbatim from a
    // run over PR #239's fixture (read_publish_checks_sh.rs holds the
    // script side of this pin).
    let re = regex::Regex::new(&pattern).expect("the pattern is a regex");
    let caps = re
        .captures("read-publish-checks: read failure — 100 alerts in 7 rules over 50 files on 12d4a6827975 (https://github.com/algedonic-dev/boss/pull/239)")
        .expect("the pattern reads the answer line");
    assert_eq!(&caps["conclusion"], "failure");
    assert_eq!(&caps["alerts"], "100");
    assert_eq!(&caps["rules"], "7");
    assert!(
        re.captures("read-publish-checks: FAILED — 3 check-runs on 12d4a6827975, still running: Analyze (rust) after 1500s")
            .is_none(),
        "a FAILED line is not an answer"
    );
    let Value::String(done) = arg(inv, "done_metadata") else {
        panic!("done_metadata is a string");
    };
    let done: serde_json::Value = serde_json::from_str(&done).expect("done_metadata is JSON");
    assert_eq!(done["conclusion"], "{conclusion}");
    assert_eq!(done["alerts"], "{alerts}");
    assert_eq!(done["rules"], "{rules}");
    assert_eq!(
        arg(inv, "on_failure"),
        Value::String("annotate-and-alert".into()),
        "a deadline passed or an unreadable GitHub must annotate the step and file the alert"
    );
}

#[test]
fn other_closes_do_not_fire_the_answer_rule_and_do_not_fail() {
    let reg = rule(ANSWER);
    for (kind, outcome) in [
        ("ops-request", "refused"),
        ("publish-to-github", "pr-opened"),
        ("ship-a-change", "merged"),
    ] {
        let out = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &NoHelpers);
        assert!(out.matched.is_empty(), "{kind}/{outcome} must not fire");
        assert!(
            out.skipped.is_empty(),
            "{kind}/{outcome}: {:?}",
            out.skipped
        );
    }
}
