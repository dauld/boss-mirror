//! The feedback obligation, at the layer it is implemented in: two
//! rule rows over generic handlers (job 2c4ae549, migration 117).
//!
//! Everything here loads the registry the way the service does —
//! `common::shipped_registry`: `seed_authored_rules` publishes
//! `infra/dispatcher/rules/` into the `dispatcher_rules` table (the
//! dispatcher's boot step since the collapse, 41ba00cd), then
//! `load_active_rules` + `Registry::from_raw` — and drives `match_event` → `dispatch` with `RecordingHandler`
//! standing in for the HTTP handlers. So the assertions are about what
//! the AUTHORED rules do, not about a fixture copy of them, and not
//! about the rows the migrations happen to leave: a fresh TestDb holds
//! the last MIGRATED version of this rule (v3, 202609082000), and since
//! a rule change is a version bump in the file with no migration
//! (`infra/lint/no-migration-writes-a-dispatcher-rule.sh`), a test that
//! read the table alone would pin a version the tree no longer ships —
//! which is how v4's second route would have gone untested here.
//! (`the_registry_equals_the_authored_directory_after_a_seed` proves the
//! derivation itself.)
//!
//! Three things are worth a test at this layer, and none of them are
//! visible from a handler unit test:
//!
//! 1. The rules SELECT correctly off the close marker — a merged car
//!    fires the completion, an abandoned one does not, a feedback
//!    close fires the notification and not the completion.
//! 2. A close marker with null `kind` / `outcome` (the catch-all
//!    close's shape) evaluates to a clean false. The expr binder makes
//!    an ABSENT identifier a `PredicateFailed`, which the runner NAKs
//!    and eventually dead-letters — so "present and null" versus
//!    "missing" is the difference between a rule that skips and a
//!    retry storm on every unrelated Job close in the system.
//! 3. The `steps` arg names real branches of the LIVE `user-feedback`
//!    Workflow. The rule row is data and the Workflow is data, and
//!    nothing but a test connects them: rename a branch and the rule
//!    would silently complete nothing, which is precisely the failure
//!    mode this whole car exists to remove.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::handler::{HandlerRegistry, RecordingHandler, dispatch};
use boss_dispatcher::rules::registry::{MatchedRule, match_event};
use boss_jobs::registry::{feedback_branch_for_disposition, seedable_platform_workflows};
use boss_testing::TestDb;
use serde_json::json;

mod common;
use common::shipped_registry;

const COMPLETE_RULE: &str = "complete-feedback-branch-on-car-merged";
const NOTIFY_RULE: &str = "notify-filer-on-feedback-terminal";

/// A `jobs.job.closed` marker in the shape all three emit sites
/// produce: every key present, null where there is no answer.
fn close_marker(kind: &str, outcome: serde_json::Value) -> serde_json::Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-08-13",
        "kind": kind,
        "outcome": outcome,
        "parent_step_id": null,
        // Always a real value at the emit sites — every Job has a
        // Subject — but any binding satisfies the predicates here.
        "subject_id": "subject-under-test",
    })
}

/// The registry is loaded whole, so every helper any `jobs.job.closed`
/// rule names must RESOLVE here even though these tests only assert the
/// two feedback rules — `spawn-car-on-sweep-remediated` (v4) calls
/// `open_job_exists`, and an unresolvable helper is a `PredicateFailed`
/// for the whole match, same as an absent identifier. Answering `false`
/// (no open item) keeps that rule inert without dead-lettering the rest.
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

fn arg_of(m: &MatchedRule, handler: &str, arg: &str) -> String {
    let inv = m
        .invocations
        .iter()
        .find(|i| i.handler == handler)
        .unwrap_or_else(|| panic!("rule {} does not invoke {handler}", m.rule_name));
    match inv.args.iter().find(|(k, _)| k == arg) {
        Some((_, Value::String(s))) => s.clone(),
        other => panic!("arg {arg:?} on {handler} is {other:?}, expected a string"),
    }
}

