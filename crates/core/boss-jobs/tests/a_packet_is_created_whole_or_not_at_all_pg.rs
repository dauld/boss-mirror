//! A packet is created whole or not at all — the Postgres half
//! (backlog f2ba226e).
//!
//! Measured 2026-09-16 02:32Z on pr-train 06e5610f: the job row
//! committed at :14.872, steps 1–9 each in their own transaction
//! over the next thirty seconds (the database was slow that night —
//! five sqlx "slow statement" warnings in one second at 03:01), and
//! the tenth step — `cancelled`, the terminal — was never written.
//! The client's request had timed out, axum dropped the handler
//! mid-loop, and nothing logged: not the per-step warn (0 in three
//! hours of log), not an error. The train held the track for ninety
//! minutes with no terminal to cancel it with.
//!
//! The port now takes the steps WITH the job —
//! `create_job_with_steps_at` — and the adapter commits the job row,
//! every step row and every STEP_CREATED outbox event once. These
//! tests pin the two halves of "or not at all" against the real
//! adapter: a step insert that FAILS leaves no job, no steps and no
//! events; and a request DROPPED mid-write (the 06e5610f shape: the
//! future cancelled while a statement waits on the database) leaves
//! the same nothing.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::events::{JOB_CREATED, STEP_CREATED, step_state_payload};
use boss_jobs::port::JobsError;
use boss_jobs::{JobsRepository, PgJobs};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/ux/jobs"),
        title: "whole or nothing".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A stamp at Postgres precision (microseconds), so a row's
/// `created_at` reads back equal to the instant that was bound.
fn stamp() -> EventStamp {
    use chrono::SubsecRound;
    let s = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let at = s.timestamp.trunc_subsecs(6);
    s.with_timestamp(at)
}

/// `n` steps of the job, the first one ready — the materialized shape.
fn steps_of(j: &Job, n: usize) -> Vec<Step> {
    (0..n)
        .map(|i| {
            let mut s = Step::new(j.id, "task", format!("step {i}"), i as i32);
            s.spec_slug = Some(format!("s{i}"));
            if i == 0 {
                s.status = StepStatus::Ready;
            }
            s
        })
        .collect()
}

fn events_for(
    stamp: &EventStamp,
    j: &Job,
    steps: &[Step],
) -> (Vec<boss_core::event::Event>, Vec<boss_core::event::Event>) {
    let job_events = vec![stamp.event(JOB_CREATED, serde_json::to_value(j).unwrap())];
    let step_events = steps
        .iter()
        .map(|s| stamp.event(STEP_CREATED, step_state_payload(s)))
        .collect();
    (job_events, step_events)
}

async fn counts(pool: &sqlx::PgPool, job: &JobId) -> (i64, i64, i64) {
    let jid = *job.inner().as_uuid();
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE id = $1")
        .bind(jid)
        .fetch_one(pool)
        .await
        .unwrap();
    let steps: i64 = sqlx::query_scalar("SELECT count(*) FROM steps WHERE job_id = $1")
        .bind(jid)
        .fetch_one(pool)
        .await
        .unwrap();
    // Every event about this job names it: JOB_CREATED carries `id`,
    // STEP_CREATED carries `job_id`.
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox \
          WHERE payload->>'id' = $1 OR payload->>'job_id' = $1",
    )
    .bind(jid.to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    (jobs, steps, events)
}

/// The whole graph lands in one commit, and the log holds one
/// STEP_CREATED per step — what the rebuilder reproduces the rows
/// from. Every row shares the one admission instant.
#[tokio::test(flavor = "multi_thread")]
async fn the_job_and_every_step_and_every_event_commit_together() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000a001");
    let steps = steps_of(&j, 10);
    let stamp = stamp();
    let (job_events, step_events) = events_for(&stamp, &j, &steps);

    repo.create_job_with_steps_at(&j, &steps, stamp.timestamp, &job_events, &step_events)
        .await
        .expect("a whole packet is admitted");

    assert_eq!(counts(&db.pool, &j.id).await, (1, 10, 11));
    let created: Vec<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT created_at FROM steps WHERE job_id = $1 \
          UNION ALL SELECT created_at FROM jobs WHERE id = $1",
    )
    .bind(*j.id.inner().as_uuid())
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(created.len(), 11);
    assert!(
        created.iter().all(|t| *t == stamp.timestamp),
        "one transaction, one instant: every row's created_at is the stamp's"
    );
    let listed = repo.list_steps(&j.id).await.unwrap();
    assert_eq!(listed.len(), 10);
    assert_eq!(listed[0].status, StepStatus::Ready, "born ready survives");
}

