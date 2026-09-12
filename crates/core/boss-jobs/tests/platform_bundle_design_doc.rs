//! The design-doc bundle's `review` step cannot complete with a
//! question unanswered (backlog 0ef658e6).
//!
//! Measured 2026-09-11: four design packets sat at `fold` with their
//! review COMPLETED and zero resolutions against 2, 3, 3 and 4 open
//! questions — twelve judgements recorded nowhere, and a fold with
//! nothing to fold from. `resolutions` was not a declared field: the
//! review surface wrote it, nothing required it, and nothing related
//! it to `questions`. The bundle now declares it — required at done,
//! elements `{anchor, decision}`, and `covers = "questions"` — and this
//! pins that contract THROUGH the bundle the seed loads and the
//! validator the step PUT runs, not through a hand-built spec.

use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;
use serde_json::json;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("design-doc has a `{title}` step"))
}

fn questions() -> serde_json::Value {
    json!([
        {"anchor": "host", "title": "Which host?", "proposal": "forge"},
        {"anchor": "credentials", "title": "Where do credentials live?", "proposal": "/etc/boss-ops"},
        {"anchor": "acts", "title": "Which acts become verbs?", "proposal": "reads now"},
    ])
}

#[test]
fn the_bundle_declares_resolutions_covering_questions() {
    let spec = bundled("design-doc");
    let review = step(&spec, "review");
    let res = review
        .fields
        .iter()
        .find(|f| f.name == "resolutions")
        .expect("review declares `resolutions`");
    assert!(res.required, "required at done");
    assert_eq!(res.field_type, "array");
    assert_eq!(
        res.item_keys,
        vec!["anchor".to_string(), "decision".to_string()]
    );
    assert_eq!(res.covers.as_deref(), Some("questions"));
    // The workflow lint accepts the shape it ships.
    let errs = boss_jobs::workflow_lint::validate_workflow(&spec, &StepRegistry::v1());
    assert!(
        errs.is_empty(),
        "the bundled design-doc lints clean: {errs:?}"
    );
}

#[test]
fn a_review_with_a_question_unanswered_is_refused_naming_the_anchor() {
    let spec = bundled("design-doc");
    let review = step(&spec, "review");
    // Zero resolutions — the 2026-09-11 shape.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions(), "resolutions": [] }),
    )
    .unwrap_err();
    let msg = err
        .iter()
        .find(|e| e.field == "resolutions")
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("host") && msg.contains("credentials") && msg.contains("acts"),
        "every open anchor is named: {msg}"
    );
    // The field absent entirely — the surface never wrote it.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions() }),
    )
    .unwrap_err();
    assert!(err.iter().any(|e| e.field == "resolutions"), "{err:?}");
    // Two of three answered: the third is named, the two are not.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions(), "resolutions": [
            {"anchor": "host", "decision": "forge now"},
            {"anchor": "credentials", "decision": "/etc/boss-ops root 0600"}
        ]}),
    )
    .unwrap_err();
    let msg = err
        .iter()
        .find(|e| e.field == "resolutions")
        .unwrap()
        .message
        .clone();
    assert!(msg.contains("acts") && !msg.contains("host"), "{msg}");
    // All three: the review may complete.
    StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions(), "resolutions": [
            {"anchor": "host", "decision": "forge now"},
            {"anchor": "credentials", "decision": "/etc/boss-ops root 0600"},
            {"anchor": "acts", "decision": "reads now; node-converge bounded"}
        ]}),
    )
    .expect("every question answered completes");
}
