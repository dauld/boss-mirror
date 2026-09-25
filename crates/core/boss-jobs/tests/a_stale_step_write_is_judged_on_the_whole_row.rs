//! A stale step write is judged on the WHOLE row, and a row that went
//! terminal records nothing it refused (backlog 6ec22d71).
//!
//! Car 88123ae0 (e381689d) made every read-modify-write step writer
//! land only over the METADATA it read. Two reviews reproduced what that
//! left, on 2026-09-25:
//!
//! - a claim moves `status` and `assignee_id` and touches no metadata,
//!   so the assignment PUT that had read the step `ready` just before
//!   wrote `ready` and its own holder back over the claim, and answered
//!   204;
//! - a row that went `completed` in the window was not judged at all:
//!   the write answered Ok, the row kept its values under the terminal
//!   CASE, and a `jobs.step.updated` carrying the stale values was
//!   recorded anyway — which `rebuild.rs::upsert_step` replays verbatim,
//!   so the rebuilt row disagrees with the live one (determinism).
//!
//! The judgement is now the row's VERSION as read, so any column any
//! writer moved refuses the write; and a terminal row takes a write only
//! when it would move nothing the row froze, so no event records what
//! the row refused. `a_stale_step_write_is_judged_on_the_whole_row_pg.rs`
//! pins the same rules against the SQL.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::port::{JobsError, StepVersion};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn dispatcher() -> User {
    User {
        id: "automation:dispatcher".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("platform".into()),
    }
}

fn run_packet(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "agent-run".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "run"),
        title: "A run whose step is written twice at once".into(),
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

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

/// A ready, unassigned step carrying only its `authority_role`.
async fn ready_step(jobs: &InMemoryJobs, job_id: &str) -> (Job, Step) {
    let j = run_packet(job_id);
    jobs.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Briefed", 1);
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    jobs.add_step(&step).await.unwrap();
    (j, step)
}

async fn put_step(app: &Router, job: &Job, step: &Step, body: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&dispatcher()).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The read a judged write is computed from: the row and its version.
async fn read(jobs: &InMemoryJobs, step: &Step) -> (Step, StepVersion) {
    jobs.get_step_versioned(&step.id).await.unwrap().unwrap()
}

fn updated_event(step: &Step) -> boss_core::event::Event {
    boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .event(
        boss_jobs::events::STEP_UPDATED,
        boss_jobs::events::step_state_payload(step),
    )
}

async fn complete(jobs: &InMemoryJobs, step: &Step) {
    let mut done = jobs.get_step(&step.id).await.unwrap().unwrap();
    done.status = StepStatus::Completed;
    jobs.update_step(&done).await.unwrap();
}

#[tokio::test]
async fn a_stale_assignment_put_over_a_claim_is_refused_and_the_claim_stands() {
    let (app, jobs) = build_app();
    let (job, step) = ready_step(&jobs, "00000000-0000-0000-0000-000000006ec1").await;
    let events_before = jobs.recorded_events().len();

    // The claim commits inside the assignment PUT's window: status and
    // holder move, metadata does not.
    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = StepStatus::Active;
        row.assignee_id = Some("agent-claimer".into());
    });
    let (status, body) = put_step(&app, &job, &step, r#"{"assignee_id":"agent-other"}"#).await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write computed from a read the row no longer matches must not answer success: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Active,
        "the claim's status stands"
    );
    assert_eq!(
        stored.assignee_id.as_deref(),
        Some("agent-claimer"),
        "the claim's holder stands"
    );
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "the refused write recorded nothing"
    );
}

#[tokio::test]
async fn a_stale_write_over_a_row_that_went_terminal_is_refused_and_records_nothing() {
    let jobs = InMemoryJobs::new();
    let (_job, step) = ready_step(&jobs, "00000000-0000-0000-0000-000000006ec2").await;
    let (before, version) = read(&jobs, &step).await;
    complete(&jobs, &step).await;
    let events_before = jobs.recorded_events().len();

    let mut stale = before.clone();
    stale.assignee_id = Some("agent-late".into());
    let answer = jobs
        .update_step_if_unchanged_at(
            &stale,
            version,
            chrono::Utc::now(),
            &[updated_event(&stale)],
        )
        .await;

    assert!(
        matches!(answer, Err(JobsError::StepChanged { id }) if id == step.id),
        "a write read before the completion is refused by name, got {answer:?}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Completed);
    assert_eq!(stored.assignee_id, None);
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "no jobs.step.updated records the stale values the row refused — \
         the rebuild would replay them"
    );
}

#[tokio::test]
async fn a_terminal_row_read_afresh_takes_a_resend_that_changes_nothing() {
    // The idempotent re-send a redelivery makes still lands.
    let jobs = InMemoryJobs::new();
    let (_job, step) = ready_step(&jobs, "00000000-0000-0000-0000-000000006ec3").await;
    complete(&jobs, &step).await;
    let (done, version) = read(&jobs, &step).await;
    jobs.update_step_if_unchanged_at(&done, version, chrono::Utc::now(), &[updated_event(&done)])
        .await
        .unwrap();
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Completed);
}

#[tokio::test]
async fn a_terminal_row_refuses_a_write_that_would_move_what_it_froze() {
    let jobs = InMemoryJobs::new();
    let (_job, step) = ready_step(&jobs, "00000000-0000-0000-0000-000000006ec4").await;
    complete(&jobs, &step).await;
    let (done, version) = read(&jobs, &step).await;
    let events_before = jobs.recorded_events().len();

    let mut retitled = done.clone();
    retitled.title = "Not what was completed".into();
    let answer = jobs
        .update_step_if_unchanged_at(
            &retitled,
            version,
            chrono::Utc::now(),
            &[updated_event(&retitled)],
        )
        .await;

    assert!(
        matches!(answer, Err(JobsError::TerminalStep { id, .. }) if id == step.id),
        "a terminal row refuses by name a write that would move a frozen column, got {answer:?}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.title, "Briefed");
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "and records no event carrying the title the row kept out"
    );
}

#[tokio::test]
async fn every_write_to_a_row_moves_its_version() {
    let jobs = InMemoryJobs::new();
    let (_job, step) = ready_step(&jobs, "00000000-0000-0000-0000-000000006ec5").await;
    let (_, first) = read(&jobs, &step).await;
    let (_, again) = read(&jobs, &step).await;
    assert_eq!(first, again, "a read does not move the version");

    jobs.claim_step_at(
        &step.id,
        "agent-claimer",
        &boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("test"),
        ),
        &[],
    )
    .await
    .unwrap();
    let (_, claimed) = read(&jobs, &step).await;
    assert_ne!(claimed, first, "a claim moves it");

    jobs.merge_step_metadata_at(
        &step.id,
        &serde_json::Map::from_iter([("k".to_string(), serde_json::json!("v"))]),
        &boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("test".into()),
        ),
    )
    .await
    .unwrap();
    let (_, merged) = read(&jobs, &step).await;
    assert_ne!(merged, claimed, "a merge moves it");

    let listed = jobs.list_steps_versioned(&step.job_id).await.unwrap();
    let listed_version = listed
        .iter()
        .find(|(s, _)| s.id == step.id)
        .map(|(_, v)| *v)
        .unwrap();
    assert_eq!(
        listed_version, merged,
        "a list read answers the version a get answers"
    );
}
