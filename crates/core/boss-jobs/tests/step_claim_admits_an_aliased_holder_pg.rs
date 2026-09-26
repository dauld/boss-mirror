//! THE CLAIM DOOR ADMITS ITS OWN HOLDER SPELLED BY ALIAS (backlog
//! d7fef617). Postgres half of the claim CAS, because the alias relation
//! is a table (`actor_aliases`, design 6fda05ae) and the CAS is one SQL
//! statement.
//!
//! Measured 2026-09-19 00:50Z on the second live `boss dispatch`
//! (da925366): the dispatcher's executor lane had nominated the step
//! with the raw env spelling `claude@algedonic.dev`; the jobs API's
//! login door rewrote the same actor's claim to `agent-claude`; and the
//! CAS (`assignee_id IS NULL OR assignee_id = $2`) answered 409
//! {holder: claude@algedonic.dev, status: ready} to the very actor named
//! as holder. The other half of the fix nominates the registered id, but
//! every step nominated BEFORE it landed still holds the alias, so the
//! CAS itself admits a holder that is an alias of the claimant and
//! rewrites the holder to the registered id in the same transaction —
//! and refuses everyone else exactly as before.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

/// The one live pair, seeded by the migration that opened the alias
/// table — the same rows `an_agent_login_resolves_to_its_registered_identity`
/// reads.
const ALIAS: &str = "claude@algedonic.dev";
const AGENT: &str = "agent-claude";

async fn seeded_step(repo: &boss_jobs::PgJobs, holder: &str) -> StepId {
    let job_id = JobId::from_uuid(Uuid::new_v4());
    let job = Job {
        id: job_id,
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/it/backlog"),
        title: "Nominated before the lane resolved aliases".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let mut step = Step::new(job_id, "task", "Build it", 0).with_assignee(holder);
    step.spec_slug = Some("build".into());
    step.status = StepStatus::Ready;
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    step.id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_as_the_registered_id_takes_a_step_held_by_its_alias() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, ALIAS).await;

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
    assert_eq!(won.status, StepStatus::Active);
    assert_eq!(
        won.assignee_id.as_deref(),
        Some(AGENT),
        "the holder is rewritten to the registered id: one spelling from here on"
    );
    let after = repo.get_step(&step_id).await.unwrap().unwrap();
    assert_eq!(after.assignee_id.as_deref(), Some(AGENT));

    // And the re-claim by the same actor stays the idempotent no-op it
    // was: active, held by the registered id.
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
    assert_eq!(again.assignee_id.as_deref(), Some(AGENT));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_held_by_someone_else_still_refuses_the_claim() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, "emp-someone-else").await;

    let lost = repo
        .claim_step_at(
            &step_id,
            AGENT,
            &boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("test"),
            ),
            &[],
        )
        .await;
    match lost {
        Err(JobsError::ClaimConflict { holder, status }) => {
            assert_eq!(holder.as_deref(), Some("emp-someone-else"));
            assert_eq!(status, "ready");
        }
        other => panic!("a step held by another actor must conflict, got {other:?}"),
    }
    let after = repo.get_step(&step_id).await.unwrap().unwrap();
    assert_eq!(
        after.assignee_id.as_deref(),
        Some("emp-someone-else"),
        "the refused claim wrote nothing"
    );
    assert_eq!(after.status, StepStatus::Ready);
}

/// The relation is directional: the ALIAS is not admitted to a step
/// the registered id holds. The door never signs a request as the
/// alias any more, so this claimant is an unresolved login — and an
/// unresolved login is not the holder.
#[tokio::test(flavor = "multi_thread")]
async fn the_alias_is_not_admitted_to_a_step_the_registered_id_holds() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let step_id = seeded_step(&repo, AGENT).await;

    let lost = repo
        .claim_step_at(
            &step_id,
            ALIAS,
            &boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("test"),
            ),
            &[],
        )
        .await;
    match lost {
        Err(JobsError::ClaimConflict { holder, status }) => {
            assert_eq!(holder.as_deref(), Some(AGENT));
            assert_eq!(status, "ready");
        }
        other => panic!("the alias must not take the registered id's step, got {other:?}"),
    }
}
