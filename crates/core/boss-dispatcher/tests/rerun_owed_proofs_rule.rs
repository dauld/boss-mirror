//! The `rerun-owed-proofs-on-job-closed` rule — the obligation an owed
//! proof becomes (backlog b9005734, approved by David 2026-09-24). A car
//! whose recorded probe waits on a rare event closes LANDED with its
//! proof owed, and declares the event as data (`waits_on.event`, the
//! close of a packet of one kind); when a packet closes, this rule hands
//! the close marker to `jobs.run-car-probes` under `scope = "owed"`, and
//! the handler files a probe run for every owed car that declared THAT
//! close.
//!
//! Pins the rule as the dispatcher boots it: it fires on EVERY close —
//! the match against a car's declared event is per-car data the handler
//! reads, so no static `when` could express it — and it fires the owed
//! scope, not the arrival rule's train scope or the hourly failing one.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "rerun-owed-proofs-on-job-closed";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "p1", "closed_on": "2026-09-24", "kind": kind, "outcome": outcome,
        "title": "t", "subject_id": "s", "parent_step_id": null
    })
}

#[test]
fn every_close_hands_the_owed_scope_its_marker() {
    let reg = rule();
    for (kind, outcome) in [
        ("ops-request", serde_json::json!("answered")),
        ("maintenance-playground-crawl", serde_json::json!("red")),
        ("cut-a-release", serde_json::Value::Null),
    ] {
        let hits = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &NoHelpers).matched;
        assert_eq!(hits.len(), 1, "a {kind} close fires the obligation once");
        let inv = &hits[0].invocations[0];
        assert_eq!(inv.handler, "jobs.run-car-probes");
        assert!(
            inv.args
                .iter()
                .any(|(k, v)| k == "scope" && *v == Value::String("owed".into())),
            "the obligation runs the OWED scope: {:?}",
            inv.args
        );
    }
}

#[test]
fn no_other_topic_fires_it() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.created",
        &closed("ops-request", serde_json::Value::Null),
        &NoHelpers,
    )
    .matched;
    assert!(hits.is_empty());
}
