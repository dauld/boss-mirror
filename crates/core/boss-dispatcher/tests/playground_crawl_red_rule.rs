//! The `file-backlog-items-on-playground-crawl-red` rule
//! (`infra/dispatcher/rules/`): a maintenance-playground-crawl packet
//! closing `failed` opens one backlog-item per RED route its run step
//! recorded (backlog ac3270c7, design 0e07ce64).
//!
//! Pins the `when` expression against the expr engine over the
//! `jobs.job.closed` payload: `kind` and `outcome` are ALWAYS present
//! on that marker (outcome null for a catch-all close), so the
//! predicate reads false — never PredicateFailed → retry → dead-letter
//! — on every other close it shares the topic with. The rule must fire
//! for exactly a crawl that closed `failed`, with the step, design and
//! area the handler files with, and not for a clean night, another
//! chore's red, or an item it filed itself closing.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "file-backlog-items-on-playground-crawl-red";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

fn closed(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "closed_on": "2026-09-19", "title": "Playground crawl",
        "kind": kind, "outcome": outcome, "subject_id": "infra/maintenance-playground-crawl",
        "parent_step_id": null
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_red_crawl_fires_the_filer_once_with_the_run_step_and_the_design() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed("maintenance-playground-crawl", serde_json::json!("failed")),
        &NoHelpers,
    )
    .matched;
    assert_eq!(hits.len(), 1, "a red crawl fires exactly once");
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "maintenance.chore.file_reds");
    assert_eq!(
        arg(&inv.args, "step"),
        Some(&Value::String("run".into())),
        "the RED lines ride the chore's `run` step (boss-chore.sh)"
    );
    assert_eq!(
        arg(&inv.args, "design"),
        Some(&Value::String("0e07ce64".into())),
        "every filed item carries the crawl's design id"
    );
    assert_eq!(arg(&inv.args, "area"), Some(&Value::String("web".into())));
}

#[test]
fn every_other_close_on_the_topic_is_quietly_false() {
    let reg = rule();
    for (kind, outcome) in [
        (
            "maintenance-playground-crawl",
            serde_json::json!("completed"),
        ),
        ("maintenance-playground-crawl", serde_json::Value::Null),
        ("maintenance-audit-integrity", serde_json::json!("failed")),
        ("backlog-item", serde_json::json!("stale")),
        ("ops-request", serde_json::json!("answered")),
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
            "kind={kind} outcome={outcome} must NOT file crawl reds"
        );
    }
}