/// The 06e5610f shape with the failure made loud: the Nth step
/// cannot be written (here: its `job_id` names a job that does not
/// exist, a foreign-key refusal). Before, the job row and steps
/// 1..N-1 were already committed and the handler answered 201; now
/// the call errs and NOTHING exists — no job, no steps, no events.
#[tokio::test(flavor = "multi_thread")]
async fn a_step_that_cannot_be_written_leaves_no_job_no_steps_no_events() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000a002");
    let mut steps = steps_of(&j, 10);
    steps[9].job_id =
        JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-00000000dead").unwrap());
    let stamp = stamp();
    let (job_events, step_events) = events_for(&stamp, &j, &steps);

    let err = repo
        .create_job_with_steps_at(&j, &steps, stamp.timestamp, &job_events, &step_events)
        .await
        .expect_err("the tenth step's insert is refused");
    assert!(matches!(err, JobsError::Storage(_)), "{err}");

    assert_eq!(
        counts(&db.pool, &j.id).await,
        (0, 0, 0),
        "a refused step must take the job, the other steps and every event with it"
    );
}

/// The 06e5610f shape itself: the request is DROPPED while a step
/// insert waits on the database. Another connection holds an
/// uncommitted row under the fifth step's id, so that INSERT blocks
/// on the unique index; a timeout drops the create future mid-
/// transaction, exactly as axum drops a handler whose client went
/// away. A dropped transaction never commits, so the job, the four
/// steps already written and their events all vanish — the residue
/// class this packet was filed on cannot occur.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_dropped_mid_write_leaves_nothing() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    // A committed job for the blocker's step to reference (FK).
    let other = job("00000000-0000-0000-0000-00000000a0ff");
    repo.create_job(&other).await.unwrap();

    let j = job("00000000-0000-0000-0000-00000000a003");
    let steps = steps_of(&j, 10);
    let stamp = stamp();
    let (job_events, step_events) = events_for(&stamp, &j, &steps);

    // The blocker: an uncommitted row under step 5's id.
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO steps (id, job_id, title) VALUES ($1, $2, 'blocker')")
        .bind(*steps[4].id.inner().as_uuid())
        .bind(*other.id.inner().as_uuid())
        .execute(&mut *blocker)
        .await
        .unwrap();

    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        repo.create_job_with_steps_at(&j, &steps, stamp.timestamp, &job_events, &step_events),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the create must still be waiting on step 5 when the timeout drops it"
    );

    // Release the lock the way the client's departure never would
    // have: the blocked statement is free to proceed — on a
    // connection whose transaction nothing will ever commit.
    blocker.rollback().await.unwrap();

    assert_eq!(
        counts(&db.pool, &j.id).await,
        (0, 0, 0),
        "a dropped request leaves no job, no partial graph and no events"
    );
    // The blocker's own row went with its rollback; the other job is
    // untouched.
    assert_eq!(counts(&db.pool, &other.id).await, (1, 0, 0));
}

/// The caller contract is one STEP_CREATED per step, index-aligned;
/// a mismatch is refused BEFORE anything is written rather than
/// zipped short (which would silently record fewer events than rows
/// — the rebuilder would then reproduce fewer steps than the live
/// projection holds).
#[tokio::test(flavor = "multi_thread")]
async fn a_step_without_its_event_is_refused_before_any_write() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000a004");
    let steps = steps_of(&j, 3);
    let stamp = stamp();
    let (job_events, mut step_events) = events_for(&stamp, &j, &steps);
    step_events.pop();

    let err = repo
        .create_job_with_steps_at(&j, &steps, stamp.timestamp, &job_events, &step_events)
        .await
        .expect_err("3 steps, 2 events");
    assert!(err.to_string().contains("3 step"), "{err}");
    assert_eq!(counts(&db.pool, &j.id).await, (0, 0, 0));
}
