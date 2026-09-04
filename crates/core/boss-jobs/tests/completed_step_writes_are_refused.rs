//! A write to a completed step must be REFUSED, not accepted and dropped.
//!
//! THE DEFECT (packet 09576fab). `PUT /api/jobs/{id}/steps/{step_id}`
//! against a completed step returns 204 and silently discards the parts
//! of the write the row freezes. Both adapters freeze `status`,
//! `completed_on` and `metadata` on a terminal row — deliberately, so a
//! write merged against a stale pre-completion fetch cannot demote a
//! finished step — but the handler reports success either way, so a
//! caller cannot tell a write that landed from one that vanished.
//!
//! WHAT IT COST. Three cars' stale gate receipts were "repaired", the
//! API said 204 three times, the writes never happened, and the cars
//! were reported fixed while staying unboardable. It cost two more
//! corrections on 2026-08-27: an accepted `correct-the-record` verdict
//! could not be applied to the sentence it corrected, because that
//! sentence lives in a completed step's metadata — so the correction
//! had to be recorded on the job, beside the claim rather than on it.
//!
//! THE OPEN QUESTION THE PACKET ASKED, now answered: is any metadata on
//! a completed step legitimately mutable? No. The conductor's
//! `boarded_head` stamp — the one path that visibly works against
//! finished cars — writes JOB metadata via `merge_job_metadata`, not
//! step metadata (train.rs: `car.get("metadata")`). So the frozen set
//! can be refused wholesale.
//!
//! WHO REFUSES WHAT. The `status` half was already fixed, by a check
//! further down the same handler added for this defect (job 903e6b90).
//! This change covers only `completed_on` and `metadata`, the two
//! fields still being dropped in silence — and deliberately leaves
//! `status` alone so the existing, more specific message survives.
//! `a_demotion_still_gets_the_specific_message` pins that boundary.
//!
//! WHY NOT REFUSE EVERY WRITE TO A TERMINAL STEP. Because the freeze
//! exists to make racing writers harmless: dispatcher assign retries
//! and JetStream redeliveries re-PUT content that is already stored.
//! Those are no-ops today and must stay 204 — turning them into 409s
//! would convert a harmless redelivery into a loud failure. The refusal
//! is therefore scoped to a write that would actually CHANGE a frozen
//! field.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
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
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn step(id: &str, status: StepStatus, metadata: serde_json::Value) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
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
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "incident".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "The CI runner lost outbound network".into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 18).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    };
    jobs.create_job(&job).await.unwrap();
    jobs.add_step(&step(
        DONE_STEP,
        StepStatus::Completed,
        serde_json::json!({ "finding": "the original sentence" }),
    ))
    .await
    .unwrap();
    jobs.add_step(&step(
        LIVE_STEP,
        StepStatus::Ready,
        serde_json::json!({ "finding": "still editable" }),
    ))
    .await
    .unwrap();
    (app, jobs)
}

async fn put_step(app: &Router, step_id: &str, body: &str) -> axum::http::Response<Body> {
    app.clone()
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
        .unwrap()
}

async fn body_of(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// THE BUG. A metadata change against a completed step must not be
/// answered with success and then dropped.
#[tokio::test]
async fn a_metadata_change_on_a_completed_step_is_refused() {
    let (app, jobs) = seed().await;

    let resp = put_step(
        &app,
        DONE_STEP,
        r#"{"metadata":{"finding":"the original sentence","correction":"this claim is false"}}"#,
    )
    .await;

    let status = resp.status();
    let body = body_of(resp).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write that would change frozen state on a completed step must be REFUSED. \
         Returning success and discarding it is what let three cars be reported repaired \
         while their receipts were never written (09576fab).\nbody: {body}"
    );

    // And the row is untouched either way — the refusal must not be a
    // partial write.
    let stored = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(DONE_STEP).unwrap()))
        .await
        .unwrap()
        .expect("step exists");
    assert_eq!(
        stored.metadata,
        serde_json::json!({ "finding": "the original sentence" }),
        "the stored metadata must be unchanged"
    );
}

/// The refusal has to say WHICH fields it refused, or the caller learns
/// only that something was wrong — the same blindness in a new colour.
#[tokio::test]
async fn the_refusal_names_the_frozen_field() {
    let (app, _jobs) = seed().await;

    let resp = put_step(&app, DONE_STEP, r#"{"metadata":{"correction":"x"}}"#).await;
    let body = body_of(resp).await;

    assert!(
        body.contains("metadata"),
        "the 409 must name the frozen field the caller tried to change.\nbody: {body}"
    );
}

/// THE HALF THAT IS EASY TO BREAK BY OVERREACHING. The freeze exists so
/// that racing writers are harmless: a dispatcher retry or a JetStream
/// redelivery re-PUTs content already stored. Those must stay 204.
#[tokio::test]
async fn a_redelivery_that_changes_nothing_still_succeeds() {
    let (app, _jobs) = seed().await;

    let resp = put_step(
        &app,
        DONE_STEP,
        r#"{"metadata":{"finding":"the original sentence"},"status":"completed"}"#,
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "a write that changes no frozen field is a harmless redelivery and must not 409 — \
         turning retries into failures would trade a silent bug for a noisy one"
    );
}

/// THE SIBLING GUARD KEEPS ITS CASE.
///
/// A demotion against a terminal step was already refused before this
/// change, by a check further down the handler added for this same
/// defect (job 903e6b90). Its message is better than a generic frozen-
/// field list: it names the status the step is and the status the
/// caller tried to set. The frozen-field check runs EARLIER in the
/// handler, so if it also claimed `status` it would preempt that
/// message with a vaguer one — a silent downgrade nobody would notice,
/// because both answers are still 409.
#[tokio::test]
async fn a_demotion_still_gets_the_specific_message() {
    let (app, _jobs) = seed().await;

    let resp = put_step(&app, DONE_STEP, r#"{"status":"ready"}"#).await;
    let status = resp.status();
    let body = body_of(resp).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.contains("does not move backwards"),
        "a status demotion must keep the specific refusal that names both statuses, not fall \
         into the generic frozen-field list.\nbody: {body}"
    );
    assert!(
        body.contains("completed") && body.contains("ready"),
        "and that message names where the step is and where the caller tried to put it.\n\
         body: {body}"
    );
}

/// A live step is untouched by any of this.
#[tokio::test]
async fn a_live_step_still_accepts_metadata() {
    let (app, jobs) = seed().await;

    let resp = put_step(&app, LIVE_STEP, r#"{"metadata":{"finding":"edited"}}"#).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let stored = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(LIVE_STEP).unwrap()))
        .await
        .unwrap()
        .expect("step exists");
    assert_eq!(stored.metadata, serde_json::json!({ "finding": "edited" }));
}
