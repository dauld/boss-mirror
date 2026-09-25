//! Postgres half of the whole-row judgement (backlog 6ec22d71).
//!
//! The version a judged step write names is the row's `xmin` as the read
//! saw it: the transaction id of the last write to that row, by ANY
//! writer — the merge door, a claim, a sign-off, a re-pin, the rebuild,
//! a hand-run UPDATE. So any column any writer moved refuses the write,
//! and no value is compared, which is what let a metadata number beyond
//! f64 precision wedge the metadata compare car 88123ae0 shipped: the
//! read's copy of it could never equal the row. A row that went terminal
//! takes a write only while the write would move nothing the row froze,
//! so no outbox event records what the row refused.
//! `a_stale_step_write_is_judged_on_the_whole_row.rs` pins the same
//! rules on the in-memory adapter.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
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

fn updated(step: &Step) -> boss_core::event::Event {
    stamp().event(
        boss_jobs::events::STEP_UPDATED,
        boss_jobs::events::step_state_payload(step),
    )
}

async fn ready_step(repo: &boss_jobs::PgJobs, id: &str) -> Step {
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

async fn complete(repo: &boss_jobs::PgJobs, step: &Step) {
    let mut done = repo.get_step(&step.id).await.unwrap().unwrap();
    done.status = StepStatus::Completed;
    repo.update_step(&done).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_write_over_a_claim_is_refused_and_the_claim_stands() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = ready_step(&repo, "00000000-0000-0000-0000-000000006ed1").await;

    // The assignment PUT reads …
    let (read, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    // … a claim commits — status and holder, no metadata …
    repo.claim_step_at(&step.id, "agent-claimer", chrono::Utc::now(), &[])
        .await
        .unwrap();
    let updates_before = outbox_updates(&db, &step).await;

    // … and the PUT writes the row it computed from its read.
    let mut stale = read.clone();
    stale.assignee_id = Some("agent-other".into());
    let answer = repo
        .update_step_if_unchanged_at(&stale, version, chrono::Utc::now(), &[updated(&stale)])
        .await;

    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "a write read before the claim is refused by name, got {answer:?}"
    );
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Active,
        "the claim's status stands"
    );
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claimer"));
    assert_eq!(outbox_updates(&db, &step).await, updates_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_write_over_a_row_that_went_terminal_is_refused_and_records_nothing() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = ready_step(&repo, "00000000-0000-0000-0000-000000006ed2").await;
    let (read, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    complete(&repo, &step).await;
    let updates_before = outbox_updates(&db, &step).await;

    let mut stale = read.clone();
    stale.assignee_id = Some("agent-late".into());
    let answer = repo
        .update_step_if_unchanged_at(&stale, version, chrono::Utc::now(), &[updated(&stale)])
        .await;

    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "got {answer:?}"
    );
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Completed);
    assert_eq!(stored.assignee_id, None);
    assert_eq!(
        outbox_updates(&db, &step).await,
        updates_before,
        "no jobs.step.updated carries the stale values the row refused — \
         rebuild.rs::upsert_step would replay them"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_number_beyond_f64_precision_does_not_wedge_the_step() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = ready_step(&repo, "00000000-0000-0000-0000-000000006ed3").await;
    // Written by hand, beyond what an f64 holds: the copy a reader
    // parses can never compare equal to it.
    sqlx::query(
        "UPDATE steps SET metadata = '{\"authority_role\": \"platform-admin\", \
         \"n\": 123456789012345678901234567890}'::jsonb WHERE id = $1",
    )
    .bind(*step.id.inner().as_uuid())
    .execute(&db.pool)
    .await
    .unwrap();

    let (read, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    let mut next = read.clone();
    next.assignee_id = Some("agent-claude".into());
    repo.update_step_if_unchanged_at(&next, version, chrono::Utc::now(), &[])
        .await
        .expect("a write over the row it read lands, whatever its metadata holds");
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some("agent-claude"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_row_takes_a_resend_that_changes_nothing_and_refuses_one_that_would() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = ready_step(&repo, "00000000-0000-0000-0000-000000006ed4").await;
    complete(&repo, &step).await;

    let (done, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    repo.update_step_if_unchanged_at(&done, version, chrono::Utc::now(), &[updated(&done)])
        .await
        .expect("an idempotent re-send over the row it read lands");

    let (done, version) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    let updates_before = outbox_updates(&db, &step).await;
    let mut retitled = done.clone();
    retitled.title = "Not what was completed".into();
    let answer = repo
        .update_step_if_unchanged_at(
            &retitled,
            version,
            chrono::Utc::now(),
            &[updated(&retitled)],
        )
        .await;
    assert!(
        matches!(answer, Err(JobsError::TerminalStep { id, .. }) if id == step.id),
        "got {answer:?}"
    );
    let stored = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.title, "Briefed");
    assert_eq!(outbox_updates(&db, &step).await, updates_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_does_not_move_the_version_and_a_list_read_answers_the_same_one() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step = ready_step(&repo, "00000000-0000-0000-0000-000000006ed5").await;
    let (_, first) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    let (_, again) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    assert_eq!(first, again);
    let listed = repo.list_steps_versioned(&step.job_id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1, first);

    repo.append_sign_off(
        &step.id,
        &boss_core::job::SignOffStamp {
            authority_id: "emp-1".into(),
            role: "qa".into(),
            stamped_at: chrono::Utc::now(),
            shape_hash: "h".into(),
            assurance: Default::default(),
            presence_nonce: None,
            voided_at: None,
            voided_by_event: None,
        },
        chrono::Utc::now(),
        &[],
    )
    .await
    .unwrap();
    let (_, signed) = repo.get_step_versioned(&step.id).await.unwrap().unwrap();
    assert_ne!(
        signed, first,
        "a sign-off — a column the step PUT never writes — moves it"
    );
}
