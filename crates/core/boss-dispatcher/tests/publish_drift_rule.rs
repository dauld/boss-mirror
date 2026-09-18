//! The `check-publish-drift-on-boss-gcp-converge-moved` rule
//! (`infra/dispatcher/rules/`): the moment boss-gcp's checkout MOVES —
//! its self-converge's `run` step closes `ok` with `converge_from` and
//! `converge_sha` differing — file a `publish-drift` ops-request for
//! boss-gcp. The packet carries NO args, so the verb file's default
//! mode (`--check`) applies: the rule can only ever ask what the drift
//! set is; publishing is a `--for-real` a packet carries on purpose.
//!
//! WHY THIS EVENT (backlog a2f97942, retro 27fad542). The verb reads
//! boss-gcp's checkout, which `boss-gcp-converge` fast-forwards to
//! forge main every 30 minutes and whose `run` step records
//! `converge_from` / `converge_sha` (infra/gcp/boss-gcp-converge.sh).
//! A train's `arrived` close is 22 minutes median after it opens, and
//! the checkout follows up to 30 minutes later — the window the
//! operator's hand `until --check ok` loop covered on 2026-09-18. The
//! converge's own moved-step is the first moment the verb can answer
//! without `not yet`, and it fires once per movement rather than once
//! per 30-minute tick.
//!
//! Pinned over the shipped rule file, not a copy: it fires on exactly
//! the moved run step of that one workflow kind, spawns the verb for
//! that one host with the converge linked back, and stays silent on an
//! unchanged tick, a failed run, another host's converge, and another
//! step of the same packet.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "check-publish-drift-on-boss-gcp-converge-moved";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// The `step.done.task` marker as boss-jobs emits it for a maintenance
/// packet's `run` step (steps.rs: `workflow_kind`, `spec_slug` and the
/// step's own metadata, which boss-step.sh merged the run summary into).
fn run_done(
    workflow_kind: &str,
    spec_slug: &str,
    metadata: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "job_id": "c-1", "step_id": "s-run", "kind": "task",
        "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
        "workflow_kind": workflow_kind,
        "completed_on": "2026-09-18",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": spec_slug
    })
}

fn moved() -> serde_json::Value {
    serde_json::json!({
        "result": "ok",
        "converge_remote": "forge",
        "converge_from": "afd5756c0000000000000000000000000000aaaa",
        "converge_sha": "cb053ed60000000000000000000000000000bbbb",
        "cli_result": "installed"
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn a_moved_boss_gcp_converge_files_one_publish_drift_check_for_boss_gcp() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "step.done.task",
        &run_done("maintenance-boss-gcp-converge", "run", moved()),
        &NoHelpers,
    )
    .matched;
    assert_eq!(
        hits.len(),
        1,
        "the moved run step fires exactly once: {hits:?}"
    );
    let inv = &hits[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    let args = &inv[0].args;
    assert_eq!(
        arg(args, "kind"),
        Some(&Value::String("ops-request".into()))
    );
    assert_eq!(
        arg(args, "subject_kind"),
        Some(&Value::String("custom".into()))
    );
    assert_eq!(
        arg(args, "subject"),
        Some(&Value::String("boss-gcp".into()))
    );
    // host + verb ride as flattened metadata.* args that jobs.spawn
    // rebuilds into nested metadata; asserted by VALUE so the check
    // never depends on how the loader renders a dotted key.
    let vals: Vec<&Value> = args.iter().map(|(_, v)| v).collect();
    assert!(
        vals.contains(&&Value::String("publish-drift".into())),
        "the publish-drift verb rides the spawn: {args:?}"
    );
    assert!(
        vals.contains(&&Value::String("boss-gcp".into())),
        "the host rides the spawn: {args:?}"
    );
    // The converge that moved the checkout is linked back, so the
    // packet says which movement it checks.
    assert!(
        vals.contains(&&Value::String("c-1".into())),
        "the converge's job id rides the spawn: {args:?}"
    );
    // NO args: the verb file's default (`--check`) is the only mode a
    // rule-filed packet can carry. A rule that could say --for-real
    // would be a publish nobody decided.
    assert!(
        args.iter().all(|(n, _)| !n.contains("args")),
        "the rule must not set metadata.args: {args:?}"
    );
    assert!(
        !vals.contains(&&Value::String("--for-real".into())),
        "the rule must not carry --for-real: {args:?}"
    );
}

#[test]
fn an_unchanged_tick_a_failed_run_another_host_or_another_step_stays_silent() {
    let reg = rule();
    let mut unchanged = moved();
    unchanged["converge_from"] = unchanged["converge_sha"].clone();
    let mut failed = moved();
    failed["result"] = serde_json::json!("exit 1");
    for (why, kind, slug, meta) in [
        (
            "an unchanged tick (from == sha)",
            "maintenance-boss-gcp-converge",
            "run",
            unchanged,
        ),
        (
            "a failed run, even one that moved",
            "maintenance-boss-gcp-converge",
            "run",
            failed,
        ),
        (
            "the forge's converge (its checkout is not the one the verb reads)",
            "maintenance-forge-converge",
            "run",
            moved(),
        ),
        (
            "the cluster converge",
            "maintenance-cluster-converge",
            "run",
            moved(),
        ),
        (
            "another step of the same packet",
            "maintenance-boss-gcp-converge",
            "scheduled",
            moved(),
        ),
    ] {
        let hits = match_event(
            &reg,
            "step.done.task",
            &run_done(kind, slug, meta),
            &NoHelpers,
        )
        .matched;
        assert!(
            hits.is_empty(),
            "{why} must NOT file a publish-drift: {hits:?}"
        );
    }
}

/// The verb the rule names exists in the allowlist, serves the host
/// the rule spawns for, and defaults its mode to --check — the three
/// facts the rule's safety rests on, read from the verb file rather
/// than restated.
#[test]
fn the_verb_the_rule_files_serves_boss_gcp_and_defaults_to_check() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            boss_testing::repo_root().join("infra/ops/verbs/publish-drift.json"),
        )
        .expect("infra/ops/verbs/publish-drift.json"),
    )
    .expect("the verb file is JSON");
    assert!(
        v["hosts"]
            .as_array()
            .is_some_and(|h| h.iter().any(|x| x == "boss-gcp")),
        "publish-drift must serve boss-gcp: {}",
        v["hosts"]
    );
    let mode = v["params"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["name"] == "mode"))
        .expect("a mode param");
    assert_eq!(mode["default"], "--check");
}
