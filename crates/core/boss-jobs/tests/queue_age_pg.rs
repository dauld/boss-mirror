//! Postgres contract for `queue_age` — the storage half of the
//! queue-age lens (packet 2a0b034e).
//!
//! Two implementations of one rule (the in-memory HTTP test carries
//! the other), so what is pinned here is the SQL's arithmetic and the
//! projection's stamping:
//!
//! 1. **`became_ready_at` is written once, at the write that first
//!    lands the step in `ready`**, whether that write is the INSERT
//!    (born ready at materialization) or an UPDATE (pending → ready
//!    promotion) — and no later write moves it. `updated_at` keeps
//!    being bumped by every write; that divergence is the whole point
//!    of the dedicated column (2a77e5fc: annotating bumped every age).
//! 2. **The fallback is `updated_at`, labelled inexact** — rows that
//!    predate the column (simulated here by nulling the stamp) still
//!    answer, as a lower bound.
//! 3. **Scope reaches the SQL**: an `OwnerIs` caller's lens holds only
//!    their packets — same rule `list_jobs` enforces.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::port::{JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn packet(id: &str, owner: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: title.into(),
        owner_id: owner.into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 29).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

fn step(id: &str, job_id: &str, status: StepStatus, title: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(job_id).unwrap()),
        kind: "generic".into(),
        title: title.into(),
        spec_slug: Some(title.to_lowercase().replace(' ', "-")),
        assignee_id: None,
        status,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

const JOB_A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const JOB_B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const STEP_PROMOTED: &str = "11111111-0000-4000-8000-000000000001";
const STEP_BORN_READY: &str = "22222222-0000-4000-8000-000000000002";
const STEP_THEIRS: &str = "33333333-0000-4000-8000-000000000003";

#[tokio::test(flavor = "multi_thread")]
async fn the_stamp_is_written_once_and_survives_annotation_and_claim() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    repo.create_job_at(
        &packet(JOB_A, "emp-david", "packet a"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    // Born pending; promoted; annotated; claimed. Three later writes,
    // none of which may move the stamp.
    repo.add_step_at(
        &step(STEP_PROMOTED, JOB_A, StepStatus::Pending, "triage"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    let promoted = step(STEP_PROMOTED, JOB_A, StepStatus::Ready, "triage");
    repo.update_step_at(&promoted, t("2026-08-30T10:00:00Z"), &[])
        .await
        .unwrap();
    let mut annotated = promoted.clone();
    annotated.metadata = serde_json::json!({"note": "bump"});
    repo.update_step_at(&annotated, t("2026-09-01T18:00:00Z"), &[])
        .await
        .unwrap();
    repo.claim_step_at(
        &StepId::from_uuid(Uuid::parse_str(STEP_PROMOTED).unwrap()),
        "claude@algedonic.dev",
        t("2026-09-02T08:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    let rows = repo.queue_age(&JobScope::All).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.since,
        t("2026-08-30T10:00:00Z"),
        "the promotion instant"
    );
    assert!(row.exact);
    assert_eq!(row.status, StepStatus::Active, "claimed, still outstanding");
    assert_eq!(row.assignee_id.as_deref(), Some("claude@algedonic.dev"));
    assert_eq!(row.job_kind, "backlog-item");
    assert_eq!(row.job_title, "packet a");

    // Meanwhile updated_at DID move — the divergence the column exists
    // for. Without the stamp this obligation would look 3 days younger.
    let updated_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT updated_at FROM steps WHERE id = $1")
            .bind(Uuid::parse_str(STEP_PROMOTED).unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(updated_at, t("2026-09-02T08:00:00Z"));
}

#[tokio::test(flavor = "multi_thread")]
async fn born_ready_stamps_at_insert_and_legacy_rows_fall_back_labelled() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    repo.create_job_at(
        &packet(JOB_A, "emp-david", "packet a"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    repo.add_step_at(
        &step(STEP_BORN_READY, JOB_A, StepStatus::Ready, "filed"),
        t("2026-08-29T09:30:00Z"),
        &[],
    )
    .await
    .unwrap();

    let rows = repo.queue_age(&JobScope::All).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].since, t("2026-08-29T09:30:00Z"));
    assert!(rows[0].exact, "materialized ready IS the ready flip");

    // A row from before the column existed: stamp NULL, only
    // updated_at to go on. The lens answers — lower bound, labelled.
    sqlx::query("UPDATE steps SET became_ready_at = NULL, updated_at = $2 WHERE id = $1")
        .bind(Uuid::parse_str(STEP_BORN_READY).unwrap())
        .bind(t("2026-08-31T00:00:00Z"))
        .execute(&db.pool)
        .await
        .unwrap();
    let rows = repo.queue_age(&JobScope::All).await.unwrap();
    assert_eq!(rows[0].since, t("2026-08-31T00:00:00Z"));
    assert!(!rows[0].exact, "a lower bound must say it is one");
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_reaches_the_sql() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    repo.create_job_at(
        &packet(JOB_A, "emp-david", "mine"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    repo.create_job_at(
        &packet(JOB_B, "emp-other", "theirs"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    repo.add_step_at(
        &step(STEP_BORN_READY, JOB_A, StepStatus::Ready, "mine"),
        t("2026-08-30T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    repo.add_step_at(
        &step(STEP_THEIRS, JOB_B, StepStatus::Ready, "theirs"),
        t("2026-08-30T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    let all = repo.queue_age(&JobScope::All).await.unwrap();
    assert_eq!(all.len(), 2);

    let mine = repo
        .queue_age(&JobScope::OwnerIs("emp-david".into()))
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].job_title, "mine");

    let none = repo.queue_age(&JobScope::None).await.unwrap();
    assert!(none.is_empty());
}
