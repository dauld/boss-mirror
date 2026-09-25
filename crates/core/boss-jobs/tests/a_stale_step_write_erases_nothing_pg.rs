//! Postgres half of `update_step_if_unchanged_at` (backlog e381689d).
//!
//! The in-memory adapter compares the stored metadata under its lock;
//! this adapter carries the comparison in the UPDATE's own WHERE clause,
//! so no window is left between the check and the write. Two
//! implementations of one rule, so the rule is pinned against the real
//! SQL: a write whose read the row no longer holds is refused as
//! `StepChanged` and writes neither row nor outbox event; a write over
//! an unchanged row lands; a terminal row is not judged; a missing row
//! is still `StepNotFound`. The shape is the one measured on run
//! 6b6fe011 on 2026-09-25: a merge committed between the assignment
//! PUT's read and its whole-row write, and the write erased it.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "agent-run".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "run"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

fn map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("test patches are objects"),
    }
}

async fn briefed(repo: &boss_jobs::PgJobs, id: &str) -> Step {
    let j = job(id);
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Briefed", 1);
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    repo.add_step(&step).await.unwrap();
    step
}

async fn outbox_updates(db: &TestDb, step: &Step) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM event_outbox \
         WHERE kind = 'jobs.step.updated' AND payload->>'step_id' = $1",
    )
    .bind(step.id.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_whose_read_the_row_no_longer_holds_is_refused_and_writes_nothing() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = briefed(&repo, "00000000-0000-0000-0000-00000000e391").await;

    // The assignment PUT reads …
    let read = repo.get_step(&step.id).await.unwrap().unwrap();
    // … the merge door commits …
    repo.merge_step_metadata_at(
        &step.id,
        &map(serde_json::json!({ "prompt_bytes": "36127" })),
        &stamp(),
    )
    .await
    .unwrap();
    let updates_before = outbox_updates(&db, &step).await;

    // … and the PUT writes the row it computed from its read.
    let mut stale = read.clone();
    stale.assignee_id = Some("agent-claude".into());
    let event = stamp().event(
        boss_jobs::events::STEP_UPDATED,
        boss_jobs::events::step_state_payload(&stale),
    );
    let answer = repo
        .update_step_if_unchanged_at(&stale, &read.metadata, chrono::Utc::now(), &[event])
        .await;

    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "a stale write is refused by name, got {answer:?}"
    );
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["prompt_bytes"], "36127",
        "the merged key stands"
    );
    assert_eq!(stored.assignee_id, None, "the refused write wrote no row");
    assert_eq!(
        outbox_updates(&db, &step).await,
        updates_before,
        "and recorded no event"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_over_the_row_it_read_lands() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = briefed(&repo, "00000000-0000-0000-0000-00000000e392").await;
    let read = repo.get_step(&step.id).await.unwrap().unwrap();
    let mut next = read.clone();
    next.assignee_id = Some("agent-claude".into());
    next.metadata["note"] = serde_json::json!("written");
    repo.update_step_if_unchanged_at(&next, &read.metadata, chrono::Utc::now(), &[])
        .await
        .unwrap();
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claude"));
    assert_eq!(stored.metadata["note"], "written");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_row_is_not_judged() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = briefed(&repo, "00000000-0000-0000-0000-00000000e393").await;
    let mut done = repo.get_step(&step.id).await.unwrap().unwrap();
    done.status = StepStatus::Completed;
    done.metadata["prompt_bytes"] = serde_json::json!("36128");
    repo.update_step(&done).await.unwrap();

    let mut stale = step.clone();
    stale.status = StepStatus::Completed;
    repo.update_step_if_unchanged_at(&stale, &step.metadata, chrono::Utc::now(), &[])
        .await
        .unwrap();
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["prompt_bytes"], "36128", "frozen");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_row_is_still_not_found() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000e394");
    repo.create_job(&j).await.unwrap();
    let mut ghost = Step::new(j.id, "task", "Never written", 1);
    ghost.id = StepId::new();
    let answer = repo
        .update_step_if_unchanged_at(&ghost, &serde_json::json!({}), chrono::Utc::now(), &[])
        .await;
    assert!(
        matches!(answer, Err(JobsError::StepNotFound(id)) if id == ghost.id),
        "got {answer:?}"
    );
}
