//! The sweep and the edge must spawn the SAME review.
//!
//! `design-review-spawn` (migration 107) reacts to
//! `docs.design.indexed`; `design-review-level-sweep` (141) asks the
//! same question on a daily schedule, because a level question asked
//! only on an edge cannot notice a review that was closed while its
//! doc still had open questions — which is how roughly twenty-three
//! questions became unreachable on 2026-08-13 (ae8a14f7).
//!
//! `docs.design.sweep` deliberately owns no spawn spec: it decides
//! WHICH docs are orphaned and hands each to `jobs.spawn` as the
//! payload the edge would have delivered. That keeps the shape of a
//! review in registry rows rather than handler code — and it means the
//! two rows now carry the same args, which is a fact living twice
//! (CLAUDE.md §9a). Collapsing it is not available: they are separate
//! registry rows by design, one event-triggered and one clock-
//! triggered. So it gets the test §9a prescribes instead, and the test
//! names the offending key when they drift.
//!
//! The failure this prevents is quiet. Change 107's args — a new
//! metadata field, a different subject_kind — and the edge keeps
//! spawning correct reviews while the sweep spawns subtly wrong ones,
//! visible only as two lineages of the same Job kind on the board.

use std::collections::BTreeMap;

/// One rule, read from its own file — the file name IS the rule name
/// (infra/dispatcher/rules/README.md).
fn rule(rule_name: &str) -> toml::Value {
    let path = format!(
        "{}/../../../infra/dispatcher/rules/{rule_name}.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let doc: toml::Value =
        toml::from_str(&src).unwrap_or_else(|e| panic!("parse {rule_name}.toml: {e}"));
    let rules = doc
        .get("rule")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("{rule_name}.toml has no [[rule]]"))
        .clone();
    assert_eq!(rules.len(), 1, "{rule_name}.toml holds one rule");
    rules[0].clone()
}

/// The `args` map of a rule's single `do` step, by rule name.
fn spawn_args(rule_name: &str) -> (String, BTreeMap<String, String>) {
    let rule = rule(rule_name);
    let dos = rule
        .get("do")
        .and_then(|d| d.as_array())
        .unwrap_or_else(|| panic!("`{rule_name}` has no [[rule.do]]"));
    assert_eq!(
        dos.len(),
        1,
        "`{rule_name}` should have exactly one do-step for this comparison to mean anything"
    );
    let handler = dos[0]
        .get("handler")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let args = dos[0]
        .get("args")
        .and_then(|a| a.as_table())
        .map(|t| {
            t.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    (handler, args)
}

#[test]
fn the_sweep_spawns_exactly_what_the_edge_spawns() {
    let (edge_handler, edge_args) = spawn_args("design-review-spawn");
    let (sweep_handler, sweep_args) = spawn_args("design-review-level-sweep");

    assert_eq!(edge_handler, "jobs.spawn");
    assert_eq!(
        sweep_handler, "docs.design.sweep",
        "the sweep must go through its own handler — it has a level question to answer \
         before anything is spawned"
    );

    // Key by key, so a drift names the field rather than dumping two maps.
    for (k, want) in &edge_args {
        let got = sweep_args.get(k).map(String::as_str);
        assert_eq!(
            got,
            Some(want.as_str()),
            "`{k}`: design-review-spawn says {want:?} but design-review-level-sweep says \
             {got:?} — the sweep hands these straight to jobs.spawn, so a difference here \
             is two lineages of the same review on the board"
        );
    }
    for k in sweep_args.keys() {
        assert!(
            edge_args.contains_key(k),
            "`{k}` is on the sweep but not on design-review-spawn — the sweep must not \
             invent fields the edge does not set"
        );
    }
}

#[test]
fn the_sweep_is_clock_triggered_and_the_edge_is_not() {
    let sweep = rule("design-review-level-sweep");
    assert!(
        sweep.get("schedule").is_some(),
        "the sweep must be clock-triggered — asking the level question only on an event \
         is the defect it exists to fix"
    );
    assert!(
        sweep.get("on").is_none() && sweep.get("on_event").is_none(),
        "a rule is triggered by an event OR a schedule, never both"
    );

    let edge = rule("design-review-spawn");
    assert!(
        edge.get("schedule").is_none(),
        "the edge rule stays an edge — prompt spawning on change is the optimisation \
         the sweep is the floor for"
    );
}
