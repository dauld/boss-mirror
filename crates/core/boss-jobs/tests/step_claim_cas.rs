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
        opened_at: None,
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

/// A step seeded with a run edge naming the run that last executed it,
/// beside one ordinary metadata key the claim must not touch.
async fn seeded_step_with_edge(
    status: StepStatus,
    assignee: Option<&str>,
) -> (InMemoryJobs, StepId) {
    let (jobs, step_id) = seeded_step(status, assignee).await;
    let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
    step.metadata = serde_json::json!({
        boss_jobs::agent_runs::EDGE_KEY: "run-that-went-before",
        "notes": "kept",
    });
    jobs.update_step(&step).await.unwrap();
    (jobs, step_id)
}

/// FRESHNESS OF THE RUN EDGE BELONGS AT THE CLAIM (backlog 9562f6df).
/// A step that came free with the previous run's `agent_run` still on
/// it — freed by any door that did not null it — and was then claimed
/// by someone else, a person in the UI say, kept naming that run, so a
/// completion by the new holder would deliver onto a run that did not
/// do the work. The claim that CHANGES the holder clears the edge.
#[tokio::test]
async fn a_claim_by_a_new_holder_clears_the_previous_runs_edge() {
    let (jobs, step_id) = seeded_step_with_edge(StepStatus::Ready, None).await;
    let won = jobs
        .claim_step_at(&step_id, "emp-b", Utc::now(), &[])
        .await
        .expect("claim");
    assert!(
        won.metadata.get(boss_jobs::agent_runs::EDGE_KEY).is_none(),
        "a new holder must not inherit the previous run's edge: {}",
        won.metadata
    );
    assert_eq!(won.metadata["notes"], "kept", "only the edge is cleared");
    let after = jobs.get_step(&step_id).await.unwrap().unwrap();
    assert!(
        after
            .metadata
            .get(boss_jobs::agent_runs::EDGE_KEY)
            .is_none()
    );
}

/// And a re-claim by the SAME holder is the idempotent no-op it was
/// designed to be — the edge its own dispatch wrote survives it, both
/// on a step it already holds active and on one nominated to it.
#[tokio::test]
async fn a_reclaim_by_the_holder_keeps_its_run_edge() {
    for status in [StepStatus::Active, StepStatus::Ready] {
        let (jobs, step_id) = seeded_step_with_edge(status, Some("emp-a")).await;
        let again = jobs
            .claim_step_at(&step_id, "emp-a", Utc::now(), &[])
            .await
            .expect("re-claim by the holder");
        assert_eq!(
            again.metadata[boss_jobs::agent_runs::EDGE_KEY],
            "run-that-went-before",
            "a {status:?} step re-claimed by its holder keeps its edge"
        );
    }
}
