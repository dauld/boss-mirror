//! Assigning a step emits the fact, so the assignee gets told.
//!
//! `messages.notify` routes on step events, and the only trigger used
//! to be `step.ready.<kind>` — fired at the READY transition. The
//! common flow is the other order: a step materializes ready and
//! unassigned, and someone picks it up afterwards. That assignment
//! changed state and emitted nothing, so the assignee was never told
//! (backlog 534a8dc8). Two halves, both pinned here:
//!
//!  - assigning a step emits `step.assigned.<kind>` carrying the
//!    assignee — but only when the assignee actually CHANGED, so a
//!    metadata PATCH doesn't re-notify;
//!  - the `step.ready.<kind>` payload carries `assignee_id`, so a step
//!    that was assigned BEFORE it became ready notifies the assignee
//!    rather than the role's on-call member.
//!
//! Double-notification is collapsed downstream by the handler's
//! deterministic message id (`notify:{step}:{recipient}`, ON CONFLICT
//! DO NOTHING) — same person, one inbox row.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
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
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), jobs)
}

/// Persisted events of `kind` (the outbox path — step lifecycle events
/// ride persistence, not the live bus; the relay forwards them to NATS
/// in production).
fn events_of<'a>(
    events: &'a [boss_core::event::Event],
    kind: &str,
) -> Vec<&'a boss_core::event::Event> {
    events.iter().filter(|e| e.kind == kind).collect()
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

/// Open a ship-a-change Job and return (job_id, steps).
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
    // The create response is the Job row; steps come from the read.
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

#[tokio::test]
async fn assigning_an_already_ready_step_emits_the_assignment() {
    let (app, jobs) = app_with_bus();
    let (job_id, steps) = open_job(&app).await;
    // `scope` promotes to ready at materialization (trigger fires).
    let scope = step_by_slug(&steps, "scope");
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    let sid = scope["id"].as_str().unwrap();

    let (status, _) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{sid}"),
            serde_json::json!({"assignee_id": "emp-42"}),
        ),
    )
    .await;
    assert!(status.is_success(), "step update: {status}");

    let recorded = jobs.recorded_events();
    let assigned = events_of(&recorded, "step.assigned.task");
    assert_eq!(
        assigned.len(),
        1,
        "one assignment, one event; got kinds: {:?}",
        recorded.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>()
    );
    let ev = assigned[0];
    assert_eq!(ev.payload["assignee_id"], "emp-42");
    assert_eq!(ev.payload["job_id"], job_id);
    assert_eq!(ev.payload["step_id"], sid);

    // Re-sending the same assignee is not an assignment — a metadata
    // PATCH must not re-notify.
    let (status, _) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{sid}"),
            serde_json::json!({"assignee_id": "emp-42", "notes": "still mine"}),
        ),
    )
    .await;
    assert!(status.is_success(), "step update: {status}");
    assert_eq!(
        events_of(&jobs.recorded_events(), "step.assigned.task").len(),
        1,
        "the unchanged-assignee PATCH must not emit a second assignment"
    );
}

#[tokio::test]
async fn a_step_assigned_before_it_becomes_ready_carries_the_assignee_on_ready() {
    let (app, jobs) = app_with_bus();
    let (job_id, steps) = open_job(&app).await;
    let scope = step_by_slug(&steps, "scope");
    let build = step_by_slug(&steps, "build");
    assert_eq!(
        build["status"], "pending",
        "precondition: build not yet ready"
    );
    let build_id = build["id"].as_str().unwrap();

    // Assign the still-pending build step…
    let (status, _) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{build_id}"),
            serde_json::json!({"assignee_id": "emp-77"}),
        ),
    )
    .await;
    assert!(status.is_success(), "step update: {status}");

    // …then complete scope, which promotes build to ready.
    let scope_id = scope["id"].as_str().unwrap();
    let (status, _) = send(
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
    assert!(status.is_success(), "step update: {status}");

    let recorded = jobs.recorded_events();
    let ready: Vec<_> = events_of(&recorded, "step.ready.task")
        .into_iter()
        .filter(|e| e.payload["step_id"] == build_id)
        .collect();
    assert_eq!(ready.len(), 1, "build promoted to ready exactly once");
    assert_eq!(
        ready[0].payload["assignee_id"], "emp-77",
        "the ready event must name the assignee so notify routes to THEM, \
         not the role's on-call member"
    );
}
