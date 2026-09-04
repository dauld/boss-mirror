//! End-to-end coverage for the `workflow-publish` StepType
//! dispatch path.
//!
//! When a step of kind `workflow-publish` flips to Done via PUT
//! /api/jobs/{id}/steps/{step_id}, the handler must:
//! 1. Pull `workflow_spec` from the step metadata.
//! 2. Hand it to the registry, which gates it on viability.
//! 3. Call `WorkflowRegistry::publish_authored(spec, job_id, actor, now)`
//!    — the registry records `jobs.kind.published` (full published
//!    spec) atomically with the workflows row; the step path no
//!    longer emits its own copy.
//! 4. Persist STEP_UPDATED only AFTER the registry write succeeds.
//!
//! Decision record: `docs/architecture-decisions.md` §Jobs,
//! Workflows, Steps (Workflows bootstrap through Jobs).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::events::WORKFLOW_PUBLISHED;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn cto() -> User {
    User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("executive".into()),
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn build_app(
    kinds: Arc<dyn WorkflowRegistry>,
) -> (Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher,
        step_registry,
        policy,
        kind_registry: Some(kinds),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs, bus)
}

async fn seed_publish_step(
    jobs: &dyn JobsRepository,
    metadata: serde_json::Value,
) -> (JobId, StepId) {
    use boss_core::job::Job as JobRow;
    let mut job = JobRow::new(
        "workflow-design",
        Subject::new("workflow", "morning-brew"),
        "Design morning-brew",
        "emp-cto",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    let job_id = job.id;
    jobs.create_job(&job).await.unwrap();

    // Single active step that will flip to Done in the test.
    let step = Step {
        id: StepId::new(),
        job_id,
        kind: "workflow-publish".into(),
        title: "Publish".into(),
        spec_slug: None,
        assignee_id: None,
        status: StepStatus::Active,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        metadata,
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    };
    let step_id = step.id;
    jobs.add_step(&step).await.unwrap();
    (job_id, step_id)
}

fn valid_spec(kind: &str) -> WorkflowSpec {
    // Must pass the viability gate `publish_authored` enforces:
    // a viable trigger → terminal pair.
    WorkflowSpec::platform_seed(
        kind,
        "Morning Brew",
        "production",
        vec!["location".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "brewed".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

async fn put_step_done(
    app: &Router,
    job_id: JobId,
    step_id: StepId,
    user_json: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job_id, step_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_json)
                .body(Body::from(json!({ "status":"completed" }).to_string()))
                .unwrap(),
        )
        .await
        .expect("router responds")
}

#[tokio::test]
async fn done_dispatches_publish_authored_and_emits_kind_published_event() {
    // Concrete handle: `recorded_events()` is the InMemory window
    // onto what the Pg adapter records in the row transaction.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "workflow_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    let status = resp.status();
    assert!(
        status.is_success(),
        "PUT step → done must succeed, got {status}"
    );

    // Registry now has the published kind, with the meta-Job's id
    // recorded as authoring_job_id.
    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(live.kind, "morning-brew");
    assert_eq!(live.version, 1);
    assert_eq!(live.status, WorkflowStatus::Active);
    assert_eq!(
        live.authoring_job_id.expect("authoring stamped"),
        *job_id.inner().as_uuid(),
    );

    // The audit-bearing event landed — recorded by the REGISTRY
    // adapter atomically with the workflows row (registry-events
    // car), no longer pushed into the step-update write. The
    // in-memory registry collects what the Pg adapter records
    // in-tx.
    let events = kinds.recorded_events();
    let published: Vec<_> = events
        .iter()
        .filter(|e| e.kind == WORKFLOW_PUBLISHED)
        .collect();
    assert_eq!(
        published.len(),
        1,
        "exactly one jobs.kind.published event should fire"
    );
    let payload = &published[0].payload;
    assert_eq!(payload["kind"], "morning-brew");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["status"], "active");
    // The actor is the session user who flipped the step.
    assert_eq!(payload["_actor"], "emp-cto");

    // The step path must NOT duplicate it — one write, one event.
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED),
        "the step-update write no longer carries jobs.kind.published"
    );
}

#[tokio::test]
async fn missing_workflow_spec_metadata_returns_400_no_publish() {
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let (job_id, step_id) =
        seed_publish_step(jobs.as_ref(), json!({ "previous_kind_version": 0 })).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "missing workflow_spec must abort the step write"
    );

    // No publish event should have recorded — neither by the
    // registry (the write never happened) nor by the step path.
    assert!(
        kinds.recorded_events().is_empty(),
        "the registry must record nothing when dispatch fails"
    );
    let events = jobs.recorded_events();
    assert!(
        events.iter().all(|e| e.kind != WORKFLOW_PUBLISHED),
        "no jobs.kind.published event must fire when dispatch fails"
    );

    // STEP_UPDATED must NOT have landed — the dispatch fails before
    // update_step_at is called, preserving audit_log integrity.
    let updated_count = events
        .iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .count();
    assert_eq!(
        updated_count, 0,
        "STEP_UPDATED must not be emitted when dispatch aborts"
    );

    // Registry untouched.
    assert!(
        kinds.list_active(None).await.unwrap().is_empty(),
        "registry must stay empty when dispatch aborts"
    );
}

#[tokio::test]
async fn malformed_workflow_spec_returns_400() {
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let (job_id, step_id) = seed_publish_step(
        jobs.as_ref(),
        json!({ "workflow_spec": "not even an object" }),
    )
    .await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    assert!(kinds.recorded_events().is_empty());
    let events = jobs.recorded_events();
    assert!(events.iter().all(|e| e.kind != WORKFLOW_PUBLISHED));
}

#[tokio::test]
async fn unviable_workflow_spec_returns_422_and_publishes_nothing() {
    // The Step dispatch path sets a registry row ACTIVE without a
    // draft ever existing, so it answers to the publish gate too
    // (2026-08-13). The refusal is 422 with the lint problems, and
    // the step must NOT flip to done behind a failed registry write.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    // Viable shape minus the outcome — the incident's exact defect.
    let mut spec = valid_spec("morning-brew");
    spec.steps[1].terminal = None;
    let metadata = json!({ "workflow_spec": serde_json::to_value(&spec).unwrap() });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    assert!(
        kinds.get_active("morning-brew").await.is_err(),
        "an unviable spec must not reach the active slot by any path"
    );
    assert!(kinds.recorded_events().is_empty());
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED)
    );
}

#[tokio::test]
async fn publish_step_without_kind_registry_returns_503() {
    // Mirror prod's degraded mode: the registry handle is unset.
    // Dispatch should refuse rather than silently no-op so the
    // operator notices the misconfiguration.
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    let app = router(state);

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "workflow_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// Silence unused import warnings when this file is the only one
// touching these names.
#[allow(dead_code)]
fn _ensure_uuid_used(_id: Uuid) {}
