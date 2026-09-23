//! THE CLAIM'S EVENT SAYS WHAT THE CLAIM DID TO THE RUN EDGE (backlog
//! 9562f6df). The claim CAS now clears `agent_run` when the holder
//! changes; the route builds the `jobs.step.updated` payload before the
//! CAS, as the optimistic post-state the CAS makes real. If that payload
//! still carried the previous run's edge, the row and the log would
//! disagree about which run a step names — and the log is the system of
//! record, so a rebuild would put the stale edge back.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agent_runs::EDGE_KEY;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{NaiveDate, Utc};
use tower::ServiceExt;
use uuid::Uuid;

const OLD_RUN: &str = "5b1d2c3e-0000-4000-8000-00000000dead";

fn user_header(id: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: "ceo".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn app(jobs: Arc<InMemoryJobs>) -> axum::Router {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState::minimal(
        jobs,
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

async fn seeded(jobs: &InMemoryJobs, status: StepStatus, holder: Option<&str>) -> (JobId, StepId) {
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
    jobs.create_job_at(&job, Utc::now(), &[]).await.unwrap();
    jobs.add_step_at(&step, Utc::now(), &[]).await.unwrap();
    (job_id, step.id)
}

/// Claim through the route and return the metadata of the
/// `jobs.step.updated` payload the claim recorded.
async fn claim_and_read_event(
    jobs: Arc<InMemoryJobs>,
    job: JobId,
    step: StepId,
    as_: &str,
) -> serde_json::Value {
    let before = jobs.recorded_events().len();
    let resp = app(jobs.clone())
        .oneshot(
            Request::post(format!("/api/jobs/{job}/steps/{step}/claim"))
                .header("x-boss-user", user_header(as_))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let recorded = jobs.recorded_events();
    let updated = recorded[before..]
        .iter()
        .find(|e| e.kind == boss_jobs::events::STEP_UPDATED)
        .expect("a claim records jobs.step.updated");
    updated.payload["metadata"].clone()
}

#[tokio::test]
async fn a_claim_by_a_new_holder_records_the_edge_cleared() {
    let jobs = Arc::new(InMemoryJobs::new());
    let (job, step) = seeded(&jobs, StepStatus::Ready, None).await;
    let md = claim_and_read_event(jobs.clone(), job, step, "emp-person").await;
    assert!(
        md.get(EDGE_KEY).is_none(),
        "the log must not keep naming the run the row no longer names: {md}"
    );
    assert_eq!(md["notes"], "kept");
    let row = jobs.get_step(&step).await.unwrap().unwrap();
    assert!(row.metadata.get(EDGE_KEY).is_none());
}

#[tokio::test]
async fn a_reclaim_by_the_holder_records_the_edge_kept() {
    let jobs = Arc::new(InMemoryJobs::new());
    let (job, step) = seeded(&jobs, StepStatus::Active, Some("emp-person")).await;
    let md = claim_and_read_event(jobs.clone(), job, step, "emp-person").await;
    assert_eq!(
        md[EDGE_KEY], OLD_RUN,
        "an idempotent re-claim changes nothing"
    );
}
