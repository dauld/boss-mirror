//! A `step.ready.<kind>` marker says WHICH workflow's step became
//! ready, and WHICH step of it.
//!
//! MEASURED (backlog 4d53fae2, left by the builder of 89c95245 on
//! 2026-09-19). `step.done.<kind>` hoists `workflow_kind` and
//! `spec_slug` to the payload root; `step.ready.<kind>` carried
//! job_id, step_id, kind, the subject pair, assignee_id and the
//! step's own metadata — and nothing that names the packet kind. The
//! shared `step.ready.task` topic carries every task step on the
//! board, so a rule that wants one workflow's one step had no `when`
//! it could write: both handlers written that day
//! (`ops.file_tag_release`, `maintenance.chore.file_reds`) exist to
//! fetch the parent Job for the kind the payload should have carried.
//! CLAUDE.md §9 calls that a registry-over-code leak.
//!
//! BOTH ALWAYS PRESENT, for the reason `step.done` gives: the
//! dispatcher's expr binder resolves flat top-level identifiers only,
//! and an absent identifier is a PredicateFailed → Retry →
//! dead-letter storm, not a quiet false. `spec_slug` is `""` for a
//! step with none (ad-hoc, or materialized before the column
//! existed); `workflow_kind` is `""` only when the Job could not be
//! read, like the subject fields beside it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn app_with_bus() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
    (status, json)
}

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", ADMIN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn open_job(app: &axum::Router) -> (String, Vec<serde_json::Value>) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "job create failed with {status}: {job}"
    );
    let id = job["id"].as_str().expect("job id").to_string();
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let steps = full["steps"].as_array().expect("steps").clone();
    (id, steps)
}

fn step_by_slug<'a>(steps: &'a [serde_json::Value], slug: &str) -> &'a serde_json::Value {
    steps
        .iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("step {slug} present"))
}

fn ready_for<'a>(
    events: &'a [boss_core::event::Event],
    step_id: &str,
) -> &'a boss_core::event::Event {
    let found: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "step.ready.task" && e.payload["step_id"] == step_id)
        .collect();
    assert_eq!(found.len(), 1, "exactly one ready marker for {step_id}");
    found[0]
}

/// The materialization path: steps ready the moment the packet opens.
#[tokio::test]
async fn a_step_ready_at_materialization_names_its_workflow_and_slug() {
    let (app, jobs) = app_with_bus();
    let (_job_id, steps) = open_job(&app).await;
    let scope = step_by_slug(&steps, "scope");
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    let scope_id = scope["id"].as_str().expect("step id");

    let recorded = jobs.recorded_events();
    let ev = ready_for(&recorded, scope_id);
    assert_eq!(
        ev.payload["workflow_kind"], "ship-a-change",
        "the ready marker must name the packet kind, so a rule on the \
         shared step.ready.task topic can select one workflow without \
         fetching the Job in a handler"
    );
    assert_eq!(
        ev.payload["spec_slug"], "scope",
        "and WHICH step of it — every ship-a-change step is kind=task"
    );
}

/// The re-evaluation path: a step promoted when its predecessor
/// completed. Same emitter, so the same two fields — pinned because
/// this is the path a filing rule actually rides.
#[tokio::test]
async fn a_step_promoted_by_a_completion_names_its_workflow_and_slug() {
    let (app, jobs) = app_with_bus();
    let (job_id, steps) = open_job(&app).await;
    let scope = step_by_slug(&steps, "scope");
    let build = step_by_slug(&steps, "build");
    assert_eq!(
        build["status"], "pending",
        "precondition: build not yet ready"
    );
    let build_id = build["id"].as_str().expect("step id");

    let scope_id = scope["id"].as_str().expect("step id");
    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            serde_json::json!({
                "status": "completed",
                "metadata": {"summary": "s", "excludes": "e",
                             "authority_role": "platform-admin"},
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "step update: {status}: {body}");

    let recorded = jobs.recorded_events();
    let ev = ready_for(&recorded, build_id);
    assert_eq!(ev.payload["workflow_kind"], "ship-a-change");
    assert_eq!(ev.payload["spec_slug"], "build");
}
