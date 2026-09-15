//! Deciding the design decides the feedback (backlog 5f0b2661): the
//! rule row `complete-feedback-design-review-on-design-doc-published`,
//! at the layer it is implemented in — one rule over the generic
//! `jobs.complete_linked_step` handler, read from the directory the
//! dispatcher boots from (`common::shipped_registry`).
//!
//! THE DEFECT. A feedback routed to `design` gave one person TWO
//! decisions: the design-doc's review (structured questions at
//! /it/design) and the feedback's own `design-review` step, an
//! `answer-question` with no question. The design IS the question, so
//! the second rendered as a statement (David, bug 4f6019d7: "There is
//! no question, just a statement"), and the link between the two was
//! `metadata.design_packet`, a string nothing read. `boss design
//! --answers` now records an `answers` job edge on the design, and this
//! rule follows it when the design publishes.
//!
//! What only this layer can pin, and a handler unit test cannot:
//!
//! 1. The rule SELECTS off the close marker: a `published` design-doc
//!    fires it; a design closed any other way (a hand close carries a
//!    null outcome) does not; a merged car does not.
//! 2. The `link` it follows is a DECLARED `design-doc` job edge, not a
//!    prose field — the registry is what makes the id ref-checked.
//! 3. The step it completes is the LIVE `design-review` of BOTH kinds a
//!    design can be filed for — `user-feedback` and `backlog-item` —
//!    and the verdict it writes is a member of that step's live enum,
//!    with every field the `answer-question` kind requires at done.
//!    (This is also the answer to the packet's side question: backlog-
//!    item's design-review has the SAME gap, and the same rule covers
//!    it — the handler follows the edge to whatever kind it names.)
//! 4. The outcome the rule's `when` names IS the design-doc Workflow's
//!    terminal — rename it and the rule silently stops firing.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::handler::{HandlerRegistry, RecordingHandler, dispatch};
use boss_dispatcher::rules::registry::{MatchedRule, match_event};
use boss_jobs::job_edges::{InMemoryJobEdges, JobEdgesRegistry};
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
use boss_testing::TestDb;
use serde_json::json;

mod common;
use common::shipped_registry;

const RULE: &str = "complete-feedback-design-review-on-design-doc-published";
const HANDLER: &str = "jobs.complete_linked_step";

/// A `jobs.job.closed` marker in the shape all three emit sites
/// produce: every key present, null where there is no answer.
fn close_marker(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    json!({
        "id": "c6bd173e-3dc9-426f-8fff-866a3b2a6117",
        "closed_on": "2026-09-15",
        "kind": kind,
        "outcome": outcome,
        "title": "A car lands where its change goes live",
        "subject_id": "boss-platform",
        "parent_step_id": null,
    })
}

/// The whole registry is loaded, so every helper any `jobs.job.closed`
/// rule names must resolve (see feedback_obligation_rules.rs).
struct NoOpenCars;

impl HelperResolver for NoOpenCars {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_job_exists" => Ok(Value::Bool(false)),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn matched_named<'a>(matched: &'a [MatchedRule], name: &str) -> Option<&'a MatchedRule> {
    matched.iter().find(|m| m.rule_name == name)
}

fn arg_of(m: &MatchedRule, arg: &str) -> Option<String> {
    let inv = m
        .invocations
        .iter()
        .find(|i| i.handler == HANDLER)
        .unwrap_or_else(|| panic!("rule {} does not invoke {HANDLER}", m.rule_name));
    match inv.args.iter().find(|(k, _)| k == arg) {
        Some((_, Value::String(s))) => Some(s.clone()),
        Some((_, other)) => panic!("arg {arg:?} on {HANDLER} is {other:?}, expected a string"),
        None => None,
    }
}

/// The design-doc Workflow's terminal outcomes, read off the shipped
/// bundle — what a `published` close actually carries.
fn design_doc_terminal_outcomes() -> Vec<String> {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "design-doc")
        .expect("design-doc is a shipped platform Workflow")
        .steps
        .into_iter()
        .filter_map(|s| s.terminal.map(|t| t.outcome))
        .collect()
}

/// (1) + (4): the rule fires on the design-doc's OWN terminal outcome,
/// follows the `answers` edge, and reaches the handler through the real
/// dispatch loop once.
#[tokio::test(flavor = "multi_thread")]
async fn a_published_design_fires_the_completion_over_its_answers_edge() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;

    let terminals = design_doc_terminal_outcomes();
    assert_eq!(
        terminals,
        vec!["published".to_string()],
        "design-doc has exactly one terminal today; a second (a declined design, say) is a \
         decision about what THIS rule should write for it, not an omission"
    );
    let payload = close_marker("design-doc", json!(terminals[0]));
    let outcome = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars);
    assert!(
        outcome.skipped.is_empty(),
        "a shipped predicate failed on a design-doc close: {:?}",
        outcome.skipped
    );
    let matched = outcome.matched;
    let m = matched_named(&matched, RULE)
        .unwrap_or_else(|| panic!("{RULE} did not match a published design-doc: {matched:?}"));

    assert_eq!(
        arg_of(m, "link").as_deref(),
        Some("answers"),
        "the rule follows the DECLARED design-doc.answers edge, not design_packet prose"
    );
    assert_eq!(
        arg_of(m, "steps").as_deref(),
        Some("design-review"),
        "a design decides exactly one branch: the design-review its feedback opened"
    );
    assert!(
        arg_of(m, "route").is_none(),
        "no route: a design must not triage a packet nobody routed to design"
    );

    let handler = RecordingHandler::new(HANDLER);
    let mut hreg = HandlerRegistry::new();
    hreg.register(handler.clone());
    for m in &matched {
        for inv in &m.invocations {
            if inv.handler != HANDLER {
                let name: &'static str = Box::leak(inv.handler.clone().into_boxed_str());
                hreg.register(RecordingHandler::new(name));
            }
        }
    }
    let results = dispatch(&matched, &hreg, "evt-1", "jobs.job.closed", &payload)
        .await
        .expect("every named handler is registered");
    assert!(
        results.iter().all(|r| r.outcome.is_ok()),
        "dispatch reported a failure: {results:?}"
    );
    assert_eq!(handler.calls().await.len(), 1, "the completion fired once");
}

