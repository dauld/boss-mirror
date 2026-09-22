//! A step's `assurance_required` holds on EVERY path that completes it.
//!
//! THE DEFECT (backlog 148549c5), measured live on packet d5efbb3c,
//! 2026-09-22. The first presence-assured step in the system was
//! completed with no passkey ceremony, no stamp and no refusal:
//!
//! ```text
//!   step `approve`
//!     assurance_required = presence     <- still declared
//!     status             = completed
//!     completed_by       = emp-david
//!     sign_offs          = []           <- no stamp at all
//! ```
//!
//! `execute` then went ready and the host ran the verb. The whole
//! guarantee of design 17835005 — that a destructive ops verb cannot
//! run until a passkey has signed the rendered plan — rests on that
//! step, and an ordinary `PUT /api/jobs/{id}/steps/{step_id}` walked
//! past it.
//!
//! WHY, counted rather than described: the entire check lived in
//! `post_step_sign_off`. Inside `update_step` the string "assurance"
//! appeared ZERO times. The control was OPT-IN — it applied on the
//! path a step reaches only when it also declares a required sign-off
//! role, and not on the path everything else uses.
//!
//! One function below the hole the code already said the right thing:
//! "NO BYPASS, which is the point David settled in Q3: an assurance
//! level with a bypass is a comment, not a control." True of the
//! sign-off path; false of the system.
//!
//! WHAT THE FIX IS, and what it deliberately is NOT. `assurance_required`
//! is a property of the STEP, so every path that completes one owes it
//! the same refusal — one judgement, called from both (§9a). It is NOT
//! a refusal of every write: a metadata write to a step that stays
//! ready needs no ceremony, and refusing those would break the filer
//! that puts the plan on the step in the first place. The line is
//! COMPLETION.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{
    Assurance, Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject,
};
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

const JOB: &str = "00000000-0000-0000-0000-00000000c001";
const GUARDED: &str = "00000000-0000-0000-0000-00000000d001";
const ORDINARY: &str = "00000000-0000-0000-0000-00000000d002";

fn operator() -> User {
    User {
        id: "emp-david".to_string(),
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

fn step(id: &str, assurance: Option<Assurance>) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "generic".into(),
        title: "Approve the plan".into(),
        spec_slug: Some("approve".into()),
        assignee_id: Some("emp-david".into()),
        status: StepStatus::Ready,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: assurance,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "plan": "{\"verb\":\"commission-a-disk\"}" }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
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
        title: "df on forge".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    })
    .await
    .unwrap();
    jobs.add_step(&step(GUARDED, Some(Assurance::Presence)))
        .await
        .unwrap();
    jobs.add_step(&step(ORDINARY, None)).await.unwrap();
    (router(state), jobs)
}

async fn put(app: &Router, step_id: &str, body: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{JOB}/steps/{step_id}"))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(body.to_string()))
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
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
        .status
}

/// THE BUG, in the shape it was measured: a status flip completed a
/// presence-required step with no ceremony.
#[tokio::test]
async fn a_plain_put_cannot_complete_a_presence_required_step() {
    let (app, jobs) = seed().await;

    let (status, body) = put(&app, GUARDED, r#"{"status":"completed"}"#).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a step declaring presence must refuse a completion carrying none — this is the \
         gate a destructive ops verb waits behind; body: {body}"
    );
    assert!(
        body.contains("assurance"),
        "the refusal must name what is missing, the way the sign-off path does: {body}"
    );
    assert_eq!(
        status_of(&jobs, GUARDED).await,
        StepStatus::Ready,
        "the refused write must NOT have completed the step"
    );
}

/// THE CONTROL that keeps the fix from being "refuse everything": an
/// ordinary step, declaring no assurance, still completes by PUT. That
/// is how almost every step in the system is completed — the
/// dispatcher, the conductor, `boss step complete`, the page march —
/// and breaking it would stop the pipeline.
#[tokio::test]
async fn an_ordinary_step_still_completes_by_put() {
    let (app, jobs) = seed().await;
    let (status, body) = put(&app, ORDINARY, r#"{"status":"completed"}"#).await;
    assert!(
        status.is_success(),
        "a step demanding nothing must still complete normally: {status} {body}"
    );
    assert_eq!(status_of(&jobs, ORDINARY).await, StepStatus::Completed);
}

/// THE SECOND CONTROL, and the one that keeps the filer working: a
/// write that does NOT complete the step is untouched. The plan is put
/// onto the approve step by an ordinary metadata write before anyone
/// signs it; refusing that would make the guarded step unusable rather
/// than guarded.
#[tokio::test]
async fn a_write_that_does_not_complete_the_step_needs_no_assurance() {
    let (app, jobs) = seed().await;
    let (status, body) = put(
        &app,
        GUARDED,
        r#"{"metadata":{"plan":"{\"verb\":\"df\"}","note":"re-rendered"}}"#,
    )
    .await;
    assert!(
        status.is_success(),
        "a metadata write to a step that stays ready needs no ceremony: {status} {body}"
    );
    assert_eq!(
        status_of(&jobs, GUARDED).await,
        StepStatus::Ready,
        "and it leaves the step ready"
    );
}

/// A SKIP IS A COMPLETION TOO, as far as the downstream predicate is
/// concerned: `execute` waits on `steps.approve.done`, and a skipped
/// step is done. So the guard has to cover it, or the bypass simply
/// moves one word over.
#[tokio::test]
async fn a_skip_cannot_walk_past_the_requirement_either() {
    let (app, jobs) = seed().await;
    let (status, body) = put(&app, GUARDED, r#"{"status":"skipped"}"#).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "skipping a presence-required step is completing it by another name; body: {body}"
    );
    assert_eq!(status_of(&jobs, GUARDED).await, StepStatus::Ready);
}
