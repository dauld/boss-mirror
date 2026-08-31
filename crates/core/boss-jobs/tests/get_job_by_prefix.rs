//! `GET /api/jobs/{id}` resolves an unambiguous 8+ char id prefix.
//!
//! The whole system REFERS to a packet by its 8-char prefix — journals,
//! arrival reports, skip reasons, David's own messages — but only
//! `boss prove` could resolve one; `GET /api/jobs/44f121e4` answered
//! `400 invalid job id`, so every hand lookup fetched a list and
//! filtered client-side (backlog c2e6a703: 6 such lookups in one drain).
//!
//! The read path now accepts the id everyone already holds:
//!   - a full uuid resolves exactly as before (the fast path, untouched);
//!   - an unambiguous hex prefix (8..=36 chars) resolves to its one job;
//!   - a prefix matching nothing is 404 (the job is genuinely absent),
//!     distinct from a prefix matching more than one, which is 409 —
//!     the caller asked a question with two answers, and guessing which
//!     is exactly the wrong thing on a lookup that precedes a write;
//!   - true garbage stays 400, unchanged.
//!
//! WRITES ARE DELIBERATELY NOT WIDENED. A prefix that resolves to the
//! wrong job on a PUT/PATCH is a silently-corrupted packet; a write
//! holds the full id it just read back. This is the smallest true fix.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

// Two jobs share the 8-char prefix `aaaaaaaa`; one owns the unique
// prefix `bcbcbcbc`. Controlled ids make ambiguity deterministic —
// random v4 ids never collide at 8 chars in a test-sized set.
const AMBIG_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const AMBIG_B: &str = "aaaaaaaa-2222-4222-8222-222222222222";
const UNIQUE: &str = "bcbcbcbc-3333-4333-8333-333333333333";

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-op",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn job_at(id: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: title.into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

async fn seed() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        let _ = kinds.seed(spec);
    }
    let jobs = Arc::new(InMemoryJobs::new());
    for (id, title) in [
        (AMBIG_A, "first sharer"),
        (AMBIG_B, "second sharer"),
        (UNIQUE, "the unique one"),
    ] {
        jobs.create_job(&job_at(id, title)).await.unwrap();
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

async fn get(app: &axum::Router, id: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs/{id}"))
                .header("x-boss-user", admin_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

#[tokio::test]
async fn full_uuid_still_resolves() {
    let app = seed().await;
    let (status, body) = get(&app, UNIQUE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], UNIQUE);
}

#[tokio::test]
async fn unique_eight_char_prefix_resolves() {
    let app = seed().await;
    let (status, body) = get(&app, "bcbcbcbc").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], UNIQUE);
}

#[tokio::test]
async fn a_longer_hyphenated_prefix_resolves() {
    let app = seed().await;
    let (status, body) = get(&app, "bcbcbcbc-3333").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], UNIQUE);
}

#[tokio::test]
async fn a_prefix_matching_nothing_is_404() {
    let app = seed().await;
    let (status, _) = get(&app, "ffffffff").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_ambiguous_prefix_is_409_not_a_guess() {
    let app = seed().await;
    let (status, _) = get(&app, "aaaaaaaa").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn true_garbage_is_still_400() {
    let app = seed().await;
    // Non-hex characters — never any id's prefix.
    assert_eq!(
        get(&app, "not-a-real-id-xyz").await.0,
        StatusCode::BAD_REQUEST
    );
    // Too short to resolve safely: under 8 chars stays a 400.
    assert_eq!(get(&app, "bcb").await.0, StatusCode::BAD_REQUEST);
}
