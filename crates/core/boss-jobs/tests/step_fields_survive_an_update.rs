//! Do a step's authored completion-contract `fields` survive
//! `update_step_at`?
//!
//! Defect a07cfddd says no: the HTTP handler merges the body over the
//! current row correctly, so `step.fields` holds the new value by the
//! time it reaches the adapter — and the Pg adapter's UPDATE simply
//! does not name the `fields` column. The caller gets 204 and the
//! write is gone. That makes a step's completion contract write-once,
//! fixed at materialization, which nothing declares and the API does
//! not refuse.
//!
//! The in-memory adapter stores the whole Step and never had the bug,
//! which is exactly why the pin runs against the real SQL (same split
//! as job_metadata_patch_pg.rs).

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepField, StepStatus, Subject};
use boss_jobs::JobsRepository;
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

fn job(id: JobId) -> Job {
    Job {
        id,
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Repair".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated: false,
    }
}

fn field(name: &str, required: bool) -> StepField {
    StepField {
        name: name.into(),
        field_type: "string".into(),
        required,
        filled_by: boss_core::job::FilledBy::Executor,
    }
}

async fn seeded(status: StepStatus, fields: Vec<StepField>) -> (TestDb, boss_jobs::PgJobs, Step) {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let job_id = JobId::from_uuid(Uuid::new_v4());
    repo.create_job_at(&job(job_id), Utc::now(), &[])
        .await
        .expect("create job");
    let mut step = Step::new(job_id, "task", "Do the work", 0);
    step.spec_slug = Some("work".into());
    step.status = status;
    step.fields = fields;
    repo.add_step_at(&step, Utc::now(), &[])
        .await
        .expect("add step");
    (db, repo, step)
}

/// The reported sequence: a live step, an update carrying a new
/// authored contract, a 204-shaped success — the fields must read
/// back, not vanish.
#[tokio::test(flavor = "multi_thread")]
async fn authored_fields_survive_an_update() {
    let (_db, repo, step) = seeded(StepStatus::Ready, vec![]).await;

    let mut updated = repo
        .get_step(&step.id)
        .await
        .expect("read")
        .expect("step exists");
    updated.fields = vec![field("summary", true), field("excludes", false)];
    repo.update_step_at(&updated, Utc::now(), &[])
        .await
        .expect("update accepts the write");

    let after = repo
        .get_step(&step.id)
        .await
        .expect("re-read")
        .expect("step still exists");
    assert_eq!(
        after.fields, updated.fields,
        "an accepted update must not silently drop the authored fields"
    );
}

/// Terminal parity with `metadata`: the row-level freeze that keeps a
/// completed step's metadata immutable must hold for its completion
/// contract too — a racing stale write cannot rewrite history.
#[tokio::test(flavor = "multi_thread")]
async fn a_completed_steps_fields_stay_frozen() {
    let frozen = vec![field("evidence", true)];
    let (_db, repo, step) = seeded(StepStatus::Completed, frozen.clone()).await;

    let mut stale = repo
        .get_step(&step.id)
        .await
        .expect("read")
        .expect("step exists");
    stale.fields = vec![field("rewritten", false)];
    repo.update_step_at(&stale, Utc::now(), &[])
        .await
        .expect("adapter accepts and ignores, same as metadata");

    let after = repo
        .get_step(&step.id)
        .await
        .expect("re-read")
        .expect("step still exists");
    assert_eq!(
        after.fields, frozen,
        "a completed step's contract is part of the frozen row"
    );
}
