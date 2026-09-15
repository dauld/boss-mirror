//! The claim primitive (queue-visibility Q2): Ready→Active is a
//! compare-and-set, not a blind PUT. Two actors pulling the same
//! step from a group queue must resolve to exactly one winner, and
//! the loser must be told who holds it — otherwise the network
//! canvas will one day animate two people winning the same packet.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

async fn seeded_step(status: StepStatus, assignee: Option<&str>) -> (InMemoryJobs, StepId) {
    let jobs = InMemoryJobs::new();
    let job_id = JobId::from_uuid(Uuid::new_v4());
    let job = Job {
        id: job_id,
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Repair".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let mut step = Step::new(job_id, "task", "Do the work", 0);
    step.spec_slug = Some("work".into());
    step.status = status;
    step.assignee_id = assignee.map(str::to_string);
    let step_id = step.id;
    jobs.create_job_at(&job, Utc::now(), &[]).await.unwrap();
    jobs.add_step_at(&step, Utc::now(), &[]).await.unwrap();
    (jobs, step_id)
}

#[tokio::test]
async fn a_claim_wins_exactly_once() {
    let (jobs, step_id) = seeded_step(StepStatus::Ready, None).await;

    let won = jobs
        .claim_step_at(&step_id, "emp-a", Utc::now(), &[])
        .await
        .expect("first claim wins");
    assert_eq!(won.assignee_id.as_deref(), Some("emp-a"));
    assert_eq!(won.status, StepStatus::Active);

    let lost = jobs.claim_step_at(&step_id, "emp-b", Utc::now(), &[]).await;
    match lost {
        Err(JobsError::ClaimConflict { holder, status }) => {
            assert_eq!(holder.as_deref(), Some("emp-a"), "loser learns the holder");
            assert_eq!(status, "active");
        }
        other => panic!("second claim must conflict, got {other:?}"),
    }

    // The row still shows the winner — the losing claim wrote nothing.
    let after = jobs.get_step(&step_id).await.unwrap().unwrap();
    assert_eq!(after.assignee_id.as_deref(), Some("emp-a"));
}

#[tokio::test]
async fn reclaim_by_the_holder_is_idempotent() {
    let (jobs, step_id) = seeded_step(StepStatus::Ready, None).await;
    jobs.claim_step_at(&step_id, "emp-a", Utc::now(), &[])
        .await
        .expect("claim");
    let again = jobs
        .claim_step_at(&step_id, "emp-a", Utc::now(), &[])
        .await
        .expect("re-claim by the same actor is a no-op success");
    assert_eq!(again.assignee_id.as_deref(), Some("emp-a"));
    assert_eq!(again.status, StepStatus::Active);
}

#[tokio::test]
async fn a_step_that_is_not_ready_cannot_be_claimed() {
    for status in [
        StepStatus::Pending,
        StepStatus::Completed,
        StepStatus::Skipped,
    ] {
        let (jobs, step_id) = seeded_step(status, None).await;
        let res = jobs.claim_step_at(&step_id, "emp-a", Utc::now(), &[]).await;
        match res {
            Err(JobsError::ClaimConflict { holder, status: st }) => {
                assert_eq!(holder, None);
                assert_ne!(st, "ready");
            }
            other => panic!("claiming a {status:?} step must fail, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_dispatcher_assigned_ready_step_is_not_poachable() {
    // The group-queue lens hides steps another actor already holds;
    // the CAS is the backstop for a stale read racing that hide.
    let (jobs, step_id) = seeded_step(StepStatus::Ready, Some("emp-a")).await;
    let res = jobs.claim_step_at(&step_id, "emp-b", Utc::now(), &[]).await;
    match res {
        Err(JobsError::ClaimConflict { holder, status }) => {
            assert_eq!(holder.as_deref(), Some("emp-a"));
            assert_eq!(status, "ready");
        }
        other => panic!("poaching must conflict, got {other:?}"),
    }
}
