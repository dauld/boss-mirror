//! FRESHNESS OF THE RUN EDGE BELONGS AT THE CLAIM (backlog 9562f6df).
//! Postgres half, because the claim CAS is one SQL statement and "the
//! same holder" includes the holder spelled by alias (`actor_aliases`,
//! design 6fda05ae; d7fef617), which only that statement can see.
//!
//! `boss dispatch` writes `agent_run` onto the step it claims (dd6d44b7)
//! and the delivery rule follows it from `step.done.<kind>` to land the
//! run. The claim route never touched metadata, so a step that came
//! free with the previous run still named — through any door that did
//! not null it — and was claimed by someone else kept naming that run.
//! Before b91a2103 a wholesale step PUT happened to wipe the edge,
//! correct for the wrong reason; the carry-forward removed the accident
//! without adding the intent. The claim that CHANGES the holder now
//! clears it, and a re-claim by the holder stays the idempotent no-op
//! the CAS was designed to be.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::JobsRepository;
use boss_jobs::agent_runs::EDGE_KEY;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

/// The one live alias pair, seeded by the migration that opened the
/// alias table — the same rows `step_claim_admits_an_aliased_holder_pg`
/// reads.
const ALIAS: &str = "claude@algedonic.dev";
const AGENT: &str = "agent-claude";
const OLD_RUN: &str = "5b1d2c3e-0000-4000-8000-00000000dead";

async fn seeded_step(repo: &boss_jobs::PgJobs, status: StepStatus, holder: Option<&str>) -> StepId {
    let job_id = JobId::from_uuid(Uuid::new_v4());
    let job = Job {
        id: job_id,
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/it/backlog"),
        title: "A step the previous run left its edge on".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 23).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let mut step = Step::new(job_id, "task", "Build it", 0);
    step.assignee_id = holder.map(str::to_string);
    step.spec_slug = Some("build".into());
    step.status = status;
    step.metadata = serde_json::json!({ EDGE_KEY: OLD_RUN, "notes": "kept" });
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    step.id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_by_a_new_holder_clears_the_previous_runs_edge() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, StepStatus::Ready, None).await;

    let won = repo
        .claim_step_at(
            &step_id,
            "emp-person",
            &boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("test"),
            ),
            &[],
        )
        .await
        .expect("an unassigned ready step is claimable");
    assert_eq!(won.assignee_id.as_deref(), Some("emp-person"));
    assert!(
        won.metadata.get(EDGE_KEY).is_none(),
        "a new holder must not inherit the previous run's edge: {}",
        won.metadata
    );
    assert_eq!(won.metadata["notes"], "kept", "only the edge is cleared");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reclaim_by_the_holder_keeps_its_run_edge() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    for status in [StepStatus::Active, StepStatus::Ready] {
        let step_id = seeded_step(&repo, status, Some(AGENT)).await;
        let again = repo
            .claim_step_at(
                &step_id,
                AGENT,
                &boss_core::publisher::EventStamp::new(
                    "jobs",
                    boss_core::actor::ActorId::automation("test"),
                ),
                &[],
            )
            .await
            .expect("re-claim by the holder");
        assert_eq!(
            again.metadata[EDGE_KEY], OLD_RUN,
            "a {status:?} step re-claimed by its holder keeps its edge"
        );
    }
}

/// The holder spelled by alias is the SAME holder: the CAS rewrites the
/// spelling to the registered id, and that rewrite is not a change of
/// executor, so the edge stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_claim_that_only_respells_the_holder_keeps_its_run_edge() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, StepStatus::Ready, Some(ALIAS)).await;

    let won = repo
        .claim_step_at(
            &step_id,
            AGENT,
            &boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("test"),
            ),
            &[],
        )
        .await
        .expect("the holder, spelled by its registered id, claims its own step");
    assert_eq!(won.assignee_id.as_deref(), Some(AGENT));
    assert_eq!(won.metadata[EDGE_KEY], OLD_RUN);
}