/// The obligation's trigger: a merged car, and the completion fires
/// with the edge and the branch list resolved.
#[tokio::test(flavor = "multi_thread")]
async fn a_merged_car_fires_the_completion_with_its_edge_and_branches() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("merged"));

    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    let m = matched_named(&matched, COMPLETE_RULE)
        .unwrap_or_else(|| panic!("{COMPLETE_RULE} did not match a merged car: {matched:?}"));

    assert_eq!(
        arg_of(m, "jobs.complete_linked_step", "link"),
        "backlog_item",
        "the rule must follow the DECLARED job edge, not a prose field"
    );
    // v5 (a994f533): and every OTHER item the car answers, through the
    // declared list edge — the same constant the edge registry serves,
    // so the rule cannot name a key the write path does not ref-check.
    assert_eq!(
        arg_of(m, "jobs.complete_linked_step", "also_link"),
        boss_jobs::car::ALSO_ANSWERS,
        "the rule must follow the declared list edge too"
    );
    assert_eq!(
        arg_of(m, "jobs.complete_linked_step", "steps"),
        "investigate,design-review,build"
    );

    // v2 (migration 150): the rule carries the completed step kind's
    // own vocabulary. user-feedback v11's design-review is
    // `answer-question` — verdict + answer required at done — and an
    // evidence-only completion would 400 there, breaking the loop on
    // the very version that fixes the review UX. The value must be a
    // JSON OBJECT (the handler tolerates a malformed one, but tolerated
    // is not shipped) whose keys are the two the StepType requires.
    let done: serde_json::Value =
        serde_json::from_str(&arg_of(m, "jobs.complete_linked_step", "done_metadata"))
            .expect("done_metadata parses as JSON");
    assert!(done.get("verdict").is_some_and(|v| v == "approved"));
    assert!(
        done.get("answer")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("{branch}")),
        "the answer names WHAT shipped via the {{branch}} placeholder"
    );

    // …and it reaches the handler through the real dispatch loop.
    // The registry is DERIVED from the rules that actually matched,
    // not hand-listed. `dispatch` refuses an unknown handler, so a
    // hand-list is a second roster of handler names that silently
    // rots: on 2026-08-14 a new rule on this very topic
    // (expire-signals-on-job-closed, migration 128) turned a train red
    // by breaking this test, which is about a DIFFERENT rule. That
    // list was never the subject of the test — it was scaffolding that
    // could fail.
    //
    // Deriving it keeps the assertion honest in the direction that
    // matters: this test says the completion fires ONCE, not that it
    // is the only thing firing. Another rule joining this topic is
    // someone else's business.
    let handler = RecordingHandler::new("jobs.complete_linked_step");
    let mut hreg = HandlerRegistry::new();
    hreg.register(handler.clone());
    // `RecordingHandler::new` wants a &'static str; the handler names
    // come from the parsed rules, so leak them. A test process is the
    // one place that is free.
    for m in &matched {
        for inv in &m.invocations {
            if inv.handler != "jobs.complete_linked_step" {
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
    // Counted BY RULE, not by handler name: `jobs.complete_linked_step`
    // is shared — `agent-run-lands-on-car-merged` (c87fb59b car 3)
    // follows a different edge off the same close — and this test is
    // about THIS rule firing once, exactly as the comment above says.
    let fired = handler
        .calls()
        .await
        .into_iter()
        .filter(|c| c.rule_name == COMPLETE_RULE)
        .count();
    assert_eq!(fired, 1, "the completion fired once");
}

/// An abandoned car answers nothing. The packet stays open for
/// whoever picks the work back up.
#[tokio::test(flavor = "multi_thread")]
async fn an_abandoned_car_completes_nothing() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("abandoned"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    assert!(
        matched_named(&matched, COMPLETE_RULE).is_none(),
        "an abandoned change must not close the packet that authorized it"
    );
}

/// A feedback packet closing notifies its filer, and does NOT feed
/// itself back into the completion rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_feedback_terminal_fires_the_notification_only() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;

    // Every terminal a packet can reach, including the ones triage
    // closes outright.
    for outcome in ["completed", "duplicate", "declined"] {
        let payload = close_marker("user-feedback", json!(outcome));
        let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
        let m = matched_named(&matched, NOTIFY_RULE).unwrap_or_else(|| {
            panic!("{NOTIFY_RULE} did not match a `{outcome}` terminal: {matched:?}")
        });
        assert_eq!(
            arg_of(m, "messages.notify_job_terminal", "recipient_key"),
            "submitted_by",
            "the FILER is the recipient the rule names"
        );
        assert!(
            matched_named(&matched, COMPLETE_RULE).is_none(),
            "a feedback close must not run the completion rule"
        );
    }
}

/// THE regression this shape exists to prevent. A catch-all close
/// carries `kind` and `outcome` as present-but-null; a rule binding
/// them must evaluate to false, not blow up. If `match_event` errors
/// here, the runner NAKs — and it NAKs for every Job close in the
/// system, not just the ones these rules care about.
#[tokio::test(flavor = "multi_thread")]
async fn a_close_marker_with_no_outcome_evaluates_to_a_clean_false() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;

    for payload in [
        close_marker("closes-by-catch-all", json!(null)),
        close_marker("ship-a-change", json!(null)),
        close_marker("pr-train", json!("arrived")),
    ] {
        let outcome = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars);
        // The failure this test exists to catch is unchanged; only its
        // blast radius is. A shipped predicate that will not evaluate
        // used to abort matching for the whole topic and dead-letter
        // the event. It is now skipped, so the symptom is quieter - the
        // rule silently stops firing while its neighbours carry on -
        // which makes asserting on it MORE important, not less.
        assert!(
            outcome.skipped.is_empty(),
            "a shipped predicate failed on a legitimate close marker ({:?}) — \
             it will now be skipped rather than dead-lettering the topic, which \
             means it stops firing without anyone noticing. Payload: {payload:#}",
            outcome.skipped
        );
        let matched = outcome.matched;
        assert!(matched_named(&matched, COMPLETE_RULE).is_none());
        assert!(matched_named(&matched, NOTIFY_RULE).is_none());
    }
}

