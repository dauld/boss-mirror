//! Both adapters store a packet's protocol the same way (backlog
//! b433bdf3).
//!
//! The review that filed b433bdf3 found the two `JobsRepository`
//! adapters answering one write two ways: `update_job_at` stored the
//! body's `workflow_version` in memory and not in Postgres, and both
//! stored the body's `kind`; `update_step_at` stored the body's
//! `spec_slug` in memory and not in Postgres, and froze `fields` in
//! memory while Postgres wrote them on a live row (a07cfddd). The HTTP
//! handlers now refuse a body that moves any of these; this pins the
//! storage underneath them, and runs ONE body of assertions against
//! each adapter, so the two cannot drift apart again without this file
//! naming which one moved (CLAUDE.md §9a).

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepField, StepStatus, Subject};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

fn job(id: JobId) -> Job {
    Job {
        id,
        kind: "field-service".into(),
        workflow_version: 3,
        subject: Subject::new("asset", "SYS-1"),
        title: "Repair".into(),
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

fn field(name: &str) -> StepField {
    StepField {
        name: name.into(),
        field_type: "string".into(),
        required: true,
        filled_by: boss_core::job::FilledBy::Executor,
        item_keys: Vec::new(),
        covers: None,
        binds: None,
        item_value_max_bytes: None,
        item_one_of: Vec::new(),
    }
}

/// The job half: an update carrying another kind and version stores
/// neither, and still stores what it may (the title).
async fn a_job_update_keeps_the_admitted_kind_and_version<R: JobsRepository>(
    repo: &R,
    adapter: &str,
) {
    let id = JobId::from_uuid(Uuid::new_v4());
    repo.create_job_at(&job(id), Utc::now(), &[])
        .await
        .expect("create job");

    let mut reshaped = repo.get_job(&id).await.unwrap().expect("job exists");
    reshaped.kind = "no-such-kind".into();
    reshaped.workflow_version = 7;
    reshaped.title = "Retitled".into();
    repo.update_job_at(&reshaped, reshaped.status, Utc::now(), &[])
        .await
        .expect("update accepted");

    let after = repo.get_job(&id).await.unwrap().expect("job exists");
    assert_eq!(
        after.kind, "field-service",
        "{adapter}: the kind is the admission's"
    );
    assert_eq!(
        after.workflow_version, 3,
        "{adapter}: the version moves only by repin"
    );
    assert_eq!(
        after.title, "Retitled",
        "{adapter}: the update itself landed"
    );
}

/// The step half: the slug is never written by an update; the authored
/// contract is written on a live row and frozen on a terminal one.
async fn a_step_update_keeps_the_slug_and_freezes_fields_only_when_terminal<R: JobsRepository>(
    repo: &R,
    adapter: &str,
) {
    let job_id = JobId::from_uuid(Uuid::new_v4());
    repo.create_job_at(&job(job_id), Utc::now(), &[])
        .await
        .expect("create job");
    let mut live = Step::new(job_id, "task", "Live", 0);
    live.spec_slug = Some("work".into());
    live.status = StepStatus::Ready;
    live.fields = vec![field("evidence")];
    repo.add_step_at(&live, Utc::now(), &[])
        .await
        .expect("add live");
    let mut done = Step::new(job_id, "task", "Done", 1);
    done.spec_slug = Some("done".into());
    done.status = StepStatus::Completed;
    done.fields = vec![field("evidence")];
    repo.add_step_at(&done, Utc::now(), &[])
        .await
        .expect("add done");

    for (id, expect_fields) in [
        (live.id, vec![field("summary")]),
        (done.id, vec![field("evidence")]),
    ] {
        let mut next = repo.get_step(&id).await.unwrap().expect("step exists");
        next.spec_slug = Some("forged".into());
        next.fields = vec![field("summary")];
        repo.update_step_at(&next, Utc::now(), &[])
            .await
            .expect("update accepted");
        let after = repo.get_step(&id).await.unwrap().expect("step exists");
        assert_ne!(
            after.spec_slug.as_deref(),
            Some("forged"),
            "{adapter}: an update never writes the slug"
        );
        assert_eq!(
            after.fields, expect_fields,
            "{adapter}: fields on {:?}",
            after.status
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_memory_adapter_keeps_a_packets_protocol() {
    let repo = InMemoryJobs::new();
    a_job_update_keeps_the_admitted_kind_and_version(&repo, "in-memory").await;
    a_step_update_keeps_the_slug_and_freezes_fields_only_when_terminal(&repo, "in-memory").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pg_adapter_keeps_a_packets_protocol() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    a_job_update_keeps_the_admitted_kind_and_version(&repo, "postgres").await;
    a_step_update_keeps_the_slug_and_freezes_fields_only_when_terminal(&repo, "postgres").await;
}
