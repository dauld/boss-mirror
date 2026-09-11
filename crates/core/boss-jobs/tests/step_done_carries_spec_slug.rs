//! `step.done.<kind>` carries the step's `spec_slug` as an
//! always-present top-level field, so the dispatcher's rule engine can
//! route on WHICH step of a kind completed.
//!
//! Every step of a `pr-train` is `kind = "task"`, so `step.done.task`
//! alone cannot tell `merged` from `deployed` from `ci`. The dispatcher
//! expr binder resolves only flat top-level identifiers in the payload,
//! so the discriminator has to be hoisted out of the (nested)
//! `metadata` and onto the payload root — as `spec_slug` — for a rule
//! like `spec_slug == "merged"` to match.
//!
//! Two halves, both pinned here, mirroring the `notify_on_done`
//! rationale one field over:
//!
//!  - a completed WORKFLOW step publishes its slug (Some branch);
//!  - a step with no slug publishes `""`, never null and never absent —
//!    an ABSENT identifier is a PredicateFailed → Retry → dead-letter
//!    storm in the binder, not a quiet false, so the field must be
//!    present on EVERY `step.done` event (None → "" branch).

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

/// `with_registry = false` leaves `kind_registry: None`, which both
/// skips the append-a-step guard and lets a job be opened under a kind
/// that has no spec — the only way to reach step completion with a step
/// whose `spec_slug` is genuinely `None`.
fn app(with_registry: bool) -> (axum::Router, Arc<InMemoryJobs>) {
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
    let kind_registry: Option<Arc<dyn WorkflowRegistry>> = if with_registry {
        let kinds = Arc::new(InMemoryWorkflows::new());
        for spec in seedable_platform_workflows() {
            kinds.seed(spec).expect("seed platform kind");
        }
        Some(kinds as Arc<dyn WorkflowRegistry>)
    } else {
        None
    };
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

/// Persisted events of `kind` (the outbox path — step lifecycle events
/// ride persistence, not the live bus).
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

/// A completed workflow step publishes its slug so a rule can route on
/// WHICH step of a kind completed. `scope` is a `task` step of the
/// `ship-a-change` workflow with a known slug.
#[tokio::test]
async fn a_completed_workflow_step_publishes_its_slug_on_step_done() {
    let (app, jobs) = app(true);

    let (status, job) = send(
        &app,
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
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    let (status, full) = send(
        &app,
        req("GET", &format!("/api/jobs/{job_id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let steps = full["steps"].as_array().expect("steps").clone();
    let scope = steps
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("scope step present");
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    let scope_id = scope["id"].as_str().unwrap();

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
    assert!(status.is_success(), "completing scope: {status}: {body}");

    let recorded = jobs.recorded_events();
    let done = events_of(&recorded, "step.done.task");
    let scope_done: Vec<_> = done
        .iter()
        .filter(|e| e.payload["step_id"] == scope_id)
        .collect();
    assert_eq!(
        scope_done.len(),
        1,
        "one step.done.task for scope; got kinds: {:?}",
        recorded.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(
        scope_done[0].payload["spec_slug"], "scope",
        "step.done must carry the slug so a rule can route on WHICH step \
         of a kind completed"
    );
}

/// A step with no slug publishes `""` — present, a string, never null
/// and never absent. An absent flat identifier dead-letters in the
/// dispatcher binder, so the field rides EVERY step.done event.
#[tokio::test]
async fn a_step_without_a_slug_publishes_an_empty_spec_slug() {
    // No kind registry: opens a job under a spec-less kind and skips the
    // append-a-step guard, the only route to completing a step whose
    // `spec_slug` is genuinely None.
    let (app, jobs) = app(false);

    let (status, job) = send(
        &app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "generic",
                "subject": {"subject_kind": "custom", "id": "no-spec"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    // An ad-hoc `generic` step — note NO `spec_slug` key, so it
    // deserializes to None. `generic` has no required-at-done fields.
    let step_id = uuid::Uuid::new_v4().to_string();
    let (status, body) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps"),
            serde_json::json!({
                "id": step_id,
                "job_id": job_id,
                "kind": "generic",
                "title": "an ad-hoc step with no spec slug",
                "status": "pending",
                "blocked_by": [],
                "sign_offs_required": [],
                "sign_offs": [],
                "fields": [],
                "metadata": {},
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "adding step: {status}: {body}");

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{step_id}"),
            serde_json::json!({"status": "completed"}),
        ),
    )
    .await;
    assert!(status.is_success(), "completing step: {status}: {body}");

    let recorded = jobs.recorded_events();
    let done = events_of(&recorded, "step.done.generic");
    assert_eq!(done.len(), 1, "one step.done.generic emitted");
    let payload = &done[0].payload;
    assert!(
        payload.get("spec_slug").is_some(),
        "spec_slug must be PRESENT on every step.done event — an absent \
         flat identifier dead-letters in the dispatcher binder: {payload}"
    );
    assert_eq!(
        payload["spec_slug"],
        serde_json::Value::String(String::new()),
        "a step with no slug must publish \"\", not null and not absent"
    );
}
