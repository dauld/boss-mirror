//! A sign-off whose protocol ROUTES a changes-requested decision
//! declares that Request changes completes it.
//!
//! The defect (backlog da322e8f, measured 2026-09-23 23:20Z). The
//! sign-off surface (`infra/step-plugins/sign-off.js`) records Request
//! changes WITHOUT completing the step — right for an ordinary sign-off,
//! where the same approver re-decides once the thing is revised. But
//! page-audit's `revise` is ready_when `steps.review.done AND
//! steps.review.metadata.decision = "changes-requested"`, so a founder's
//! change request was saved and then sat: three page reviews (/ux/parts,
//! /ux/products, /ux/vendors) waited with the decision recorded and
//! `revise` never opening, until the operator completed them by hand.
//!
//! The two halves are one fact written twice — the predicate that needs
//! the step done on changes-requested, and the step's
//! `changes_requested_completes = true` that makes the surface complete
//! it — so this pins them equal (CLAUDE.md §9a). A new protocol that
//! routes the decision without declaring it fails here naming the
//! workflow and the step, not at a founder's third unrouted review.

use boss_jobs::registry::seedable_platform_workflows;
use serde_json::json;

#[test]
fn every_sign_off_whose_changes_request_is_routed_declares_that_it_completes() {
    let mut routed = Vec::new();
    let mut undeclared = Vec::new();
    for wf in seedable_platform_workflows() {
        for s in wf.steps.iter().filter(|s| s.kind == "sign-off") {
            let done = format!("steps.{}.done", s.title);
            let reads = format!(
                "steps.{}.metadata.decision = \"changes-requested\"",
                s.title
            );
            let is_routed = wf
                .steps
                .iter()
                .any(|o| o.ready_when.contains(&done) && o.ready_when.contains(&reads));
            if !is_routed {
                continue;
            }
            routed.push(format!("{}.{}", wf.kind, s.title));
            if s.metadata_defaults["changes_requested_completes"] != json!(true) {
                undeclared.push(format!("{}.{}", wf.kind, s.title));
            }
        }
    }
    assert!(
        routed.contains(&"page-audit.review".to_string()),
        "the sweep must see the case that stalled (page-audit.review); saw {routed:?}"
    );
    assert!(
        undeclared.is_empty(),
        "these sign-offs route changes-requested on `.done` but do not declare \
         metadata_defaults.changes_requested_completes = true, so the sign-off \
         surface records the request and never completes the step: {undeclared:?}"
    );
}
