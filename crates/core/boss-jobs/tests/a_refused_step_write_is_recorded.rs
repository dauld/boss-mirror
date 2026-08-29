//! A refused step write leaves a trace.
//!
//! WHY. Step conformance measures 100% across 3,517 completed steps and
//! cannot measure anything else: required-at-done validators run on the
//! write path, so a step physically cannot reach `completed` without its
//! required fields. The number certifies the validator, not the work.
//! Every real failure lives in what the record never held — attempts
//! that did not become completions. Until this change a refusal was an
//! early `return` in `http/steps.rs`, above the OUTBOX block, so it
//! wrote nothing and emitted nothing.
//!
//! WHAT IS AND IS NOT BEING MEASURED. The count of refusals is NOT the
//! metric, and driving it to zero is not the goal — the cheapest way to
//! do that is to loosen validation, which would destroy the one
//! mechanism keeping conformance at 100%. A refusal is usually the
//! protocol working. The readings are (1) refusals never followed by a
//! success on that step, and (2) distinct actors hitting the same
//! (step, error_class) — several independent actors on one class means
//! the PROTOCOL is hard to comply with, which is the actor-independent
//! signal this exists to produce.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::refusals::ErrorClass;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-0000000000a1";
const DONE_STEP: &str = "00000000-0000-0000-0000-0000000000b1";
const LIVE_STEP: &str = "00000000-0000-0000-0000-0000000000b2";

fn operator() -> User {
    User {
        id: "emp-op".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn allow_update() -> Arc<dyn PolicyClient> {
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

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry: Arc::new(StepRegistry::v1()),
        policy: allow_update(),
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), jobs)
}

fn step(id: &str, status: StepStatus, metadata: serde_json::Value) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).expect("step uuid")),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).expect("job uuid")),
        kind: "generic".into(),
        title: "Reproduce and investigate".into(),
        spec_slug: Some("investigate".into()),
        assignee_id: Some("emp-op".into()),
        status,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: match status {
            StepStatus::Completed => NaiveDate::from_ymd_opt(2026, 8, 20),
            _ => None,
        },
        metadata,
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let (app, jobs) = build_app();
    let job = Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).expect("job uuid")),
        kind: "incident".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "The CI runner lost outbound network".into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 18).expect("date"),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    };
    jobs.create_job(&job).await.expect("create job");
    jobs.add_step(&step(
        DONE_STEP,
        StepStatus::Completed,
        serde_json::json!({ "finding": "the original sentence" }),
    ))
    .await
    .expect("add done step");
    jobs.add_step(&step(
        LIVE_STEP,
        StepStatus::Ready,
        serde_json::json!({ "finding": "still editable" }),
    ))
    .await
    .expect("add live step");
    (app, jobs)
}

async fn put_step(app: &Router, step_id: &str, body: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{JOB}/steps/{step_id}"))
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&operator()).expect("user json"),
                )
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn body_of(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn a_refused_step_write_is_recorded_with_its_class_and_actor() {
    let (app, jobs) = seed().await;

    let resp = put_step(
        &app,
        DONE_STEP,
        r#"{"metadata":{"finding":"the original sentence","correction":"this claim is false"}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let recorded = jobs.step_write_refusals(50).await.expect("read refusals");
    assert_eq!(
        recorded.len(),
        1,
        "the refusal must leave exactly one trace"
    );

    let r = &recorded[0].refusal;
    assert_eq!(
        r.error_class,
        ErrorClass::State,
        "a write refused because the step is already completed is a STATE refusal, \
         not a validation one — the actor complied with the protocol and lost a race \
         with the world"
    );
    assert_eq!(
        r.actor_id, "emp-op",
        "the actor is what makes reading 2 possible: several actors on one class \
         means the protocol is at fault, one actor across many classes means the actor is"
    );
    assert_eq!(r.status_code, 409);
    assert_eq!(r.method, "PUT");
    assert_eq!(
        r.job_id,
        Some(Uuid::parse_str(JOB).expect("job uuid")),
        "ids come off the path so the refusal can be paired with the step later"
    );
    assert_eq!(
        r.step_id,
        Some(Uuid::parse_str(DONE_STEP).expect("step id"))
    );
    assert!(
        !r.detail.is_empty(),
        "the detail is what tells a reader WHICH rule refused"
    );
}

/// The layer buffers the refusal body to record it. If it rebuilds the
/// response wrong, every caller loses the message that tells them what
/// to fix — a far worse outcome than not measuring at all.
#[tokio::test]
async fn recording_does_not_damage_the_response_the_caller_reads() {
    let (app, _jobs) = seed().await;

    let resp = put_step(
        &app,
        DONE_STEP,
        r#"{"metadata":{"finding":"the original sentence","correction":"this claim is false"}}"#,
    )
    .await;
    let status = resp.status();
    let body = body_of(resp).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.contains("metadata"),
        "the refusal must still name what it refused after passing through the \
         recording layer; got: {body}"
    );
}

#[tokio::test]
async fn a_write_that_lands_is_not_recorded() {
    let (app, jobs) = seed().await;

    let resp = put_step(&app, LIVE_STEP, r#"{"notes":"picked this up"}"#).await;
    assert!(
        resp.status().is_success(),
        "precondition: this write must succeed, got {}",
        resp.status()
    );

    assert!(
        jobs.step_write_refusals(50)
            .await
            .expect("read refusals")
            .is_empty(),
        "only refusals are recorded — counting successes would make the table a \
         request log rather than a measurement"
    );
}

#[tokio::test]
async fn a_refused_read_is_not_recorded() {
    let (app, jobs) = seed().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs/not-a-uuid/steps")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&operator()).expect("user json"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    assert!(
        jobs.step_write_refusals(50)
            .await
            .expect("read refusals")
            .is_empty(),
        "a refused READ says nothing about whether an obligation was dischargeable"
    );
}

#[tokio::test]
async fn the_recorded_refusal_is_readable_through_its_door() {
    // Without a read path the table is a black hole and "let's try it
    // for a while and see how it goes" has nothing to look at.
    let (app, _jobs) = seed().await;
    put_step(
        &app,
        DONE_STEP,
        r#"{"metadata":{"finding":"the original sentence","correction":"this claim is false"}}"#,
    )
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs/step-write-refusals")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&operator()).expect("user json"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_of(resp).await;
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["total"], 1, "body: {body}");
    assert_eq!(parsed["data"][0]["error_class"], "state", "body: {body}");
    assert_eq!(parsed["data"][0]["actor_id"], "emp-op", "body: {body}");
}
