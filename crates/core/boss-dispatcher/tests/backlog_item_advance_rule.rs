//! `complete-feedback-branch-on-car-merged` from v3 (migration
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
//! Pins the row AS THE FILE AUTHORS IT — `infra/dispatcher/rules/`, the
//! directory the dispatcher boots from — not a copy of it: a merged
//! close selects it and the backlog-item route resolves; an abandoned
//! close does not select it at all. Until 2026-09-14 the TOML was
//! inlined here "verbatim from the migration", and when v4 turned
//! `route` into a list the copy kept saying object and kept passing
//! (backlog 488cadca). The per-kind vocabulary check against the live
//! Workflows is feedback_obligation_rules.rs's job; this file is about
//! selection and the shape of the one route it was written for.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal of the v3 row (backlog 488cadca) — a second copy
/// of rule text (CLAUDE.md §9a) that still said `route` was one object
/// after v4 made it a list, and passed.
const RULE: &str = "complete-feedback-branch-on-car-merged";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

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
    let reg = rule();
    let payload = close_marker("ship-a-change", serde_json::json!("merged"));
    let hits = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
    assert_eq!(hits.len(), 1, "a merged car fires exactly one obligation");

    let args = &hits[0].invocations[0].args;
    assert_eq!(hits[0].invocations[0].handler, "jobs.complete_linked_step");
    assert_eq!(arg_of(args, "link"), "backlog_item");
    assert_eq!(arg_of(args, "steps"), "investigate,design-review,build");

    // The route names the kind it may route, the routing step, and the
    // completion vocabulary that step requires at done — `disposition`
    // (the fork) and `evidence` (the record). v3 shipped it as one
    // object; v4 (1c704bb8) made it a LIST, one entry per kind, and the
    // handler accepts both — so this reads both, and asks for the
    // backlog-item entry by name rather than assuming it is the only one.
    let routes: Vec<serde_json::Value> =
        match serde_json::from_str(&arg_of(args, "route")).expect("route parses as JSON") {
            serde_json::Value::Array(list) => list,
            one @ serde_json::Value::Object(_) => vec![one],
            other => panic!("route is neither a list nor an object: {other}"),
        };
    let route = routes
        .iter()
        .find(|r| r["kind"] == "backlog-item")
        .unwrap_or_else(|| panic!("no route is scoped to the backlog item protocol: {routes:?}"));
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
    let reg = rule();
    for outcome in [serde_json::json!("abandoned"), serde_json::json!(null)] {
        let payload = close_marker("ship-a-change", outcome.clone());
        let hits = match_event(&reg, "jobs.job.closed", &payload, &NoHelpers).matched;
        assert!(
            hits.is_empty(),
            "outcome={outcome} must NOT advance the backlog item"
        );
    }
}
