//! A REJECTED `workflow-design` must not publish, and must still end.
//!
//! The defect (8686485c, the sibling sweep af28e250 asked for).
//! `approve` is a sign-off declaring a REQUIRED enum
//! `decision = pending|approved|rejected|changes-requested`, and
//! `publish` read `ready_when = "steps.approve.done"` and nothing else.
//! So a spec the approver REJECTED reached the `workflow-publish` step
//! exactly as an approved one did, and completing it wrote the rejected
//! spec into the registry. It is af28e250's class with the harm
//! reversed: there a decision to build was dropped, here a decision NOT
//! to publish is.
//!
//! The second half is closure. The protocol had four steps and exactly
//! one terminal, `published`. Gate `publish` on the decision and a
//! rejected packet has nowhere to go at all — so the fix is two steps:
//! `publish` reads the decision, and a new `not-published` terminal
//! catches everything else.
//!
//! Asserted at the SPEC, against the real platform bundle, because
//! that is where the defect lives — these are predicates, and
//! `reevaluate` is the one engine both the live API and the simulator
//! run them through. Driving the approved path through the router
//! would additionally complete a `workflow-publish` step, i.e. perform
//! a registry write, which is a different test's business.

use boss_core::job::{JobId, Step, StepId, StepStatus, Subject};
use boss_jobs::registry::{
    WorkflowSpec, materialize_steps, reevaluate, seedable_platform_workflows,
};

fn spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "workflow-design")
        .expect("workflow-design is in the platform bundle")
}

fn fresh(spec: &WorkflowSpec) -> (Subject, serde_json::Value, Vec<Step>) {
    let subject = Subject::new("custom", "workflow-design-test");
    let job_metadata = serde_json::json!({});
    let steps = materialize_steps(spec, &subject, JobId::new(), &job_metadata, StepId::new);
    (subject, job_metadata, steps)
}

fn idx(spec: &WorkflowSpec, slug: &str) -> usize {
    spec.steps
        .iter()
        .position(|s| s.title == slug)
        .unwrap_or_else(|| panic!("no spec step `{slug}`"))
}

/// Walk the packet to a completed `approve` carrying `decision`, then
/// re-evaluate. `None` completes the sign-off with no decision at all.
fn decide(decision: Option<&str>) -> (WorkflowSpec, Vec<Step>) {
    let spec = spec();
    let (subject, job_metadata, mut steps) = fresh(&spec);
    for slug in ["author", "validate", "approve"] {
        steps[idx(&spec, slug)].status = StepStatus::Completed;
    }
    steps[idx(&spec, "approve")].metadata = match decision {
        Some(d) => serde_json::json!({ "decision": d }),
        None => serde_json::json!({}),
    };
    reevaluate(&spec, &mut steps, &subject, &job_metadata);
    (spec, steps)
}

/// The path that must keep working. This protocol is the meta-kind —
/// every other Workflow in the registry is authored through it, and
/// the tenant bootstrap drives one of these Jobs per Workflow it
/// publishes — so the approved path failing is not one packet's
/// problem.
#[test]
fn an_approved_decision_still_reaches_publish() {
    let (spec, steps) = decide(Some("approved"));
    assert_eq!(
        steps[idx(&spec, "publish")].status,
        StepStatus::Ready,
        "an approved spec must still publish"
    );
    assert_eq!(
        steps[idx(&spec, "not-published")].status,
        StepStatus::Skipped,
        "the other terminal must not sit ready beside the publish it lost to"
    );
}

/// The defect, stated three ways. Every decision that is not `approved`
/// must miss `publish` and reach a terminal.
///
/// `changes-requested` ends the packet rather than looping back to
/// `author`, and that is deliberate: a predicate cannot un-complete a
/// step, so there is no loop to offer. The revision is a NEW
/// workflow-design packet, which is the shape the network framing
/// wants anyway — a terminal for one packet is an admission edge for
/// the next.
///
/// `pending` is in the enum, so a sign-off CAN complete carrying it.
/// It is a decision not taken, and it must not publish.
#[test]
fn no_decision_but_approved_reaches_publish_and_every_one_of_them_terminates() {
    for decision in ["rejected", "changes-requested", "pending"] {
        let (spec, steps) = decide(Some(decision));
        assert_eq!(
            steps[idx(&spec, "publish")].status,
            StepStatus::Skipped,
            "`{decision}` must not publish"
        );
        assert_eq!(
            steps[idx(&spec, "not-published")].status,
            StepStatus::Ready,
            "`{decision}` must reach a terminal — a protocol with nowhere to put a \
             rejection leaves the packet open forever"
        );
    }
}

/// An ABSENT decision must fall to the terminal, never to publish.
///
/// This is why the terminal is stated as the NEGATIVE of `approved`
/// rather than as a list of the other three values. Written positively,
/// a decision outside the enum would skip `publish` AND skip the
/// terminal — every reference terminal, every predicate false — and the
/// packet would sit open with nothing reachable. Written negatively,
/// the unreadable case lands on the terminal.
///
/// The direction matters more here than it did on `backlog-item`. There
/// the two outcomes of getting it wrong were "wedged open" versus
/// "closed on a guess". Here the wrong direction PUBLISHES a spec
/// nobody approved, into the registry every other protocol is authored
/// through. The safe default and the honest default are the same one:
/// only an explicit `approved` publishes.
#[test]
fn an_absent_decision_reaches_the_terminal_and_never_publishes() {
    let (spec, steps) = decide(None);
    assert_eq!(
        steps[idx(&spec, "publish")].status,
        StepStatus::Skipped,
        "an unreadable decision must never publish"
    );
    assert_eq!(
        steps[idx(&spec, "not-published")].status,
        StepStatus::Ready,
        "an unreadable decision must still reach a terminal"
    );
}

/// Neither terminal may be reachable on a freshly created Job.
///
/// The shape that has bitten twice: an outcome step that reads ready at
/// create is auto-completed by the dispatcher's complete-on-ready rule
/// and closes the Job before any work happens. Both predicates here
/// lead with `steps.approve.done`, which is false at create, so the
/// conjunction is false whatever the metadata says.
#[test]
fn no_terminal_is_ready_on_a_freshly_created_job() {
    let spec = spec();
    let (_, _, steps) = fresh(&spec);
    for slug in ["publish", "not-published"] {
        assert_eq!(
            steps[idx(&spec, slug)].status,
            StepStatus::Pending,
            "`{slug}` is a terminal and must not be reachable at create"
        );
    }
}

/// The bundle still passes the same viability lint the publish path
/// and the boot guard both run. `workflow_quarantine` re-lints every
/// active row at boot; since this morning it logs and starts rather
/// than refusing, so an unviable row can no longer take the system of
/// record down — but it would shout on every roll, and this is the
/// meta-kind.
#[test]
fn the_new_version_passes_the_gate_the_boot_guard_runs() {
    let problems = boss_jobs::workflow_lint::gate_active(&spec());
    assert!(
        problems.is_ok(),
        "workflow-design is unviable: {:?}",
        problems.err()
    );
}
