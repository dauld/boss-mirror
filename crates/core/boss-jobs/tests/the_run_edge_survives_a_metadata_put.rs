//! The run edge survives a wholesale metadata PUT (backlog b91a2103).
//!
//! `boss dispatch` writes `agent_run` onto the step it CLAIMS, and the
//! `agent-run-delivers-when-its-step-is-done` rule follows that edge
//! from `step.done.<kind>` to complete the run's `building` step with
//! `result = delivered` (backlog dd6d44b7). The edge is therefore the
//! only thing that makes an analyst run land: a run that ships no car
//! has no gate-run and no parked car to land on.
//!
//! `PUT /api/jobs/{id}/steps/{step_id}` replaces top-level metadata
//! WHOLESALE, so a completer that sends `metadata` without first
//! reading and merging erased the edge. The failure was silent and
//! delayed: the completion succeeded, the work was recorded correctly,
//! and the run then never delivered and died on the four-hour silence
//! clock as though the agent had gone quiet. ~94 analyst runs are
//! queued behind the page march, each one exposed to it.
//!
//! So the edge joins `authority_role` and `human_only` as a key the
//! PUT carries forward — carried for the same reason, that losing it
//! breaks something invisible. It is NOT immutable like
//! `authority_role`: the merge door (`PATCH .../metadata`) still
//! overwrites it and still deletes it with an explicit `null`, which
//! is what makes a re-claim by a different run correct — `boss
//! dispatch` writes the new run's id through that door right after the
//! claim, so a carried-forward value can never outlive the next
//! dispatch.

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

const ADMIN_ID: &str = "emp-bootstrap-admin";
const RUN_A: &str = "5b1d2c3e-0000-4000-8000-00000000000a";
const RUN_B: &str = "5b1d2c3e-0000-4000-8000-00000000000b";

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![ADMIN_ID.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == ADMIN_ID)
    }
}

fn app() -> axum::Router {
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
        cadence: None,
        delivery: None,
        dispatcher_firings: None,
        agent_budget: None,
    };
    router(state)
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

/// A ship-a-change packet and its ready `scope` step — the shape a
/// dispatched run claims.
async fn open_job(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": ADMIN_ID,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {status} {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("the scope step")
        .clone();
    assert_eq!(step["status"], "ready", "precondition: scope is ready");
    (id, step["id"].as_str().expect("step id").to_string())
}

/// The step's stored metadata, read back the way every consumer reads
/// it — through the API, not the adapter.
async fn stored_metadata(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step)
        .expect("the step")["metadata"]
        .clone()
}

/// The edge as `boss dispatch` writes it: the merge door, one key.
async fn write_edge(app: &axum::Router, job: &str, step: &str, run: &str) {
    let (status, body) = send(
        app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: run }),
        ),
    )
    .await;
    assert!(status.is_success(), "edge patch: {status} {body}");
}

/// THE DEFECT. A completer that sends `metadata` without merging used
/// to erase the edge, and the run behind it could never deliver.
#[tokio::test]
async fn a_wholesale_metadata_put_does_not_wipe_the_run_edge() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job}/steps/{step}"),
            // Exactly what a hand-built completion sends: the fields
            // the caller cares about, and no read-modify-write.
            serde_json::json!({
                "status": "completed",
                "metadata": {
                    "summary": "what the change does",
                    "excludes": "what it deliberately leaves alone",
                },
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "completing PUT: {status} {body}");

    let stored = stored_metadata(&app, &job, &step).await;
    assert_eq!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_A)),
        "the run edge is carried forward like authority_role and human_only: {stored}",
    );
    assert_eq!(
        stored.get("summary"),
        Some(&serde_json::json!("what the change does")),
        "and the body's own keys still land: {stored}",
    );
}

/// AND IT IS NOT IMMUTABLE. `authority_role` is stripped from the
/// merge door too, because a step's required sign-off authority is not
/// a caller's to change. The run edge is the opposite: a step
/// re-claimed by a different run must name the run that now holds it,
/// and `boss dispatch` writes that through this same door immediately
/// after the claim. So an overwrite lands, and an explicit `null`
/// clears — the carry-forward only ever survives OMISSION.
#[tokio::test]
async fn the_merge_door_still_overwrites_and_clears_the_run_edge() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    // A re-claim: the next dispatch names its own run.
    write_edge(&app, &job, &step, RUN_B).await;
    let stored = stored_metadata(&app, &job, &step).await;
    assert_eq!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_B)),
        "no stale run id is pinned onto a step a new run holds: {stored}",
    );

    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: serde_json::Value::Null }),
        ),
    )
    .await;
    assert!(status.is_success(), "clearing patch: {status} {body}");
    let stored = stored_metadata(&app, &job, &step).await;
    assert!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY).is_none(),
        "an explicit null still deletes the edge: {stored}",
    );
}
