//! `triage` and `measure` on a backlog-item are an analyst's steps,
//! and their procedures carry the three acts that make a route worth
//! trusting (backlog cf6be734).
//!
//! Measured 2026-09-25 05:20Z: 295 open backlog-items, 95 of them at
//! `triage`, the oldest untriaged six days; `boss dispatch eed361e0`
//! refused with "step `triage` of backlog-item declares no agent
//! block", and `measure` refused the same way on 70da1212 and 8db1d6a6
//! the day before. Both steps were already agent-claude's work, done by
//! hand, one at a time, while builder slots and gate bays sat on the
//! drain. The block is the whole difference between that and a
//! dispatched run — the roster in `platform_bundle_agent_blocks.rs`
//! holds its settings; this file holds what the run is TOLD.
//!
//! WHAT THE PROCEDURE MUST SAY, and why each is pinned:
//! - origin/main — a claim is measured against the tree as it stands,
//!   not the tree it was filed against; five "open" packets closed on
//!   2026-09-01 had fixes already on main (CLAUDE.md §Engineering
//!   Session Startup, step 4).
//! - the open-items read — dedup is a READ of every open backlog-item,
//!   not a recollection, and a duplicate names the packet it duplicates.
//! - the verb with its `-file` twin — evidence reaches the step's own
//!   `evidence` field through the door that checks the route, and
//!   through a file, because a backtick in argv is command substitution
//!   (2376b89e).
//!
//! AND THE 95 ALREADY OPEN. They are pinned to the version they were
//! admitted under. `boss dispatch` already reads the ACTIVE row's block
//! for a step whose packet predates it (`settings_for`, backlog
//! dacee8cc), so publishing makes them dispatchable; but the procedure
//! they are briefed with is the one copied at admission. `boss job
//! convert` is the door that re-projects it, and the last test here
//! holds that the move from a version without these blocks is one the
//! door takes automatically and that it carries the new text.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::protocol_conversion::{Convertibility, convertibility};
use boss_jobs::registry::{StepSpec, WorkflowSpec, materialize_steps_at, platform_bundle_path};
use boss_jobs::repin::plan;
use boss_jobs::seed_loader::load_workflows;
use serde_json::json;

fn backlog_item() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "backlog-item")
        .expect("backlog-item ships in the platform bundle")
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("backlog-item has a `{title}` step"))
}

fn procedure(title: &str) -> String {
    step(&backlog_item(), title).metadata_defaults["procedure"]
        .as_str()
        .unwrap_or_else(|| panic!("`{title}` carries a procedure"))
        .to_string()
}

fn assert_names(title: &str, anchors: &[&str]) {
    let text = procedure(title);
    for anchor in anchors {
        assert!(
            text.contains(anchor),
            "the `{title}` procedure must name `{anchor}`: {text}"
        );
    }
}

/// Both steps are an analyst's: they ship no car, so the rules a
/// builder is briefed with (worktree, gate, push) would be prose about
/// work these steps do not do — the call `draft-design` made.
#[test]
fn triage_and_measure_declare_an_analyst() {
    let wf = backlog_item();
    for title in ["triage", "measure"] {
        let agent = step(&wf, title)
            .agent
            .as_ref()
            .unwrap_or_else(|| panic!("`{title}` declares an agent block"));
        assert_eq!(agent.profile, "analyst", "{title}");
    }
}

#[test]
fn the_triage_procedure_measures_on_origin_main_dedups_and_records_through_the_verb() {
    assert_names(
        "triage",
        &[
            "origin/main",
            "kind=backlog-item&status=open",
            "--of",
            "boss triage",
            "--evidence-file",
        ],
    );
}

#[test]
fn the_measure_procedure_measures_on_origin_main_dedups_and_records_through_the_verb() {
    assert_names(
        "measure",
        &[
            "origin/main",
            "kind=backlog-item&status=open",
            "boss step complete",
            "--field-file evidence=",
        ],
    );
}

/// The version before: the same row with neither block and the
/// procedures it carried — triage's absence-verdict rules alone,
/// measure none at all.
fn before(to: &WorkflowSpec) -> WorkflowSpec {
    let mut from = to.clone();
    from.version = to.version - 1;
    for s in from.steps.iter_mut() {
        if s.title == "triage" || s.title == "measure" {
            s.agent = None;
            s.metadata_defaults = if s.title == "triage" {
                json!({ "procedure": "Measure the claim yourself before you route it." })
            } else {
                json!({})
            };
        }
    }
    from
}

fn packet_at_triage(from: &WorkflowSpec) -> (Job, Vec<Step>) {
    let job = Job {
        id: JobId::new(),
        kind: "backlog-item".into(),
        workflow_version: from.version,
        subject: Subject::new("custom", "bosspipeline"),
        title: "an untriaged item".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 24).expect("a date"),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let steps = materialize_steps_at(
        from,
        &job.subject,
        job.id,
        &job.metadata,
        StepId::new,
        Some(job.opened_on),
        None,
    )
    .into_iter()
    .map(|mut s| {
        s.status = match s.spec_slug.as_deref() {
            Some("filed") => StepStatus::Completed,
            Some("triage") => StepStatus::Ready,
            _ => s.status,
        };
        s
    })
    .collect();
    (job, steps)
}

/// An agent block and a procedure are not conversion obstacles — they
/// change how the step is RUN, not what it must collect — so the
/// convert door moves every open item without a review, and the
/// re-pin writes the analyst's settings and the new text onto the
/// triage step the item is waiting at.
#[test]
fn an_item_pinned_before_the_blocks_converts_and_reads_the_new_procedure() {
    let to = backlog_item();
    let from = before(&to);
    assert_eq!(
        convertibility(&from, &to),
        Convertibility::Automatic,
        "adding the blocks must not make the convert door refer every open item"
    );

    let (job, rows) = packet_at_triage(&from);
    let moved = plan(&from, &to, &job, &rows).expect("planned");
    for slug in ["triage", "measure"] {
        let row = moved
            .reprojected
            .iter()
            .find(|r| r.step.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("`{slug}` re-projected: {moved:?}"));
        assert_eq!(row.step.metadata["agent_profile"], "analyst", "{slug}");
        assert_eq!(
            row.step.metadata["procedure"],
            procedure(slug),
            "{slug} reads the new version's procedure after the move"
        );
    }
}
