//! An answered sweep measurement judges the sweep that asked for it
//! (backlog 970c0c94, measured 2026-09-18: four daily sweeps' Inspect
//! checklists sat assigned to the agent for 24h while their readings —
//! exit 0, no verdict — sat on the ops-requests the
//! `measure-*-sweep-on-inspect-ready` rules had filed).
//!
//! Pinned over the shipped rule directory, not a copy: each
//! `judge-*-sweep-on-report-answered` rule fires on an ops-request
//! closing `answered` and hands `maintenance.sweep.judge` one (target,
//! verb) pair — and that pair is the SAME pair the sweep's measure rule
//! files, read off that rule's spawn args. Two rules name one verb for
//! one sweep; the pin names whichever drifts (CLAUDE.md §9a).
//!
//! Deliberately NOT judged (yet): image-freshness and stale-build-caches.
//! Their measure rules file `disk-report` — a copy of disk-headroom's —
//! whose verdict is the disk floor and says nothing about image age or
//! cargo caches. A clean disk must not complete a sweep it does not
//! measure, so no judge rule names them until each has a verb that
//! answers its own question; `a_sweep_whose_verb_does_not_judge_its_
//! question_has_no_judge` holds that line.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

mod common;

fn shipped_rules() -> Registry {
    common::authored_registry()
}

/// The close marker in the shape jobs-api's three emit sites produce.
fn report_closed(outcome: &str) -> serde_json::Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-09-18",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "disk-report on forge — the disk-headroom sweep's measurement",
        "subject_id": "forge",
        "parent_step_id": null,
    })
}

fn inspect_ready(target: &str) -> serde_json::Value {
    json!({
        "job_id": format!("sweep-{target}"), "step_id": "s-inspect", "kind": "checklist",
        "subject_kind": "custom", "subject_id": target,
        "assignee_id": null,
        "metadata": { "authority_role": "platform-admin" }
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a str> {
    args.iter()
        .find(|(n, _)| n == k)
        .and_then(|(_, v)| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
}

/// Every (rule name, target, verb) the judge rules hand the handler on
/// an answered ops-request.
fn judges(reg: &Registry) -> Vec<(String, String, String)> {
    match_event(
        reg,
        "jobs.job.closed",
        &report_closed("answered"),
        &NoHelpers,
    )
    .matched
    .iter()
    .flat_map(|h| {
        h.invocations
            .iter()
            .filter(|i| i.handler == "maintenance.sweep.judge")
            .map(move |i| {
                (
                    h.rule_name.clone(),
                    arg(&i.args, "target").unwrap_or("").to_string(),
                    arg(&i.args, "verb").unwrap_or("").to_string(),
                )
            })
    })
    .collect()
}

/// The verb the measure rule for `target` spawns an ops-request with.
fn measured_verb(reg: &Registry, target: &str) -> Option<String> {
    match_event(
        reg,
        "step.ready.checklist",
        &inspect_ready(target),
        &NoHelpers,
    )
    .matched
    .iter()
    .flat_map(|h| h.invocations.iter())
    .filter(|i| i.handler == "jobs.spawn" && arg(&i.args, "kind") == Some("ops-request"))
    .find_map(|i| arg(&i.args, "metadata.verb").map(str::to_string))
}

#[test]
fn the_disk_headroom_and_cluster_conformance_sweeps_are_judged_by_the_verb_that_measures_them() {
    let reg = shipped_rules();
    let judged = judges(&reg);
    for target in ["disk-headroom", "cluster-conformance"] {
        let ours: Vec<_> = judged.iter().filter(|(_, t, _)| t == target).collect();
        assert_eq!(
            ours.len(),
            1,
            "{target}: exactly one judge rule; got {judged:?}"
        );
        let (rule, _, verb) = ours[0];
        assert!(
            rule.starts_with("judge-") && rule.ends_with("-sweep-on-report-answered"),
            "{target}: the rule is named for what it does: {rule}"
        );
        // THE PIN: the verb the judge reads is the verb the sweep files.
        let measured = measured_verb(&reg, target)
            .unwrap_or_else(|| panic!("{target}: its measure rule files no ops-request"));
        assert_eq!(
            verb, &measured,
            "{target}: {rule} judges `{verb}` but measure-{target}-sweep-on-inspect-ready files `{measured}` — one verb per sweep, move both"
        );
    }
}

/// The measure rules for image-freshness and stale-build-caches file
/// `disk-report` — which judges the disk floor, not their question. No
/// judge rule may name them until they have a verb of their own.
#[test]
fn a_sweep_whose_verb_does_not_judge_its_question_has_no_judge() {
    let reg = shipped_rules();
    let judged = judges(&reg);
    for target in ["image-freshness", "stale-build-caches"] {
        let verb = measured_verb(&reg, target).unwrap_or_default();
        let ours: Vec<_> = judged.iter().filter(|(_, t, _)| t == target).collect();
        if verb == "disk-report" {
            assert!(
                ours.is_empty(),
                "{target} is measured by disk-report, whose verdict is the disk floor and not \
                 this sweep's question — a judge rule on it would complete a sweep nothing \
                 measured: {ours:?}"
            );
        }
    }
}

/// A refused ops-request carries no verdict: nothing judges it. Nor
/// does any other kind closing `answered`.
#[test]
fn only_an_answered_ops_request_is_judged() {
    let reg = shipped_rules();
    let refused = match_event(
        &reg,
        "jobs.job.closed",
        &report_closed("refused"),
        &NoHelpers,
    )
    .matched
    .iter()
    .flat_map(|h| h.invocations.iter())
    .filter(|i| i.handler == "maintenance.sweep.judge")
    .count();
    assert_eq!(refused, 0, "a refusal is not a reading");

    let mut other = report_closed("answered");
    other["kind"] = json!("maintenance-sweep");
    let other = match_event(&reg, "jobs.job.closed", &other, &NoHelpers)
        .matched
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|i| i.handler == "maintenance.sweep.judge")
        .count();
    assert_eq!(other, 0, "only an ops-request carries a verb's answer");
}
