//! Layer 4: a `backlog-item` whose work was DELIVERED THROUGH THE
//! TENANT REPO closes on proof of that delivery, not as `stale`.
//!
//! The defect (backlog 4bcf1265, measured 2026-09-24). 3fec1ed9
//! (marketing-weekly) and fa77e3d7 (modules) were both delivered
//! through `david/algedonic-llc` plus a tenant publish — no car in this
//! tree. The build step's disposition offered only `stale`, `duplicate`
//! and `decline`, and a bare completion reaches `closed` on the
//! strength of a car that never existed, so both items closed as
//! `stale`: the record says the claim died when in fact the work was
//! done. Algedonic's department protocols live in the tenant repo, so
//! this recurs on every one of them (design fd8b5143, build-plan item 3).
//!
//! THE FIX, ON THE SAME SHAPE AS 6c114a23. `build` gains a fourth
//! disposition, `delivered`, and a delivered build does NOT close the
//! item: it opens `prove-delivery`, which requires WHERE it landed and
//! the LIVE READ-BACK that shows it running (for a protocol, `GET
//! /api/workflows/<kind>` naming it at the version the tenant branch
//! declared). Only that proof reaches `closed`. A delivered build that
//! could close on its own word would be the mostly-sure shape — a
//! belief about a repo this tree cannot see — so the read-back is
//! required at done, the one artifact the record can hold.
//!
//! These tests drive the REAL router against the REAL platform bundle,
//! for the reason `backlog_item_build_can_refute` gives.

use std::sync::Arc;

use async_trait::async_trait;
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
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

async fn open_item(app: &axum::Router) -> String {
    let body = serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
        "title": "Delivered tenant work closes as stale",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": { "area": "platform" },
        "tags": ["protocol"],
    })
    .to_string();
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn read(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    body
}

fn step_of<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {job:#?}"))
}

fn status_of(job: &serde_json::Value, slug: &str) -> String {
    step_of(job, slug)["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn actionable(job: &serde_json::Value, slug: &str) -> bool {
    matches!(status_of(job, slug).as_str(), "ready" | "active")
}

/// Complete a step, merging `extra` over its current metadata —
/// never replacing, because `authority_role` shares that object.
async fn try_complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let step = step_of(job, slug);
    let mut metadata = step["metadata"].clone();
    for (k, v) in extra.as_object().into_iter().flatten() {
        metadata[k] = v.clone();
    }
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/jobs/{job_id}/steps/{}",
                step["id"].as_str().expect("step id")
            ))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": metadata }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) {
    assert!(
        actionable(job, slug),
        "step `{slug}` is `{}`, not actionable",
        status_of(job, slug)
    );
    let (status, body) = try_complete(app, job_id, job, slug, extra).await;
    assert!(
        status.is_success(),
        "completing `{slug}` failed {status}: {body}"
    );
}

fn triage_build() -> serde_json::Value {
    serde_json::json!({
        "disposition": "build",
        "evidence": "The claim was measured against origin/main at triage time and held.",
    })
}

/// Route a fresh packet to `build`, returning it as read after triage.
async fn routed_to_build(app: &axum::Router) -> (String, serde_json::Value) {
    let job_id = open_item(app).await;
    let job = read(app, &job_id).await;
    complete(app, &job_id, &job, "triage", triage_build()).await;
    let after = read(app, &job_id).await;
    (job_id, after)
}

/// Route to `build` and complete it as `delivered`, returning the
/// packet as read after the build.
async fn delivered(app: &axum::Router) -> (String, serde_json::Value) {
    let (job_id, after) = routed_to_build(app).await;
    complete(
        app,
        &job_id,
        &after,
        "build",
        serde_json::json!({ "disposition": "delivered" }),
    )
    .await;
    let after = read(app, &job_id).await;
    (job_id, after)
}

fn proof() -> serde_json::Value {
    serde_json::json!({
        "landed": "david/algedonic-llc main at 3c1d9e0a, published by boss tenant publish",
        "read_back": "GET /api/workflows/marketing-weekly answers version 2, active",
    })
}

/// Routing to `build` must keep the proof step PENDING, not Skipped —
/// a skipped step never comes back, and it references only `build`.
#[tokio::test]
async fn routing_to_build_keeps_the_proof_step_alive() {
    let app = app();
    let (_id, after) = routed_to_build(&app).await;
    assert_eq!(
        status_of(&after, "prove-delivery"),
        "pending",
        "`prove-delivery` must stay reachable from the build. Steps: {:#?}",
        after["steps"]
    );
}

/// THE DEFECT ITSELF. A build delivered through the tenant repo opens
/// the proof, withdraws nothing, and does not close the item on its
/// own word.
#[tokio::test]
async fn a_delivered_build_opens_the_proof_and_closes_nothing() {
    let app = app();
    let (_id, after) = delivered(&app).await;

    assert!(
        actionable(&after, "prove-delivery"),
        "a delivered build must open `prove-delivery` — it is `{}`. Steps: {:#?}",
        status_of(&after, "prove-delivery"),
        after["steps"]
    );
    for slug in ["stale", "duplicate", "declined"] {
        assert_eq!(
            status_of(&after, slug),
            "skipped",
            "delivered work is not a withdrawal — `{slug}` is `{}`",
            status_of(&after, slug)
        );
    }
    assert_eq!(
        status_of(&after, "closed"),
        "pending",
        "the item must not close on the delivery's own word, before its proof"
    );
    assert_eq!(after["status"], "open");
}

/// No evidence is not a pass: the proof step refuses to complete
/// without the live read-back.
#[tokio::test]
async fn the_proof_requires_the_live_read_back() {
    let app = app();
    let (job_id, after) = delivered(&app).await;

    let (status, body) = try_complete(
        &app,
        &job_id,
        &after,
        "prove-delivery",
        serde_json::json!({ "landed": "david/algedonic-llc main at 3c1d9e0a" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a proof with no read-back must be refused: {body}"
    );
}

/// Proven, the item closes as `completed` — the work was done — never
/// as `stale`.
#[tokio::test]
async fn a_proven_delivery_closes_the_item_as_completed() {
    let app = app();
    let (job_id, after) = delivered(&app).await;

    complete(&app, &job_id, &after, "prove-delivery", proof()).await;
    let after = read(&app, &job_id).await;
    if actionable(&after, "closed") {
        complete(&app, &job_id, &after, "closed", serde_json::json!({})).await;
    }
    let done = read(&app, &job_id).await;
    assert_eq!(
        done["status"], "closed",
        "a proven delivery closes the item"
    );
    assert_eq!(
        done["metadata"]["outcome"], "completed",
        "delivered work closes as completed, not stale: {:#?}",
        done["metadata"]
    );
}

/// THE REGRESSION THIS MUST NOT CAUSE. A build that built in this tree
/// carries no disposition, skips the proof, and closes as before.
#[tokio::test]
async fn a_build_that_built_here_skips_the_proof() {
    let app = app();
    let (job_id, after) = routed_to_build(&app).await;

    complete(&app, &job_id, &after, "build", serde_json::json!({})).await;
    let after = read(&app, &job_id).await;
    assert_eq!(
        status_of(&after, "prove-delivery"),
        "skipped",
        "a car built in this tree has no tenant delivery to prove"
    );
    assert!(
        actionable(&after, "closed") || after["status"] == "closed",
        "an ordinary build still reaches `closed` — it is `{}`",
        status_of(&after, "closed")
    );
}
