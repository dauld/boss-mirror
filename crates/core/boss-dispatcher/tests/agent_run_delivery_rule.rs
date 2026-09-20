//! `agent-run-delivers-when-its-step-is-done` at the layer it is
//! implemented in (backlog dd6d44b7): one rule row over the generic
//! `jobs.complete_linked_step`, reading its edge off the COMPLETING
//! STEP rather than off a closing packet's job metadata.
//!
//! The rule is what finally reaches an ANALYST's run. A builder's run
//! lands when its gate goes green or its car arrives, and both of those
//! follow a job-level `agent_run` the gate stamps. A run that ships no
//! car — a measure, a draft, a judgement — has neither, and the only
//! fact that means it succeeded is its own step completing on the
//! packet it was dispatched to. That edge cannot be job-level, because
//! one packet hosts a run PER STEP.
//!
//! Three things are worth a test here, and none of them is visible from
//! the handler's unit tests:
//!
//! 1. The rule SELECTS on the wildcard the way the dispatcher matches
//!    it — a `step.done.<anything>` fires it, whatever the packet's
//!    protocol, because the executing kind is not the rule's business.
//! 2. It binds NO payload identifier. The expr binder makes an absent
//!    identifier a `PredicateFailed`, which NAKs and dead-letters — and
//!    this rule rides the highest-volume step topic there is, so a
//!    predicate that cannot evaluate would take every other handler on
//!    every step done with it.
//! 3. The args name a real step of the LIVE `agent-run` Workflow and a
//!    `result` its enum admits. The rule row is data and the Workflow
//!    is data; nothing but a test connects them. Rename `building`, or
//!    retire `delivered` from the fork, and the completion would 400 on
//!    every analyst run, silently — which is the exact failure this
//!    rule exists to end.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::match_event;
use boss_jobs::registry::seedable_platform_workflows;
use serde_json::json;

mod common;
use common::authored_rule;

const RULE: &str = "agent-run-delivers-when-its-step-is-done";
const HANDLER: &str = "jobs.complete_linked_step";

/// This rule names no helper; an unknown one is a failure, not a
/// silent false, so a helper appearing in the file is caught here.
struct NoHelpers;

impl HelperResolver for NoHelpers {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        Err(EvalError::UnknownHelper(name.to_string()))
    }
}

/// A `step.done.<kind>` marker in the shape `boss-jobs`'s step-done
/// builder emits: the step's whole metadata under `metadata`, the
/// packet under `job_id`, and the two hoisted roots.
fn step_done(kind: &str, metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "job_id": "9876ef0d-0000-4000-8000-000000000001",
        "step_id": "0000ef0d-0000-4000-8000-000000000002",
        "kind": kind,
        "subject_kind": "custom",
        "subject_id": "bosspipeline",
        "workflow_kind": "page-audit",
        "completed_on": "2026-09-19",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": "measure",
    })
}

fn arg_of(reg: &boss_dispatcher::rules::registry::Registry, topic: &str, arg: &str) -> String {
    let outcome = match_event(reg, topic, &step_done("task", json!({})), &NoHelpers);
    assert!(
        outcome.skipped.is_empty(),
        "the rule's predicate failed to evaluate: {:?}",
        outcome.skipped
    );
    let m = outcome
        .matched
        .into_iter()
        .find(|m| m.rule_name == RULE)
        .unwrap_or_else(|| panic!("{RULE} did not match `{topic}`"));
    let inv = m
        .invocations
        .iter()
        .find(|i| i.handler == HANDLER)
        .unwrap_or_else(|| panic!("{RULE} does not invoke {HANDLER}"));
    match inv.args.iter().find(|(k, _)| k == arg) {
        Some((_, Value::String(s))) => s.clone(),
        other => panic!("arg {arg:?} is {other:?}, expected a string"),
    }
}

/// The selection: any completed step, of any kind, on any protocol.
/// The rule does not know — and must not know — which kinds an agent
/// is dispatched to, because that is the packet's business and the
/// page march alone spans 47 routes.
#[test]
fn any_completed_step_of_any_kind_fires_the_delivery() {
    let reg = authored_rule(RULE);
    // The kinds a dispatched step actually wears today, plus one the
    // registry has never seen: the wildcard is the point.
    for kind in ["task", "review-design", "checklist", "some-future-kind"] {
        let outcome = match_event(
            &reg,
            &format!("step.done.{kind}"),
            &step_done(kind, json!({ "agent_run": "not-read-here" })),
            &NoHelpers,
        );
        assert!(
            outcome.skipped.is_empty(),
            "a predicate failed on `step.done.{kind}`, which would NAK the whole topic: {:?}",
            outcome.skipped
        );
        assert_eq!(
            outcome.matched.len(),
            1,
            "the rule fires once on `step.done.{kind}`"
        );
    }
}