/// (1): a design closed WITHOUT publishing — a hand status-PUT carries
/// a null outcome — completes nothing. The design-doc protocol has no
/// declined terminal today, so a design that was abandoned rather than
/// decided leaves the feedback's step to a person; the rule must not
/// fabricate a verdict from silence. And a merged car is the OTHER
/// rule's business.
#[tokio::test(flavor = "multi_thread")]
async fn a_design_closed_without_publishing_completes_nothing() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    for payload in [
        close_marker("design-doc", json!(null)),
        close_marker("design-doc", json!("abandoned")),
        close_marker("ship-a-change", json!("merged")),
        close_marker("user-feedback", json!("completed")),
    ] {
        let outcome = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars);
        assert!(
            outcome.skipped.is_empty(),
            "a shipped predicate failed on a legitimate close marker: {:?} — payload {payload:#}",
            outcome.skipped
        );
        assert!(
            matched_named(&outcome.matched, RULE).is_none(),
            "{RULE} must not fire on {payload:#}"
        );
    }
}

/// (2): the edge the rule follows is declared in the job_edges
/// registry for the closing kind — which is what makes the id on the
/// design ref-checked and prefix-normalised at the write.
#[tokio::test(flavor = "multi_thread")]
async fn the_link_the_rule_follows_is_a_declared_design_doc_edge() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("design-doc", json!("published"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    let m = matched_named(&matched, RULE).expect("the rule matched");
    let link = arg_of(m, "link").expect("link arg");

    let edges = InMemoryJobEdges.list().await.expect("edges");
    let edge = edges
        .iter()
        .find(|e| e.source_kind == "design-doc" && e.field_path == link)
        .unwrap_or_else(|| {
            panic!("the rule follows `{link}` but no job edge declares it on design-doc: {edges:?}")
        });
    assert_eq!(edge.field_kind, "job_id");
    assert_eq!(edge.on_missing, "abort");
}

/// (3): the step the rule completes is the live `design-review` of
/// BOTH kinds a design can be filed for, and what it writes at done is
/// what that step — and the `answer-question` kind under it — require.
/// The rule is data, the Workflows are data, and only this connects
/// them: rename the step, retire the `approved` verdict, or add a
/// required field, and the completion would 400 on every published
/// design, silently — the exact failure this car exists to remove.
#[tokio::test(flavor = "multi_thread")]
async fn the_step_the_rule_completes_is_the_live_design_review_on_both_kinds() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("design-doc", json!("published"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    let m = matched_named(&matched, RULE).expect("the rule matched");

    let steps: Vec<String> = arg_of(m, "steps")
        .expect("steps arg")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let done: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&arg_of(m, "done_metadata").expect("done_metadata arg"))
            .expect("done_metadata parses as a JSON object");
    let verdict = done
        .get("verdict")
        .and_then(|v| v.as_str())
        .expect("the completion writes a verdict");
    let answer = done
        .get("answer")
        .and_then(|v| v.as_str())
        .expect("the completion writes an answer");
    assert!(
        answer.contains("{title}") && answer.contains("{car}"),
        "the answer names the design by title and id via the handler's placeholders: {answer}"
    );

    let workflows = seedable_platform_workflows();
    let step_types = StepRegistry::v1();
    for kind in ["user-feedback", "backlog-item"] {
        let spec = workflows
            .iter()
            .find(|w| w.kind == kind)
            .unwrap_or_else(|| panic!("{kind} is a shipped platform Workflow"));
        for slug in &steps {
            let step = spec
                .steps
                .iter()
                .find(|s| &s.title == slug)
                .unwrap_or_else(|| {
                    panic!("`{kind}` has no step `{slug}` for the rule to complete")
                });
            assert!(
                step.terminal.is_none(),
                "`{kind}.{slug}` is a terminal — there is never an open step there to complete"
            );
            assert_eq!(
                step.kind, "answer-question",
                "`{kind}.{slug}` is the decision surface the verb writes a question onto"
            );
            // Every field the STEP requires at done is written…
            for field in step.fields.iter().filter(|f| f.required) {
                assert!(
                    done.contains_key(&field.name),
                    "`{kind}.{slug}` requires `{}` at done and the rule does not write it",
                    field.name
                );
            }
            // …and the verdict is one its enum admits.
            let allowed: Vec<&str> = step
                .fields
                .iter()
                .find(|f| f.name == "verdict")
                .map(|f| f.field_type.split('|').map(str::trim).collect())
                .unwrap_or_default();
            assert!(
                allowed.contains(&verdict),
                "`{verdict}` is not a `{kind}.{slug}` verdict: {allowed:?}"
            );
            // …and every field the KIND requires at done is written too.
            let kind_spec = step_types
                .get(&step.kind)
                .unwrap_or_else(|| panic!("{} is a registered StepType", step.kind));
            for field in kind_spec.fields.iter().filter(|f| f.required) {
                assert!(
                    done.contains_key(field.name),
                    "`{}` requires `{}` at done and the rule does not write it",
                    step.kind,
                    field.name
                );
            }
        }
    }
}
