//! A terminal step refuses a backwards transition instead of ignoring it.
//!
//! `update_step_at`'s UPDATE freezes status, completed_on and metadata
//! once a step is Completed or Skipped — deliberately, so a write
//! computed against a pre-completion fetch (dispatcher assign retries,
//! JetStream redeliveries, any racing read-modify-write) cannot demote
//! it. That invariant is correct and this test does not challenge it.
//!
//! What it pins is that the caller is TOLD. Before job 903e6b90 the
//! handler returned 204 and the columns simply did not move, so an
//! actor that believed it had recorded something had not — and the
//! same silence ate a correction to a car's build step earlier the
//! same day, discovered only when the record was read back.
//!
//! The distinction that keeps the guard usable: this compares VALUES,
//! not intent. A redelivery re-completing an already-completed step
//! sends the same status, changes nothing, and must still succeed.
//! Only a DIFFERENT status against a terminal row is a real conflict.

use std::sync::Arc;

use async_trait::async_trait;
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

/// A Workflow with no DECLARED terminal: its one step completing
/// leaves every step terminal, so the Job closes through the
/// `compute_job_status` catch-all. That is the emit site under test —
/// none of the platform kinds can reach it, because they all declare
/// outcomes.
fn catch_all_spec() -> boss_jobs::registry::WorkflowSpec {
    let mut spec = boss_jobs::registry::WorkflowSpec::platform_seed(
        "closes-by-catch-all",
        "Closes by catch-all",
        "platform",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "The only work".into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

struct AdminRoster;

#[async_trait]
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

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The router plus the in-memory jobs adapter, kept so the test can
/// read back the events the outbox paths recorded.
fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    kinds
        .seed(catch_all_spec())
        .expect("seed the catch-all kind");
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
                Resource::job(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
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
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn get_job(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, job) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {job}");
    job
}

async fn open_and_complete_first_step(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": "closes-by-catch-all",
                    "subject": { "subject_kind": "custom", "id": "terminal-refusal" },
                    "title": "a packet whose step will be completed",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "open failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    let full = get_job(app, &job_id).await;
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["status"] == "ready")
        .expect("a ready step to complete");
    let step_id = step["id"].as_str().expect("step id").to_string();

    let (status, body) = send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(
        status.is_success(),
        "completing the step failed: {status} {body}"
    );
    (job_id, step_id)
}

#[tokio::test]
async fn a_completed_step_refuses_a_backwards_transition() {
    let (app, _jobs) = app();
    let (job_id, step_id) = open_and_complete_first_step(&app).await;

    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "ready" }).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a backwards transition must be refused, not ignored: {body}"
    );
    let text = body.as_str().unwrap_or_default();
    assert!(
        text.contains("completed") && text.contains("ready"),
        "the refusal must name both states so the caller can act: {text}"
    );

    // And the step really is untouched.
    let full = get_job(&app, &job_id).await;
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step_id.as_str())
        .expect("the step");
    assert_eq!(step["status"], "completed");
}

#[tokio::test]
async fn re_completing_a_completed_step_still_succeeds() {
    // The idempotency the guard must not break: a redelivered
    // completion sends the same status, changes nothing, and is not a
    // conflict. Refusing it would turn every at-least-once retry into
    // a dead letter.
    let (app, _jobs) = app();
    let (job_id, step_id) = open_and_complete_first_step(&app).await;

    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(
        status.is_success(),
        "an idempotent re-completion must not be refused: {status} {body}"
    );
}