/// THE REGRESSION THE SHAPE EXISTS TO PREVENT. The edge lives in
/// NESTED step metadata, which the expr binder cannot reach — it
/// resolves flat top-level identifiers only — so the rule binds
/// nothing and lets the handler read the edge off the marker. A `when`
/// naming `agent_run` would be a `PredicateFailed` on every step done
/// in the system, dead-lettering the busiest topic there is. A step
/// carrying no edge still MATCHES here; it is the handler that returns,
/// before any read.
#[test]
fn a_step_with_no_edge_matches_cleanly_rather_than_failing_a_predicate() {
    let reg = authored_rule(RULE);
    for metadata in [json!({}), json!({ "authority_role": "platform-admin" })] {
        let outcome = match_event(
            &reg,
            "step.done.task",
            &step_done("task", metadata),
            &NoHelpers,
        );
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        assert_eq!(outcome.matched.len(), 1);
    }
}

/// The edge the rule follows is the one `boss dispatch` writes, read
/// off the STEP. `boss-cli` holds the other half of this pin (it owns
/// the write); this half is that the rule row says so.
#[test]
fn the_rule_follows_the_dispatch_edge_on_the_completing_step() {
    let reg = authored_rule(RULE);
    assert_eq!(arg_of(&reg, "step.done.task", "link"), "agent_run");
    assert_eq!(
        arg_of(&reg, "step.done.task", "link_from"),
        "step",
        "off the completing step — one packet hosts a run per step, so a \
         job-level key could not name which"
    );
    assert_eq!(
        arg_of(&reg, "step.done.task", "evidence_key"),
        "delivered",
        "the evidence says which packet and step delivered the run"
    );
}

/// The rule row is data and the `agent-run` Workflow is data; only
/// this connects them. The step it completes must exist and be open
/// at that point in the protocol, and the `result` it writes must be
/// a value the step's own enum admits — a `result` the fork does not
/// name 400s at the write, on every analyst run, forever.
#[test]
fn the_step_and_the_result_are_the_live_agent_run_workflows() {
    let reg = authored_rule(RULE);
    let steps: Vec<String> = arg_of(&reg, "step.done.task", "steps")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let done: serde_json::Value =
        serde_json::from_str(&arg_of(&reg, "step.done.task", "done_metadata"))
            .expect("done_metadata parses as JSON");
    let result = done["result"].as_str().expect("it writes a result");

    let spec = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "agent-run")
        .expect("`agent-run` is a shipped platform Workflow");

    for slug in &steps {
        let step = spec
            .steps
            .iter()
            .find(|s| &s.title == slug)
            .unwrap_or_else(|| panic!("`agent-run` has no step `{slug}` to complete"));
        assert!(
            step.terminal.is_none(),
            "`{slug}` is a terminal — completing it would close the run with no report"
        );
        // Every field required at done is one the rule writes: a
        // missing one 400s the completion and the run stays at
        // `building` until the silence clock calls it died.
        for field in step.fields.iter().filter(|f| f.required) {
            assert!(
                done.get(&field.name).is_some(),
                "`agent-run.{slug}` requires `{}` at done and the rule does not write it",
                field.name
            );
        }
        let admitted: Vec<&str> = step
            .fields
            .iter()
            .find(|f| f.name == "result")
            .map(|f| f.field_type.split('|').map(str::trim).collect())
            .unwrap_or_default();
        assert!(
            admitted.contains(&result),
            "`{result}` is not an `agent-run.{slug}` result: {admitted:?}"
        );
    }

    // …and the value opens the step that records the handback, which
    // no rule ever writes. A `result` that opened nothing would leave
    // a run stuck between `building` and its terminals.
    let needle = format!("steps.building.metadata.result = \"{result}\"");
    let opened = spec
        .steps
        .iter()
        .find(|s| s.ready_when.contains(&needle))
        .unwrap_or_else(|| panic!("no `agent-run` step opens on `{needle}`"));
    assert_eq!(
        opened.title, "reported",
        "`{result}` must open the handback step, not a terminal"
    );
}
