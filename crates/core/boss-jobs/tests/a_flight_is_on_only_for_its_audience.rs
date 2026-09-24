//! `GET /api/flights/mine` — the flights read, end to end over the
//! shipped `flight-a-change` row (design c4c2a607, backlog 73c31776
//! car 1).
//!
//! A flight is an open packet carrying a `flight` block, and its state
//! is its completed steps read against the protocol row it is pinned
//! to. This drives one packet of the BUNDLED row through the jobs API
//! the way an agent and David would — file, ship, turn on for David,
//! record a reading, widen, pull — and asks the read, as each viewer,
//! after every move. So the row and the read are proven together: a
//! reshaped row that stops declaring a state fails here, not on
//! David's screen.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus, Priority, Step, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::platform_bundle_path;
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "flight-a-change";
const CODE: &str = "it-map-motion";

fn user(id: &str, role: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

fn agent() -> String {
    user("agent-claude", "platform-admin")
}

fn david() -> String {
    user("emp-david", "platform-admin")
}

fn app() -> (Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    let row = load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == KIND)
        .expect("flight-a-change ships in the platform bundle");
    kinds.seed(row).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let policy = FakePolicyClient::builder()
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
        .build();
    let policy: Arc<dyn PolicyClient> = Arc::new(policy);
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, json)
}

async fn file(app: &Router, jobs: &InMemoryJobs, audience: Value) -> Job {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", CODE),
        "Flight: continuous motion on the IT map",
        "emp-david",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
    );
    // Job::new is a draft; a flight is an OPEN packet.
    job.status = JobStatus::Open;
    job.metadata = json!({"flight": {
        "code": CODE,
        "hypothesis": "motion makes a stalled region findable in under five seconds",
        "signal": "David's recorded reading, and the troubled regions he named unprompted",
        "audience": audience,
        "owner": "agent-claude",
        "observe_days": 7,
    }});
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", agent())
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap();
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
    jobs.get_job(&job_id).await.unwrap().unwrap()
}

async fn step(jobs: &InMemoryJobs, job: &Job, slug: &str) -> Step {
    jobs.list_steps(&job.id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step {slug}"))
}

/// Complete `slug` as `who`, with `fields` laid over its stored
/// metadata (the step PUT refuses a body that drops a stored key).
async fn complete(
    app: &Router,
    jobs: &InMemoryJobs,
    job: &Job,
    slug: &str,
    who: &str,
    fields: Value,
) {
    let s = step(jobs, job, slug).await;
    let mut md = s.metadata.clone();
    if let (Some(m), Some(f)) = (md.as_object_mut(), fields.as_object()) {
        m.extend(f.clone());
    }
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", s.job_id, s.id))
                .header("content-type", "application/json")
                .header("x-boss-user", who)
                .body(Body::from(
                    json!({"status": "completed", "metadata": md}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert!(status.is_success(), "completing `{slug}`: {status} {body}");
}

async fn mine(app: &Router, who: &str) -> Vec<String> {
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/flights/mine")
                .header("x-boss-user", who)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["flights"]
        .as_array()
        .unwrap_or_else(|| panic!("no flights array: {body}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// THE CLAIM, as the design's first flight will run: filed and shipped
/// it is off for everyone; turned on it is on for David and nobody
/// else; widened by David it is on for everyone; pulled it is off for
/// everyone, for good.
#[tokio::test]
async fn a_flight_is_on_for_its_audience_and_off_everywhere_else() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, json!({"actors": ["emp-david"]})).await;
    let other = user("emp-ops", "operator");
    let none: Vec<String> = vec![];
    let on = vec![CODE.to_string()];

    assert_eq!(mine(&app, &david()).await, none, "filed, not shipped: off");
    complete(&app, &jobs, &job, "ship", &agent(), json!({"car": "c1"})).await;
    assert_eq!(mine(&app, &david()).await, none, "shipped: still off");

    complete(
        &app,
        &jobs,
        &job,
        "turn-on",
        &agent(),
        json!({"turned_on_for": "emp-david"}),
    )
    .await;
    assert_eq!(mine(&app, &david()).await, on, "turned on: on for David");
    assert_eq!(mine(&app, &other).await, none, "and for nobody else");
    assert_eq!(mine(&app, &agent()).await, none, "not even its owner");

    complete(
        &app,
        &jobs,
        &job,
        "observe",
        &agent(),
        json!({"reading": "found the stalled region in 3s", "observed_days": "7"}),
    )
    .await;
    complete(
        &app,
        &jobs,
        &job,
        "widen",
        &david(),
        json!({"why": "it works"}),
    )
    .await;
    assert_eq!(mine(&app, &other).await, on, "widened by David: everyone");

    complete(
        &app,
        &jobs,
        &job,
        "pull",
        &agent(),
        json!({"reason": "regressed"}),
    )
    .await;
    assert_eq!(mine(&app, &david()).await, none, "pulled: off for David");
    assert_eq!(mine(&app, &other).await, none, "and for everyone");
}

/// A role in the audience reaches every holder of it, and the answer
/// names only codes — never another flight's audience.
#[tokio::test]
async fn a_role_audience_reaches_its_holders_and_the_answer_is_codes_only() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, json!({"roles": ["platform-admin"]})).await;
    complete(&app, &jobs, &job, "ship", &agent(), json!({"car": "c1"})).await;
    complete(
        &app,
        &jobs,
        &job,
        "turn-on",
        &agent(),
        json!({"turned_on_for": "platform-admin"}),
    )
    .await;
    assert_eq!(mine(&app, &agent()).await, vec![CODE.to_string()]);
    assert!(mine(&app, &user("emp-ops", "operator")).await.is_empty());
}

/// No identity at all is nobody's audience: the read answers, and it
/// answers off.
#[tokio::test]
async fn an_unidentified_caller_is_in_no_audience() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, json!({"actors": ["emp-david"]})).await;
    complete(&app, &jobs, &job, "ship", &agent(), json!({"car": "c1"})).await;
    complete(
        &app,
        &jobs,
        &job,
        "turn-on",
        &agent(),
        json!({"turned_on_for": "emp-david"}),
    )
    .await;
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/flights/mine")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["flights"], json!([]), "{body}");
}
