//! `infra/platform/workflows/pr-train.toml` keeps its decided shape.
//! One pin file per kind file (see `platform_bundle.rs`), so a new
//! protocol touches no shared line.

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::audience::Audience;
use boss_jobs::registry::{WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The actor that drives every task step of a train — the value
/// `completed_by` carries on every one of them in the system of record.
const CONDUCTOR: &str = "automation:train-conductor";

/// The train's task steps, in protocol order. Every one is completed
/// by the conductor and by nothing else.
const CONDUCTORS_STEPS: [&str; 7] = [
    "collect",
    "assemble",
    "pr",
    "ci",
    "merged",
    "deployed",
    "converged",
];

/// The conductor's steps declare the conductor as their audience — one
/// declaration, the `individual` shape (design f5ebd2e1) — and nothing
/// else about who they are for.
///
/// WHY (backlog af796788). Measured 2026-09-18 on pr-trains 018fa4ad,
/// 784c4178 and 16966a29: every task step carried
/// `authority_role = platform-admin` and no assignee at birth, so the
/// dispatcher's executes-lane (`BOSS_DISPATCH_EXECUTOR_ID`, roles
/// `platform-admin`) nominated each one to `claude@algedonic.dev`, and
/// the conductor then completed it. Seven steps per train in the
/// agent's MY WORK, every ~45 minutes, none of them the agent's. A bare
/// `authority_role` with no assignee is not the fix either: the
/// assignment query's role arm lists such a step for EVERY holder of
/// the role, so it would have reached David's queue as claimable work
/// instead. The step is the conductor's; the record should say so.
#[test]
fn the_conductors_steps_declare_the_conductor_as_their_audience() {
    let train = bundled("pr-train");
    for slug in CONDUCTORS_STEPS {
        let step = train
            .steps
            .iter()
            .find(|s| s.title == slug)
            .unwrap_or_else(|| panic!("the conductor completes a `{slug}` step; it is gone"));
        assert_eq!(step.kind, "task", "`{slug}` is a task step");
        assert_eq!(
            step.audience,
            Some(Audience::Individual(CONDUCTOR.into())),
            "`{slug}` is the conductor's step and declares so as its audience"
        );
        assert_eq!(
            step.authority_role, None,
            "`{slug}` declares who it is for ONCE — the legacy `authority_role` would put it \
             in every platform-admin holder's role queue"
        );
    }
}

/// The `cancelled` terminal is materialised as an ABORT, and that is
/// the only reason the conductor's cancel can complete it.
///
/// WHY (backlog 5186c5e1). Its `ready_when` waits on the `empty`
/// marker, which a train that boarded cars never carries — and
/// `cancel_train` (boss-cli train/conductor.rs) completes it anyway,
/// without the marker, after it has closed the PR and released the
/// cars. The step API opens a Pending step by hand only where its own
/// predicate holds (backlog 570e72bd), EXCEPT a terminal whose
/// materialised `outcome_kind` is `aborted`, which completes from any
/// open state. So this row, and nothing else, is what lets a cancel
/// finish: a version without it answers the cancel 409 and the train
/// is left half-cancelled. (The conductor now also refuses such a
/// train before its first write; this pin keeps the protocol from
/// becoming one.)
#[test]
fn the_cancelled_terminal_is_an_abort() {
    let train = bundled("pr-train");
    let spec = train
        .steps
        .iter()
        .find(|s| s.title == "cancelled")
        .expect("pr-train carries a `cancelled` terminal");
    assert_eq!(
        spec.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("cancelled"),
        "`cancelled` is a terminal with outcome `cancelled`"
    );
    assert!(
        spec.ready_when.contains("job.metadata.empty"),
        "precondition of this pin: the machine's own road waits on the `empty` marker, which \
         a cancelled train that boarded cars does not carry — got {:?}",
        spec.ready_when
    );
    let steps = materialize_steps(
        &train,
        &Subject::new("custom", "train/20260925-0222"),
        JobId::new(),
        &serde_json::Value::Object(Default::default()),
        StepId::new,
    );
    let cancelled = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("cancelled"))
        .expect("`cancelled` materialised");
    assert_eq!(
        cancelled.metadata.get("outcome_kind"),
        Some(&serde_json::json!("aborted")),
        "the materialised row the step API reads says `aborted`, so the conductor's cancel \
         completes it from any open state"
    );
}

