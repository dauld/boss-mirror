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
//! Three sweeps are judged: disk-headroom by `disk-report`,
//! cluster-conformance by `conformance-report`, and — since backlog
//! 18df96c4 (2026-09-18) — image-freshness by `ci-image-report`, the
//! verb that reads the runner's floating boss-ci tag against the
//! registry's. Until then image-freshness and stale-build-caches filed
//! `disk-report`, a copy of disk-headroom's, whose verdict is the disk
//! floor and says nothing about image age or cargo caches; a clean disk
//! must not complete a sweep it does not measure, so neither had a
//! judge. The stale-build-caches sweep was RETIRED by the same car: its
//! question (cargo targets on the dev pod) is answered hourly by the
//! maintenance-dev-scratch-reclaim packet on the host where the caches
//! live, where no ops-runner runs. `the_retired_stale_build_caches_
//! sweep_is_neither_measured_nor_judged` holds that line.

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
fn each_measured_sweep_is_judged_by_the_verb_that_measures_it() {
    let reg = shipped_rules();
    let judged = judges(&reg);
    for target in ["disk-headroom", "cluster-conformance", "image-freshness"] {
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

/// The image-freshness sweep's question is the runner's floating
/// boss-ci tag against the registry's — a question `disk-report` never
/// answered (its verdict is the disk floor). The verb that judges it
/// is its own, and the measure rule files that one, not disk-report.
#[test]
fn image_freshness_is_measured_by_its_own_verb_not_the_disk_floor() {
    let reg = shipped_rules();
    assert_eq!(
        measured_verb(&reg, "image-freshness").as_deref(),
        Some("ci-image-report"),
        "measure-image-freshness-sweep-on-inspect-ready must file the verb that reads \
         image freshness; disk-report judges the disk floor, not this question"
    );
}

/// Retired 2026-09-18 (backlog 18df96c4): the stale-build-caches sweep
/// measured with `disk-report`, which cannot see a cargo target dir on
/// the dev pod, and the question it asked is answered hourly by the
/// maintenance-dev-scratch-reclaim packet (stale_targets_reclaimed,
/// targets_removed_mib, worktrees_kept_*) on the host where the caches
/// live. Nothing files a report for it and nothing judges one.
#[test]
fn the_retired_stale_build_caches_sweep_is_neither_measured_nor_judged() {
    let reg = shipped_rules();
    assert_eq!(
        measured_verb(&reg, "stale-build-caches"),
        None,
        "a retired sweep files no measurement"
    );
    let judged = judges(&reg);
    let ours: Vec<_> = judged
        .iter()
        .filter(|(_, t, _)| t == "stale-build-caches")
        .collect();
    assert!(ours.is_empty(), "a retired sweep has no judge: {ours:?}");
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
