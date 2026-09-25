//! Both adapters keep a step's assurance requirement and plugin version
//! through an update (backlog 36352452).
//!
//! The review of car 9392b8b5 found `update_step_at` answering one write
//! two ways: the in-memory adapter stored the body's
//! `assurance_required` and `step_plugin_version`, while the Pg UPDATE
//! never names either column, so the same `null` lowered a Presence
//! step in memory and was dropped in Postgres behind a 204. The step
//! PUT now refuses a body that moves either; this pins the storage
//! underneath it, one body of assertions run against each adapter, so
//! the two cannot drift apart again without this file naming which one
//! moved (CLAUDE.md §9a).

use boss_core::job::{Assurance, Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

fn job(id: JobId) -> Job {
    Job {
        id,
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Approve".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

async fn a_step_update_keeps_its_assurance_and_plugin_version<R: JobsRepository>(
    repo: &R,
    adapter: &str,
) {
    let job_id = JobId::from_uuid(Uuid::new_v4());
    repo.create_job_at(&job(job_id), Utc::now(), &[])
        .await
        .expect("create job");
    let mut approve = Step::new(job_id, "task", "Approve in person", 0);
    approve.status = StepStatus::Ready;
    approve.assurance_required = Some(Assurance::Presence);
    repo.add_step_at(&approve, Utc::now(), &[])
        .await
        .expect("add step");
    let stored = repo
        .get_step(&approve.id)
        .await
        .unwrap()
        .expect("step exists");

    let mut next = stored.clone();
    next.assurance_required = None;
    next.step_plugin_version = stored.step_plugin_version + 4;
    next.title = "Approve, in person".into();
    repo.update_step_at(&next, Utc::now(), &[])
        .await
        .expect("update accepted");

    let after = repo
        .get_step(&approve.id)
        .await
        .unwrap()
        .expect("step exists");
    assert_eq!(
        after.assurance_required,
        Some(Assurance::Presence),
        "{adapter}: an update never lowers the assurance requirement"
    );
    assert_eq!(
        after.step_plugin_version, stored.step_plugin_version,
        "{adapter}: an update never moves the plugin version"
    );
    assert_eq!(
        after.title, "Approve, in person",
        "{adapter}: the update itself landed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_memory_adapter_keeps_a_steps_assurance() {
    let repo = InMemoryJobs::new();
    a_step_update_keeps_its_assurance_and_plugin_version(&repo, "in-memory").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pg_adapter_keeps_a_steps_assurance() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    a_step_update_keeps_its_assurance_and_plugin_version(&repo, "postgres").await;
}
