//! `complete-feedback-branch-on-car-merged` v3 (migration
//! 202609082000) — a landed car advances the backlog item it was
//! parked against (dda0713c).
//!
//! The v2 row already fired at the right moment: ship-a-change's
//! `merged` terminal is `ready_when = "steps.proven.done"`, so
//! `jobs.job.closed` with `outcome = "merged"` IS the car proven in
//! production. What v2 could not do was act on an UNTRIAGED backlog
//! item — `build` was `pending`, triage was the open step, and the
//! handler wrote an `obligation_noop` note on the car instead. v3
//! adds a `route` arg: the routing the obligation may make, as data,
//! scoped to the kind whose triage vocabulary it speaks.
//!
//! Pins the row as authored — expression + args verbatim from the
//! migration — the way converge_on_merge_rule.rs pins its own: a
//! merged close selects it and the route resolves; an abandoned close
//! does not select it at all.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

/// The migration row, expressed as the same TOML the registry loader
/// accepts — expression + args verbatim from 202609082000.
const RULE: &str = r#"
[[rule]]
name = "complete-feedback-branch-on-car-merged"
version = 3
on_event = "jobs.job.closed"
when = "kind = \"ship-a-change\" AND outcome = \"merged\""
[[rule.do]]
handler = "jobs.complete_linked_step"
args = { link = "\"backlog_item\"", steps = "\"investigate,design-review,build\"", done_metadata = '"{\"verdict\": \"approved\", \"answer\": \"shipped: {branch} — {title}\"}"', route = '"{\"kind\": \"backlog-item\", \"step\": \"triage\", \"metadata\": {\"disposition\": \"build\", \"evidence\": \"shipped and proven: {branch} — {title} (car {car})\"}}"' }
"#;

fn close_marker(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-09-08",
        "kind": kind,
        "outcome": outcome,
        "parent_step_id": null,
        "subject_id": "fix/x",
    })
}

fn arg_of(args: &[(String, Value)], k: &str) -> String {
    match args.iter().find(|(n, _)| n == k) {
        Some((_, Value::String(s))) => s.clone(),
        other => panic!("arg {k:?} is {other:?}, expected a string; got {args:?}"),
    }
}

#[test]
fn a_proven_car_fires_the_obligation_with_a_route_for_backlog_items() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let payload = close_marker("ship-a-change", serde_json::json!("merged"));
    let hits = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "a merged car fires exactly one obligation");

    let args = &hits[0].invocations[0].args;
    assert_eq!(hits[0].invocations[0].handler, "jobs.complete_linked_step");
    assert_eq!(arg_of(args, "link"), "backlog_item");
    assert_eq!(arg_of(args, "steps"), "investigate,design-review,build");

    // The route is a JSON object naming the kind it may route, the
    // routing step, and the completion vocabulary that step requires
    // at done — `disposition` (the fork) and `evidence` (the record).
    let route: serde_json::Value =
        serde_json::from_str(&arg_of(args, "route")).expect("route parses as JSON");
    assert_eq!(
        route["kind"], "backlog-item",
        "scoped to the backlog item protocol"
    );
    assert_eq!(route["step"], "triage");
    assert_eq!(route["metadata"]["disposition"], "build");
    let evidence = route["metadata"]["evidence"].as_str().unwrap_or_default();
    assert!(
        evidence.contains("{branch}") && evidence.contains("{car}"),
        "the evidence names WHAT shipped via placeholders: {evidence}"
    );

    // v2's translation rides alongside, unchanged.
    let done: serde_json::Value =
        serde_json::from_str(&arg_of(args, "done_metadata")).expect("done_metadata parses");
    assert_eq!(done["verdict"], "approved");
}

#[test]
fn an_abandoned_car_does_not_fire_the_obligation() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    for outcome in [serde_json::json!("abandoned"), serde_json::json!(null)] {
        let payload = close_marker("ship-a-change", outcome.clone());
        let hits = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
        assert!(
            hits.is_empty(),
            "outcome={outcome} must NOT advance the backlog item"
        );
    }
}