/// v3 (dda0713c), widened by v4 (1c704bb8): every entry of the rule's
/// `route` list speaks the LIVE triage vocabulary of the kind it names,
/// and the disposition it writes opens a branch the `steps` arg then
/// completes. The route is data, the Workflow is data, and only this
/// connects them — rename the step, retire the `build` disposition, or
/// add a required field to triage, and the route would 400 at the
/// write (or route into nothing) on every landed car, silently. So
/// each of those fails here instead, per kind.
///
/// And the list must name BOTH kinds a car is parked against. v3
/// routed `backlog-item` only; on 2026-09-14 David's own feedback
/// 9827c699 had its car landed and proven inside seventy minutes and
/// stayed open at triage until an operator closed it by hand — the
/// noop the obligation wrote on the car said exactly which step it
/// could not make. A route that drops a kind reverts to that.
#[tokio::test(flavor = "multi_thread")]
async fn every_route_speaks_its_kinds_live_triage_and_opens_a_branch_the_rule_completes() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("merged"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    let m = matched_named(&matched, COMPLETE_RULE).expect("the completion rule matched");

    let routes: Vec<serde_json::Value> =
        match serde_json::from_str(&arg_of(m, "jobs.complete_linked_step", "route"))
            .expect("route parses as JSON")
        {
            serde_json::Value::Array(list) => list,
            one @ serde_json::Value::Object(_) => vec![one],
            other => panic!("route is neither a list nor an object: {other}"),
        };
    let mut kinds: Vec<&str> = routes
        .iter()
        .map(|r| r["kind"].as_str().expect("route.kind"))
        .collect();
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        ["backlog-item", "user-feedback"],
        "the route list names every kind a car is parked against (and each once)"
    );

    let completes: Vec<String> = arg_of(m, "jobs.complete_linked_step", "steps")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let workflows = seedable_platform_workflows();

    for route in &routes {
        let kind = route["kind"].as_str().expect("route.kind");
        let step_slug = route["step"].as_str().expect("route.step");
        let written = route["metadata"]
            .as_object()
            .expect("route.metadata is an object");
        let disposition = written["disposition"]
            .as_str()
            .expect("the route chooses a disposition");

        let spec = workflows
            .iter()
            .find(|w| w.kind == kind)
            .unwrap_or_else(|| panic!("route.kind `{kind}` is not a shipped platform Workflow"));
        let routing = spec
            .steps
            .iter()
            .find(|s| s.title == step_slug)
            .unwrap_or_else(|| panic!("`{kind}` has no step `{step_slug}` to route through"));

        // Every field the routing step REQUIRES at done is one the
        // route writes — a missing one 400s the completion, and the
        // obligation would fall back to the noop note it was built to
        // replace.
        for field in routing.fields.iter().filter(|f| f.required) {
            assert!(
                written.contains_key(&field.name),
                "`{kind}.{step_slug}` requires `{}` at done and the route does not write it",
                field.name
            );
        }
        // …and the disposition it writes is one the step's enum admits.
        let enum_values = routing
            .fields
            .iter()
            .find(|f| f.name == "disposition")
            .map(|f| f.field_type.split('|').map(str::trim).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            enum_values.contains(&disposition),
            "`{disposition}` is not a `{kind}.{step_slug}` disposition: {enum_values:?}"
        );

        // The branch that disposition opens must be one the `steps`
        // arg completes, and must not be a declared terminal — the
        // whole point is a second write that closes the packet through
        // its own `closed` outcome.
        let needle = format!("steps.{step_slug}.metadata.disposition = \"{disposition}\"");
        let opened = spec
            .steps
            .iter()
            .find(|s| s.ready_when.contains(&needle))
            .unwrap_or_else(|| panic!("no `{kind}` step opens on `{needle}`"));
        assert!(
            opened.terminal.is_none(),
            "`{}` is a terminal — routing there closes `{kind}` with nothing to complete",
            opened.title
        );
        assert!(
            completes.contains(&opened.title),
            "the `{kind}` route opens `{}` but the rule's `steps` arg {completes:?} does not \
             complete it",
            opened.title
        );
    }
}

