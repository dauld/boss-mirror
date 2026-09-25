//! Both adapters refuse to move a finished packet's status (backlog
//! 570e72bd, road 5).
//!
//! The job PUT's reopen refusal read the row, judged it, then wrote the
//! whole row back: a close that committed between the read and the
//! write — the terminal close, the catch-all, another operator — was
//! overwritten, and the packet came back open with its outcome erased.
//! The handler cannot close that window from where it stands; the
//! storage can, because it is the one place that sees the row as it is
//! when the write lands. So `update_job_at` is a compare-and-set on a
//! finished status: a Closed or Cancelled row keeps its status, and a
//! write that would move it is refused as `TerminalJob` — never
//! answered as if it had landed. One body of assertions runs against
//! each adapter, so the two cannot drift apart (CLAUDE.md §9a).

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::{InMemoryJobs, JobsError, JobsRepository};
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

fn job(id: JobId) -> Job {
    Job {
        id,
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Finish".into(),
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

async fn a_finished_status_does_not_move<R: JobsRepository>(repo: &R, adapter: &str) {
    for finished in [JobStatus::Closed, JobStatus::Cancelled] {
        let id = JobId::from_uuid(Uuid::new_v4());
        let stale = job(id);
        repo.create_job_at(&stale, Utc::now(), &[])
            .await
            .expect("create job");

        // The close that lands between a writer's read and its write.
        let mut ended = stale.clone();
        ended.status = finished;
        ended.closed_on = Some(NaiveDate::from_ymd_opt(2026, 8, 2).unwrap());
        ended.metadata = serde_json::json!({ "outcome": "done" });
        repo.update_job_at(&ended, Utc::now(), &[])
            .await
            .expect("an open packet ends");

        // The writer's copy, read while it was open.
        let mut late = stale.clone();
        late.title = "Written from a read taken before the close".into();
        let err = repo
            .update_job_at(&late, Utc::now(), &[])
            .await
            .expect_err("a stale open copy must not reopen a finished packet");
        assert!(
            matches!(err, JobsError::TerminalJob { .. }),
            "{adapter}: refused as TerminalJob, got {err:?}"
        );

        // Nor does a finished packet move to the other end state.
        let mut flipped = ended.clone();
        flipped.status = match finished {
            JobStatus::Closed => JobStatus::Cancelled,
            _ => JobStatus::Closed,
        };
        let err = repo
            .update_job_at(&flipped, Utc::now(), &[])
            .await
            .expect_err("a finished packet's end state does not flip");
        assert!(
            matches!(err, JobsError::TerminalJob { .. }),
            "{adapter}: {err:?}"
        );

        let after = repo.get_job(&id).await.unwrap().expect("job exists");
        assert_eq!(after.status, finished, "{adapter}: the end state stands");
        assert_eq!(
            after.metadata["outcome"], "done",
            "{adapter}: the outcome stands"
        );
        assert_eq!(
            after.title, "Finish",
            "{adapter}: nothing of the write landed"
        );

        // Control: a write that keeps the finished status still lands —
        // a retitle after the close is not a reopen.
        let mut retitled = after.clone();
        retitled.title = "Retitled after the close".into();
        repo.update_job_at(&retitled, Utc::now(), &[])
            .await
            .expect("a write that keeps the status lands");
        let after = repo.get_job(&id).await.unwrap().expect("job exists");
        assert_eq!(after.title, "Retitled after the close", "{adapter}");
    }

    // Control: a job that does not exist is still NotFound.
    let missing = job(JobId::from_uuid(Uuid::new_v4()));
    let err = repo
        .update_job_at(&missing, Utc::now(), &[])
        .await
        .expect_err("no such job");
    assert!(matches!(err, JobsError::NotFound(_)), "{adapter}: {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_memory_adapter_keeps_a_finished_status() {
    let repo = InMemoryJobs::new();
    a_finished_status_does_not_move(&repo, "in-memory").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pg_adapter_keeps_a_finished_status() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    a_finished_status_does_not_move(&repo, "postgres").await;
}
