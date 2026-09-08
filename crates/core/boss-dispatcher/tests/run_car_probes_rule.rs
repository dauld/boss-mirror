//! The `run-car-probes-on-train-arrived` rule (migration
//! 202609082130) — a pr-train closing `arrived` runs each boarded
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

/// The migration row, expressed as the same TOML the registry loader
/// accepts — expression verbatim from 202609082130.
const RULE: &str = r#"
[[rule]]
name = "run-car-probes-on-train-arrived"
on_event = "jobs.job.closed"
when = "kind = \"pr-train\" AND outcome = \"arrived\""
[[rule.do]]
handler = "jobs.run-car-probes"
"#;

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "t1", "closed_on": "2026-09-08",
        "kind": kind, "outcome": outcome, "parent_step_id": null
    })
}

#[test]
fn an_arrived_train_fires_the_probe_runner_once() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
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
    let reg = Registry::from_toml(RULE).expect("rule parses");
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