/// Materialised, the conductor's steps are born the conductor's: the
/// dispatcher's assignee-already-set guard passes them over, and
/// neither arm of the assignment query lists them for a person or an
/// agent — no `assignee_id` a login matches, no `authority_role`.
#[test]
fn a_materialised_train_is_born_the_conductors() {
    let train = bundled("pr-train");
    let subject = Subject::new("custom", "train/20260918-1141");
    let steps = materialize_steps(
        &train,
        &subject,
        JobId::new(),
        &serde_json::Value::Object(Default::default()),
        StepId::new,
    );
    for slug in CONDUCTORS_STEPS {
        let step = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("`{slug}` materialised"));
        assert_eq!(
            step.assignee_id.as_deref(),
            Some(CONDUCTOR),
            "`{slug}` is born assigned to the conductor"
        );
        assert!(
            step.metadata.get("authority_role").is_none(),
            "`{slug}` carries no authority_role for the role arm to list"
        );
    }
    // The train's markers are untouched: the trigger and both outcomes
    // are nobody's, exactly as before.
    for slug in ["scheduled", "arrived", "cancelled"] {
        let step = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("`{slug}` materialised"));
        assert_eq!(step.assignee_id, None, "`{slug}` is a marker, nobody's");
    }
}

/// A MERGED TRAIN WHOSE MERGE MAIN LOST ENDS ON EVIDENCE (backlog
/// f9256445, design d812f1b7 D1). Train 2026-09-25 20:04 merged as
/// c85941b4; by 20:11:14Z forge main was back at 777a5888 and no later
/// cluster commit could ever descend from the merge, so `converged`
/// waited for ever: `arrived` needs `converged`, and `cancelled` (closed
/// UNMERGED, and waiting on the `empty` marker) would record something
/// false about a PR the forge reports merged.
///
/// `merge-lost` is the third ending: an ABORT (the delivery did not
/// complete, and an abort completes from any open state — so its four
/// REQUIRED fields are its gate: nobody reaches it without the reading),
/// ready only after `merged` and on the conductor's own marker.
#[test]
fn a_merge_main_lost_is_an_abort_that_carries_its_reading() {
    let train = bundled("pr-train");
    let spec = train
        .steps
        .iter()
        .find(|s| s.title == "merge-lost")
        .expect("pr-train carries a `merge-lost` terminal");
    assert_eq!(spec.kind, "outcome");
    assert_eq!(
        spec.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("merge-lost")
    );
    assert!(
        spec.ready_when.contains("steps.merged.done")
            && spec
                .ready_when
                .contains("job.metadata.merge_lost = \"true\""),
        "only a merged train can lose its merge, and only the conductor's marker says so: {:?}",
        spec.ready_when
    );
    let required: Vec<&str> = spec
        .fields
        .iter()
        .filter(|f| f.required)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        required,
        ["merge_ref", "main_at_read", "read_at", "evidence"],
        "the four fields the arm reads are its gate"
    );
    let steps = materialize_steps(
        &train,
        &Subject::new("custom", "train/20260925-2004"),
        JobId::new(),
        &serde_json::Value::Object(Default::default()),
        StepId::new,
    );
    let lost = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("merge-lost"))
        .expect("`merge-lost` materialised");
    assert_eq!(
        lost.metadata.get("outcome_kind"),
        Some(&serde_json::json!("aborted"))
    );
    assert_eq!(lost.assignee_id, None, "a terminal is a marker, nobody's");
}
