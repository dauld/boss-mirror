//! A step the engine has already made ready cannot be "blocked".
//!
//! THE INCIDENT, 2026-09-22. The ops-request protocol gained an
//! approval step gated on a flag, and `execute` waited for it with a
//! DISJUNCTION:
//!
//! ```text
//!   steps.filed.done AND (NOT job.metadata.requires_approval
//!                         OR steps.approve.done)
//! ```
//!
//! Every ops-request on the forge jammed. 16 stuck at `execute`, the
//! oldest 68 minutes, including `converge on forge — a train merged to
//! main`; the runner logged `PUT failed … 409` once a minute and
//! answered nothing. Deploys survived only because the converge timer
//! is an independent floor of the fast path it jammed.
//!
//! WHY. `blocked_by` is derived from a predicate's REFERENCES, not from
//! its TRUTH — CLAUDE.md calls it exactly what it is, "a
//! predicate-derived denormalized edge list for DAG rendering". It
//! cannot represent a disjunction: naming `steps.approve.done` anywhere
//! in the expression lists approve as a blocker, whatever branch
//! actually made the step ready. So the readiness engine said READY
//! (the `NOT` arm holds when the flag is absent) and the step WRITE
//! refused with "step has unresolved blockers: approve=pending".
//!
//! TWO READINGS OF ONE EDGE, AND ONLY ONE IS THE AUTHORITY. The
//! predicate decides readiness; the engine evaluates it; `blocked_by`
//! is a rendering aid the write path had turned into a gate. Where they
//! disagree, the rendering aid was winning.
//!
//! THE FIX, and its bound. The blocker refusal now applies only to a
//! step the engine has NOT already opened. A `pending` step being
//! completed out of order is still refused — that is the guard's real
//! job and it keeps it. A step that is `ready` or `active` has had its
//! predicate judged true by the engine, so its blockers are not
//! unresolved; they are simply not all required.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-00000000e001";
const GATEKEEPER: &str = "00000000-0000-0000-0000-00000000f001";
const READY_STEP: &str = "00000000-0000-0000-0000-00000000f002";
const PENDING_STEP: &str = "00000000-0000-0000-0000-00000000f003";

fn operator() -> User {
    User {
        id: "automation-runner".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    )
}

fn step(id: &str, slug: &str, status: StepStatus, blocked_by: Vec<StepId>) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "task".into(),
        title: slug.into(),
        spec_slug: Some(slug.into()),
        assignee_id: None,
        status,
        sort_order: 1,
        blocked_by,
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

fn sid(s: &str) -> StepId {
    StepId::from_uuid(Uuid::parse_str(s).unwrap())
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    jobs.create_job(&Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "ops-request".into(),
        workflow_version: 2,
        subject: Subject::new("custom", "forge"),
        title: "converge on forge".into(),
        owner_id: "automation-runner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "verb": "converge", "host": "forge" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    })
    .await
    .unwrap();
    // The gatekeeper stays PENDING — exactly the approve step whose
    // flag was absent, so the disjunction's other arm carried the day.
    jobs.add_step(&step(GATEKEEPER, "approve", StepStatus::Pending, vec![]))
        .await
        .unwrap();
    jobs.add_step(&step(
        READY_STEP,
        "execute",
        StepStatus::Ready,
        vec![sid(GATEKEEPER)],
    ))
    .await
    .unwrap();
    jobs.add_step(&step(
        PENDING_STEP,
        "answered",
        StepStatus::Pending,
        vec![sid(GATEKEEPER)],
    ))
    .await
    .unwrap();
    (router(state), jobs)
}

async fn complete(app: &Router, step_id: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{JOB}/steps/{step_id}"))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(r#"{"status":"completed"}"#.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn status_of(jobs: &InMemoryJobs, id: &str) -> StepStatus {
    jobs.get_step(&sid(id))
        .await
        .unwrap()
        .expect("the step is there")
        .status
}

/// THE JAM, reproduced: a READY step whose blocker list names a pending
/// step must complete. This is the runner's PUT, and it 409'd once a
/// minute for over an hour.
#[tokio::test]
async fn a_ready_step_completes_though_a_listed_blocker_is_pending() {
    let (app, jobs) = seed().await;

    let (status, body) = complete(&app, READY_STEP).await;

    assert!(
        status.is_success(),
        "the engine made this step ready, so its predicate holds — the write must not \
         refuse it over a rendering aid that cannot express a disjunction. This is the \
         409 that jammed every ops-request on the forge; {status} {body}"
    );
    assert_eq!(status_of(&jobs, READY_STEP).await, StepStatus::Completed);
}

/// THE GUARD'S REAL JOB, kept: a PENDING step with an unresolved
/// blocker is still refused. Without this the fix would be "stop
/// checking", and a step could be completed out of order.
#[tokio::test]
async fn a_pending_step_with_an_unresolved_blocker_is_still_refused() {
    let (app, jobs) = seed().await;

    let (status, body) = complete(&app, PENDING_STEP).await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a step the engine has NOT opened is still gated by its blockers — completing it \
         would be going out of order; body: {body}"
    );
    assert!(
        body.contains("unresolved"),
        "and the refusal still names what is unresolved: {body}"
    );
    assert_eq!(
        status_of(&jobs, PENDING_STEP).await,
        StepStatus::Pending,
        "the refused write changed nothing"
    );
}
