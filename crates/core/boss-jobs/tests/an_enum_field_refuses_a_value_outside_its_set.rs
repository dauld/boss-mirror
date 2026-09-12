//! An enum field refuses a value outside its set — at completion, where
//! required-ness is already checked.
//!
//! THE DEFECT (backlog item cb9661fe, defect 2). A backlog-item's
//! "Decide the design" step is an `answer-question` step that authored
//! no fields of its own, so at completion only the kind bundle's fields
//! applied — and the bundle declares `verdict` as a bare `string`, with
//! `approved | declined | answered` living in the description where no
//! validator reads it. A field-discovery probe that PUT
//! `verdict = "__probe__"` got 204 and completed a live design-review
//! step (a305385b). The `approval` workflow, which authors the same
//! enum inline on its decide step, was already refused correctly; the
//! leak was the declaration, not the validator.
//!
//! WHY THE FIX IS ON THE WORKFLOW, NOT THE BUNDLE. The bundle is one
//! unversioned `Arc<StepRegistry>`: tightening `verdict` there would
//! retighten every in-flight answer-question step at the next restart,
//! which is exactly what steptype-bundle-ratchet refuses. So the
//! vocabulary is authored on backlog-item's and user-feedback's
//! design-review steps — the versioned path, pinned per packet.
//!
//! This test drives the real handler over the in-memory adapter with a
//! design-review step carrying the contract the bundle now materializes
//! (its authored `fields`, read from the bundle itself), so what is
//! under test is the shipped protocol data through the shipped handler.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{
    Job, JobId, JobStatus, Priority, Step, StepField, StepId, StepStatus, Subject,
};
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
const DESIGN_REVIEW: &str = "00000000-0000-0000-0000-0000000000b1";
const FREE_TEXT: &str = "00000000-0000-0000-0000-0000000000b2";

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

/// The authored completion contract of `workflow`'s design-review
/// step, read from the platform bundle a deployment actually seeds.
fn design_review_fields(workflow: &str) -> Vec<StepField> {
    boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == workflow)
        .unwrap_or_else(|| panic!("the bundle carries {workflow}"))
        .steps
        .into_iter()
        .find(|s| s.title == "design-review")
        .unwrap_or_else(|| panic!("{workflow} has a design-review step"))
        .fields
}

fn step(id: &str, kind: &str, fields: Vec<StepField>) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: kind.into(),
        title: "Decide the design".into(),
        spec_slug: Some("design-review".into()),
        assignee_id: Some("emp-op".into()),
        status: StepStatus::Ready,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields,
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let (app, jobs) = build_app();
    let job = Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 3,
        subject: Subject::new("custom", "boss-jobs"),
        title: "answer-question verdict enum not enforced".into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    };
    jobs.create_job(&job).await.unwrap();
    // The step as backlog-item now materializes it: the authored
    // fields come from the shipped bundle, not a hand-written copy.
    jobs.add_step(&step(
        DESIGN_REVIEW,
        "answer-question",
        design_review_fields("backlog-item"),
    ))
    .await
    .unwrap();
    // A step whose only contract is a free-text authored field.
    jobs.add_step(&step(
        FREE_TEXT,
        "task",
        vec![StepField {
            name: "finding".into(),
            field_type: "string".into(),
            required: true,
            filled_by: Default::default(),
            item_keys: Vec::new(),
            covers: None,
        }],
    ))
    .await
    .unwrap();
    (app, jobs)
}

async fn complete(
    app: &Router,
    step_id: &str,
    metadata: serde_json::Value,
) -> (StatusCode, String) {
    let body = serde_json::json!({ "status": "completed", "metadata": metadata });
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

async fn status_of(jobs: &InMemoryJobs, step_id: &str) -> StepStatus {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(step_id).unwrap()))
        .await
        .unwrap()
        .expect("step exists")
        .status
}

/// THE BUG. The probe that completed a live step must be a 400 that
/// says which field, which value, and what would have been accepted —
/// and the step must still be open afterwards.
#[tokio::test]
async fn a_verdict_outside_the_set_is_refused_at_completion() {
    let (app, jobs) = seed().await;
    let (status, body) = complete(
        &app,
        DESIGN_REVIEW,
        serde_json::json!({ "verdict": "__probe__", "answer": "a placeholder" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    for needle in ["verdict", "'__probe__'", "approved", "declined", "answered"] {
        assert!(
            body.contains(needle),
            "the refusal names the field, the value, and the allowed set — missing `{needle}`: {body}"
        );
    }
    assert_eq!(
        status_of(&jobs, DESIGN_REVIEW).await,
        StepStatus::Ready,
        "a refused completion leaves the step open"
    );
}

#[tokio::test]
async fn a_verdict_in_the_set_completes_the_step() {
    let (app, jobs) = seed().await;
    let (status, body) = complete(
        &app,
        DESIGN_REVIEW,
        serde_json::json!({ "verdict": "approved", "answer": "ship it" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(status_of(&jobs, DESIGN_REVIEW).await, StepStatus::Completed);
}

/// A plain `string` field is not an enum; any string still completes it.
#[tokio::test]
async fn a_free_text_field_still_takes_any_string() {
    let (app, jobs) = seed().await;
    let (status, body) = complete(
        &app,
        FREE_TEXT,
        serde_json::json!({ "finding": "__probe__" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(status_of(&jobs, FREE_TEXT).await, StepStatus::Completed);
}