/// The rule row is data and the Workflow is data; only a test
/// connects them. Every slug the rule names must be a real,
/// non-terminal `user-feedback` branch — rename one in the Workflow
/// and this fails loudly instead of the rule silently completing
/// nothing, which IS the failure mode this car exists to remove.
#[tokio::test(flavor = "multi_thread")]
async fn every_branch_the_rule_names_is_a_live_non_terminal_feedback_branch() {
    let db = TestDb::new().await;
    let reg = shipped_registry(&db).await;
    let payload = close_marker("ship-a-change", json!("merged"));
    let matched = match_event(&reg, "jobs.job.closed", &payload, &NoOpenCars).matched;
    let m = matched_named(&matched, COMPLETE_RULE).expect("the completion rule matched");
    let listed: Vec<String> = arg_of(m, "jobs.complete_linked_step", "steps")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(!listed.is_empty(), "the rule names no branches at all");

    // Every branch a disposition opens, from the shipped Workflow.
    let all_branches: Vec<(String, bool)> = [
        "reproduce",
        "design",
        "build",
        "needs-info",
        "duplicate",
        "decline",
    ]
    .iter()
    .filter_map(|d| feedback_branch_for_disposition(d))
    .map(|b| (b.slug, b.terminal))
    .collect();

    // …and the step each of those opens in turn. Since f90ca046 the
    // `design` route is two steps deep: triage opens the executor's
    // `draft-design`, and the founder's `design-review` is
    // `ready_when = steps.draft-design.done`. A merged car still
    // completes the review — a packet in-flight under the version
    // before this one has it disposition-opened, and a v2 packet
    // reaches it once the design is filed — so the rule keeps naming
    // it, and this test follows the route one hop to find it.
    let feedback = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "user-feedback")
        .expect("user-feedback is a shipped platform Workflow");
    let downstream: Vec<(String, bool)> = feedback
        .steps
        .iter()
        .filter(|s| {
            all_branches
                .iter()
                .any(|(b, _)| s.ready_when == format!("steps.{b}.done"))
        })
        .map(|s| (s.title.clone(), s.terminal.is_some()))
        .collect();
    let on_the_route: Vec<(String, bool)> =
        all_branches.iter().chain(&downstream).cloned().collect();

    for slug in &listed {
        let branch = on_the_route
            .iter()
            .find(|(s, _)| s == slug)
            .unwrap_or_else(|| {
                panic!(
                    "the rule names branch `{slug}`, which no `user-feedback` disposition \
                     opens, directly or one hop on. Live branches: {on_the_route:?}"
                )
            });
        assert!(
            !branch.1,
            "`{slug}` is a DECLARED TERMINAL — triage closes the packet outright there, \
             so there is never an open step for a merged car to complete"
        );
    }

    // And the other direction: a branch added to the Workflow must be
    // a decision someone makes, not an omission nobody notices.
    // `needs-info` is deliberately excluded — a change landing does
    // not answer a question asked of the reporter. So is
    // `draft-design` (f90ca046): a car landing does not draft the
    // design that was asked for — the draft records `design_id`,
    // which a car cannot supply. A packet at the draft when a car
    // merges is left there, noted on both ends by the handler, for
    // the executor to file the design or re-route.
    let mut expected: Vec<String> = on_the_route
        .iter()
        .filter(|(_, terminal)| !*terminal)
        .map(|(s, _)| s.clone())
        .collect();
    expected.sort();
    let mut covered: Vec<String> = listed.clone();
    covered.push("needs-info".to_string());
    covered.push("draft-design".to_string());
    covered.sort();
    covered.dedup();
    assert_eq!(
        covered, expected,
        "the `user-feedback` fork grew (or shrank) a non-terminal branch. Decide \
         whether a shipped change satisfies it: add it to the rule row's `steps` \
         arg, or add it to this test's deliberate-exclusion list alongside \
         `needs-info` and `draft-design`."
    );
}
