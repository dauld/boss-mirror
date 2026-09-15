//! The `run-car-probes-on-train-arrived` rule (first seeded by migration
//! 202609082130, now its file under `infra/dispatcher/rules/`) — a
//! pr-train closing `arrived` runs each boarded
//! car's recorded probe, by filing the ops-request the forge answers
//! (backlog 28ac45ab).
//!
//! Pins the `when` expression against the expr engine over the
//! `jobs.job.closed` payload: `kind` and `outcome` are ALWAYS present
//! on that marker (outcome null for a catch-all close), so the
//! predicate reads false — never PredicateFailed → retry → dead-letter
//! — on every other close it shares the topic with. The rule must fire
//! for exactly a pr-train that ARRIVED, and not for a cancelled train,
//! a merged car, or any other kind's close.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal "verbatim from 202609082130" (backlog 94f150f9) —
/// measured equal to the file that day, and a copy all the same.
const RULE: &str = "run-car-probes-on-train-arrived";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "t1", "closed_on": "2026-09-08",
        "kind": kind, "outcome": outcome, "parent_step_id": null
    })
}

#[test]
fn an_arrived_train_fires_the_probe_runner_once() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed("pr-train", serde_json::json!("arrived")),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "an arrived train fires exactly once");
    assert_eq!(hits[0].invocations[0].handler, "jobs.run-car-probes");
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    let reg = rule();
    for (kind, outcome) in [
        ("pr-train", serde_json::json!("cancelled")),
        ("pr-train", serde_json::Value::Null),
        ("ship-a-change", serde_json::json!("merged")),
        ("ops-request", serde_json::json!("answered")),
        ("backlog-item", serde_json::json!("arrived")),
    ] {
        let hits = match_event(
            &reg,
            "jobs.job.closed",
            &closed(kind, outcome.clone()),
            &NoHelpers,
        )
        .matched;
        assert!(
            hits.is_empty(),
            "kind={kind} outcome={outcome} must NOT run the probes"
        );
    }
}
