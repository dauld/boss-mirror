//! The `refresh-publish-drift-daily` rule
//! (`infra/dispatcher/rules/`): every day, file one `mirror-drift`
//! ops-request for the forge, so the drift measurement on the packet
//! David signs is today's — whether or not a packet is already open.
//!
//! WHY IT EXISTS (design cb38d806, backlog e1b6ddf7).
//! `publish-to-github-daily` fires only on
//! `NOT open_publish_exists("github-mirror")` — right about twins
//! (9f0c566a), wrong about holds: the PII hold of 2026-09-17 -> 09-18
//! suppressed every day behind it and #239 arrived as a 396-commit /
//! 1319-file snapshot. The guard class is
//! `a-dedup-guard-silently-retires-a-cadence`, and this is its fourth
//! instance.
//!
//! THE PIN IS THE PAIR, not this rule alone. For either answer to
//! `open_publish_exists("github-mirror")` the mirror is measured: no
//! open packet -> the daily publish rule files today's; an open packet
//! -> this rule's measurement lands on the one that is open. A day
//! that does NEITHER is the defect the design was filed for, and
//! `a_held_packet_never_costs_the_day` is what refuses its return.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, NoHelpers, Value};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

const RULE: &str = "refresh-publish-drift-daily";
const DAILY: &str = "publish-to-github-daily";

/// `open_publish_exists(...)` answering a fixed verdict, and refusing
/// any other helper — a stand-in for the jobs-API read the runner
/// binds, so both halves of the pair are exercised on both answers.
struct Held(bool);

impl HelperResolver for Held {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_publish_exists" => Ok(Value::Bool(self.0)),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 19).expect("a day")
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

/// The rule's firing on one day, through the schedule runner that
/// fires it in production.
fn fired(helpers: &dyn HelperResolver) -> Vec<(String, Value)> {
    let reg = common::authored_rule(RULE);
    let (matched, _) = ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day(), helpers);
    assert_eq!(matched.len(), 1, "the rule fires once a day: {matched:?}");
    let inv = &matched[0].invocations;
    assert_eq!(inv.len(), 1, "one spawn: {inv:?}");
    assert_eq!(inv[0].handler, "jobs.spawn");
    inv[0].args.clone()
}

#[test]
fn the_day_files_one_mirror_drift_measurement_for_the_forge() {
    let args = fired(&NoHelpers);
    assert_eq!(
        arg(&args, "kind"),
        Some(&Value::String("ops-request".into()))
    );
    // The packet's subject is the MIRROR — one identity this rule alone
    // files, so the silence sweep derives a cadence it can measure
    // instead of one hidden inside the forge's ordinary traffic.
    assert_eq!(
        arg(&args, "subject"),
        Some(&Value::String("github-mirror".into()))
    );
    // host + verb ride as flattened metadata.* args that jobs.spawn
    // rebuilds into nested metadata; asserted BY VALUE so the check
    // never depends on how the loader renders a dotted key.
    let vals: Vec<&Value> = args.iter().map(|(_, v)| v).collect();
    assert!(
        vals.contains(&&Value::String("mirror-drift".into())),
        "the mirror-drift verb rides the spawn: {args:?}"
    );
    assert!(
        vals.contains(&&Value::String("forge".into())),
        "the host the ops-runner routes on rides the spawn: {args:?}"
    );
    // A MEASUREMENT, never a publication: the publish verb is the one
    // David's approve step opens, and no rule may reach it from here.
    assert!(
        !vals.contains(&&Value::String("publish-github-pr".into())),
        "the refresh must not file the publish verb: {args:?}"
    );
    assert!(
        args.iter().all(|(n, _)| !n.contains("args")),
        "the rule must not set metadata.args — the verb's argv fixes --measure: {args:?}"
    );
}

/// THE PIN the packet asks for. The daily publish rule goes quiet the
/// moment a packet is open — that is its guard working as designed —
/// so the measurement must not depend on the same question. This rule
/// carries NO `when`, so it fires on both answers, and a day is never
/// silent on both halves.
#[test]
fn a_held_packet_never_costs_the_day() {
    let publish = common::authored_rule(DAILY);
    for held in [false, true] {
        let refreshed = fired(&Held(held));
        assert!(
            !refreshed.is_empty(),
            "with a publish packet open = {held}, the measurement did not fire"
        );
        let (spawned, _) =
            ScheduleRunner::matched_for_day(&publish, &HashMap::new(), day(), &Held(held));
        // Non-vacuity in the same breath: the publish rule really does
        // go quiet on a hold and really does fire without one, so the
        // line above is covering a live guard rather than a dead rule.
        assert_eq!(
            spawned.len(),
            usize::from(!held),
            "`{DAILY}` with a packet open = {held}: {spawned:?}"
        );
    }
}

/// The measurement must not be guarded on the packet it measures. A
/// `NOT open_*` here would retire this cadence exactly the way it
/// retired the publish rule's — read off the shipped file, because
/// that is the mistake a later edit would make.
#[test]
fn the_measurement_carries_no_dedup_guard() {
    let raw = parse_raw_path(boss_testing::dispatcher_rules_dir())
        .expect("parse the shipped rule directory");
    let rule = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .unwrap_or_else(|| panic!("{RULE} is not in the shipped directory"));
    assert_eq!(
        rule.when, None,
        "`{RULE}` acquired a `when`. Its idempotence lives in the verb (mirror-drift finds the \
         one open publish packet, or answers `nothing to refresh` and exits 0); a guard here is \
         `a-dedup-guard-silently-retires-a-cadence` a fifth time — the defect this rule was \
         filed for (design cb38d806, backlog e1b6ddf7)."
    );
}

/// The verb the rule names exists, serves the host the rule routes to,
/// and takes NO packet-supplied input — the three facts the rule's
/// safety rests on, read from the verb file rather than restated. The
/// `--measure` in its argv is what makes a rule-filed packet incapable
/// of publishing: `jobs.spawn` carries no args list, so a verb's own
/// argv is the only thing that can select a mode.
#[test]
fn the_verb_the_rule_files_serves_the_forge_and_can_only_measure() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            boss_testing::repo_root().join("infra/ops/verbs/mirror-drift.json"),
        )
        .expect("infra/ops/verbs/mirror-drift.json"),
    )
    .expect("the verb file is JSON");
    assert_eq!(
        v["hosts"].as_array().map(Vec::as_slice),
        Some(&[serde_json::json!("forge")][..]),
        "the verb must serve exactly the host the rule routes to: {v}"
    );
    let argv: Vec<&str> = v["argv"]
        .as_array()
        .expect("argv")
        .iter()
        .filter_map(|a| a.as_str())
        .collect();
    assert_eq!(
        argv,
        vec!["infra/forge/publish-github-pr.sh", "--measure"],
        "the mode must be fixed in the argv, not selectable: {v}"
    );
    assert_eq!(
        v["params"].as_array().map(Vec::len),
        Some(0),
        "the verb must admit no packet-supplied input: {v}"
    );
}
