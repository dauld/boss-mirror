//! Boot-time viability check — what boot owes the operator when an
//! ACTIVE Workflow cannot finish.
//!
//! 2026-08-13: ANY unviable active Workflow made `boss-jobs-api`
//! refuse to start; one registry row took down jobs, docs, the gateway
//! and the human door on the next pod roll. The first fix auto-retired
//! the row when no Jobs were pinned to it, and refused to start when
//! open Jobs were.
//!
//! 2026-09-07: that fix took the system of record down again. Boot
//! found `incident-post-mortem` v1 unviable with one open Job pinned,
//! refused to start, and the pod crash-looped; it found
//! `publish-to-github` v5 unviable with none pinned and retired a live
//! protocol — a persisted write nobody asked for.
//!
//! The contract now: boot CHECKS and LOGS. It never refuses to start
//! over a data condition and it never writes. Quarantine is a
//! deliberate act (retire through the registry), not a boot
//! side-effect.

use std::sync::Arc;

use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_jobs::events::{WORKFLOW_QUARANTINED, WORKFLOW_RETIRED};
use boss_jobs::registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_jobs::workflow_quarantine::check_active_workflows_viable;
use boss_jobs::{InMemoryJobs, JobsRepository};
use chrono::NaiveDate;

fn viable(kind: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        "Viable",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

/// The incident row: steps, no terminal. Arrives by direct SQL or
/// predates the publish gate — Layer 1 makes this rare, not
/// impossible.
fn unviable(kind: &str) -> WorkflowSpec {
    let mut spec = viable(kind);
    spec.steps[1].terminal = None;
    spec
}

async fn seed_open_job(jobs: &InMemoryJobs, kind: &str, version: i32, status: JobStatus) {
    let mut job = Job::new(
        kind,
        Subject::new("custom", "thing-1"),
        "Live work",
        "emp-1",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
    );
    job.workflow_version = version;
    job.status = status;
    jobs.create_job(&job).await.unwrap();
}

/// The row is still active afterwards, and no retirement or
/// quarantine marker was written anywhere — boot touched nothing.
async fn assert_untouched(registry: &InMemoryWorkflows, jobs: &InMemoryJobs, kind: &str) {
    assert_eq!(
        registry
            .get_active(kind)
            .await
            .unwrap_or_else(|e| panic!("`{kind}` must still be active after boot: {e}"))
            .status,
        WorkflowStatus::Active,
        "`{kind}` must still be active after boot"
    );
    assert!(
        registry
            .recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_RETIRED),
        "boot must not retire anything"
    );
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_QUARANTINED),
        "boot must not write a quarantine marker"
    );
}

#[tokio::test]
async fn an_unviable_row_with_no_open_jobs_is_reported_and_left_untouched() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(viable("healthy")).unwrap();
    registry.seed(unviable("publish-to-github")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());

    let report = check_active_workflows_viable(registry.as_ref(), jobs.as_ref())
        .await
        .expect("boot check completes");

    assert_eq!(report.checked, 2);
    assert_eq!(report.unviable.len(), 1);
    assert_eq!(report.unviable[0].kind, "publish-to-github");
    assert_eq!(report.unviable[0].version, 1);
    assert_eq!(report.unviable[0].open_jobs, 0);
    assert!(
        report.unviable[0]
            .problems
            .iter()
            .any(|p| p.reason.contains("no terminal")),
        "the report carries the problems the log printed"
    );

    // 2026-09-07: this is the row boot used to retire. It must not.
    assert_untouched(&registry, &jobs, "publish-to-github").await;
    assert!(
        registry.get_active("healthy").await.is_ok(),
        "a viable row is untouched"
    );
    assert!(
        jobs.recorded_events().is_empty(),
        "boot wrote nothing at all"
    );
}

#[tokio::test]
async fn an_unviable_row_with_open_jobs_pinned_is_reported_and_boot_continues() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(unviable("incident-post-mortem")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    // Two open Jobs pinned to v1 of the offending Workflow, plus a
    // closed one that does not count.
    seed_open_job(&jobs, "incident-post-mortem", 1, JobStatus::Open).await;
    seed_open_job(&jobs, "incident-post-mortem", 1, JobStatus::Blocked).await;
    seed_open_job(&jobs, "incident-post-mortem", 1, JobStatus::Closed).await;

    // 2026-09-07: this is the shape that crash-looped the system of
    // record. The check must return Ok — there is no refusal any more.
    let report = check_active_workflows_viable(registry.as_ref(), jobs.as_ref())
        .await
        .expect("an unviable row with pinned Jobs is a report, not a refusal");

    assert_eq!(report.unviable.len(), 1);
    assert_eq!(report.unviable[0].kind, "incident-post-mortem");
    assert_eq!(
        report.unviable[0].open_jobs, 2,
        "the report names how much live work sits on the row"
    );

    assert_untouched(&registry, &jobs, "incident-post-mortem").await;
}

/// The exact registry boot met on 2026-09-07: one unviable row with
/// live work pinned, one with none, one healthy. Both unviable rows
/// survive boot unchanged, and boot returns Ok.
#[tokio::test]
async fn the_2026_09_07_registry_boots_and_both_unviable_rows_survive() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(viable("healthy")).unwrap();
    registry.seed(unviable("incident-post-mortem")).unwrap();
    registry.seed(unviable("publish-to-github")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    seed_open_job(&jobs, "incident-post-mortem", 1, JobStatus::Open).await;

    let report = check_active_workflows_viable(registry.as_ref(), jobs.as_ref())
        .await
        .expect("boot check completes");

    assert_eq!(report.checked, 3);
    let mut named: Vec<(&str, i64)> = report
        .unviable
        .iter()
        .map(|u| (u.kind.as_str(), u.open_jobs))
        .collect();
    named.sort();
    assert_eq!(
        named,
        vec![("incident-post-mortem", 1), ("publish-to-github", 0)]
    );

    assert_untouched(&registry, &jobs, "incident-post-mortem").await;
    assert_untouched(&registry, &jobs, "publish-to-github").await;
    assert!(registry.get_active("healthy").await.is_ok());
}

#[tokio::test]
async fn a_clean_registry_reports_nothing() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(viable("healthy")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());

    let report = check_active_workflows_viable(registry.as_ref(), jobs.as_ref())
        .await
        .expect("boot check completes");

    assert_eq!(report.checked, 1);
    assert!(report.unviable.is_empty());
    assert!(jobs.recorded_events().is_empty());
}
