//! The launch calendar is a read of a STEP PROPERTY, never of a Job
//! kind, and both adapters answer it the same way.
//!
//! WHY THIS FILE EXISTS (backlog 649b3303, found by page-audit
//! 0ceeffa6 on /ux/calendar). `list_launch_calendar` filtered on
//! `j.kind = 'marketing-motion'` in `PgJobs` and on
//! `job.kind != "marketing-motion"` in `InMemoryJobs` — a
//! used-device-shop Workflow kind spelled as a literal in Tier 1,
//! against CLAUDE.md §9 (registries over hardcoded paths) and §10 (no
//! tenant assumptions in core). The step side already followed the
//! rule: the launch step is whichever step carries `launch_date`. The
//! kind filter is gone; a Job is on the calendar because one of its
//! steps carries `launch_date` — the field the StepType registry
//! declares on `marketing-launch`, and which that Workflow's
//! `metadata_defaults` materialise as `""` so an undated launch still
//! lists under "unscheduled".
//!
//! The same audit found the two adapters DISAGREE on a packet with two
//! launch steps: Postgres joined one row per launch step, while the
//! in-memory fold kept whichever step its HashMap happened to visit
//! last — one row, chosen by iteration order. Both now answer one row
//! per launch step inside the window. One contract below is run against
//! each adapter, so the two cannot drift apart again without a red.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::JobsRepository;
use chrono::NaiveDate;
use uuid::Uuid;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn job(n: u128, kind: &str, title: &str, status: JobStatus) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::from_u128(0x0ca1_0000 + n)),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("campaign", "fall-drop"),
        title: title.into(),
        owner_id: "emp-owner".into(),
        status,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 1),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn step(job: &Job, sort_order: i32, kind: &str, metadata: serde_json::Value) -> Step {
    Step {
        id: StepId::new(),
        job_id: job.id,
        kind: kind.into(),
        title: format!("step {sort_order}"),
        spec_slug: None,
        assignee_id: None,
        status: StepStatus::Ready,
        sort_order,
        blocked_by: Vec::new(),
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata,
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

fn launch(date: &str, channel: &str) -> serde_json::Value {
    serde_json::json!({ "launch_date": date, "launch_channel": channel })
}

async fn seed<R: JobsRepository>(repo: &R, job: Job, steps: Vec<Step>) {
    repo.create_job(&job).await.expect("create job");
    for s in steps {
        repo.add_step(&s).await.expect("add step");
    }
}

/// Seed the fixture and assert the calendar through the port.
async fn the_calendar_contract<R: JobsRepository>(repo: &R) {
    // A Job of a kind no core code names, carrying a dated launch step.
    let a = job(1, "product-drop", "A dated drop", JobStatus::Open);
    let a_steps = vec![
        step(&a, 0, "task", serde_json::json!({})),
        step(&a, 1, "marketing-launch", launch("2026-10-05", "email")),
    ];
    seed(repo, a, a_steps).await;

    // An open Job with no step carrying launch_date is not a launch —
    // whatever its kind. On main, a `marketing-motion` in this shape
    // listed as "unscheduled"; without the kind filter the join must be
    // an inner one, or every open packet in the system would.
    let b = job(2, "marketing-motion", "B no launch step", JobStatus::Open);
    let b_steps = vec![step(&b, 0, "task", serde_json::json!({}))];
    seed(repo, b, b_steps).await;

    // An undated launch step (the Workflow's metadata_defaults write
    // `launch_date = ""`) lists with no date: "unscheduled". An empty
    // channel is no channel — Postgres's NULLIF said so, and the
    // in-memory adapter answered `Some("")` until this file.
    let c = job(3, "webinar", "C undated", JobStatus::Open);
    let c_steps = vec![step(&c, 0, "marketing-launch", launch("", ""))];
    seed(repo, c, c_steps).await;

    // Closed packets and out-of-window dates stay off.
    let d = job(4, "product-drop", "D closed", JobStatus::Closed);
    let d_steps = vec![step(
        &d,
        0,
        "marketing-launch",
        launch("2026-10-07", "email"),
    )];
    seed(repo, d, d_steps).await;
    let e = job(5, "product-drop", "E out of window", JobStatus::Open);
    let e_steps = vec![step(
        &e,
        0,
        "marketing-launch",
        launch("2026-12-01", "email"),
    )];
    seed(repo, e, e_steps).await;

    // Two launch steps on one packet: one row per launch step inside the
    // window — the Postgres join's answer, now the in-memory one too.
    let f = job(6, "marketing-motion", "F two launches", JobStatus::Open);
    let f_steps = vec![
        step(&f, 0, "marketing-launch", launch("2026-10-01", "webinar")),
        step(
            &f,
            1,
            "marketing-launch",
            launch("2026-10-20", "paid-social"),
        ),
        step(&f, 2, "marketing-launch", launch("2027-01-15", "event")),
    ];
    seed(repo, f, f_steps).await;

    let rows = repo
        .list_launch_calendar(day(2026, 10, 1), day(2026, 10, 31))
        .await
        .expect("calendar read");
    let got: Vec<(&str, Option<NaiveDate>, Option<&str>)> = rows
        .iter()
        .map(|r| (r.title.as_str(), r.launch_date, r.launch_channel.as_deref()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("C undated", None, None),
            ("F two launches", Some(day(2026, 10, 1)), Some("webinar")),
            ("A dated drop", Some(day(2026, 10, 5)), Some("email")),
            (
                "F two launches",
                Some(day(2026, 10, 20)),
                Some("paid-social")
            ),
        ],
        "the calendar lists every open packet's launch steps in the window, whatever the packet's kind"
    );
    let a_row = rows
        .iter()
        .find(|r| r.title == "A dated drop")
        .expect("A listed");
    assert_eq!(a_row.current_tier, Some(0), "min non-terminal sort_order");
    assert_eq!(a_row.status, JobStatus::Open);
}

#[tokio::test]
async fn the_in_memory_calendar_reads_the_launch_step_not_the_kind() {
    let repo = boss_jobs::InMemoryJobs::new();
    the_calendar_contract(&repo).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_postgres_calendar_reads_the_launch_step_not_the_kind() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    the_calendar_contract(&repo).await;
}
