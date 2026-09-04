//! Smoke test: full boss-jobs-api → boss-policy-api round trip over
//! HTTP.
//!
//! Every other test in this directory uses `FakePolicyClient` (in-
//! process), which means none of them would notice if
//! `BOSS_POLICY_URL` pointed at the wrong port — the canonical
//! 7060/7250 collision (commit `bb60c58`) shipped because no test
//! exercised the real wire.
//!
//! This test boots an in-memory `JobsApiState` configured with a
//! `ReqwestPolicyClient` that points at the running boss-policy-api
//! (default `http://localhost:7250`, override via
//! `BOSS_POLICY_URL` to mirror the production env shape). It then
//! issues `GET /api/workflows` and asserts a 200 — proving the
//! policy gate end-to-end on the smoke-tester fixture role.
//!
//! Skipped cleanly when boss-policy-api isn't reachable, so a fresh
//! checkout running `cargo test --workspace` doesn't false-fail
//! before services are up. Set `BOSS_REQUIRE_POLICY_ROUNDTRIP=1` to
//! make the skip a hard failure (CI uses this flag).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::InMemoryWorkflows;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, WorkflowRegistry};
use boss_policy_client::{PolicyClient, ReqwestPolicyClient};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

fn policy_url() -> String {
    std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| "http://localhost:7250".to_string())
}

fn require_roundtrip() -> bool {
    std::env::var("BOSS_REQUIRE_POLICY_ROUNDTRIP")
        .map(|v| v == "1")
        .unwrap_or(false)
}

async fn policy_reachable(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(750))
        .build()
        .expect("build reqwest client");
    let health = format!("{}/api/policy/health", url.trim_end_matches('/'));
    matches!(client.get(&health).send().await, Ok(r) if r.status().is_success())
}

#[tokio::test(flavor = "multi_thread")]
async fn smoke_tester_can_read_workflows_through_real_policy_api() {
    let url = policy_url();
    if !policy_reachable(&url).await {
        if require_roundtrip() {
            panic!(
                "BOSS_REQUIRE_POLICY_ROUNDTRIP=1 set but boss-policy-api is unreachable at {url} \
                 — start the service or unset the env var"
            );
        }
        eprintln!(
            "skipping: boss-policy-api not reachable at {url} \
             (set BOSS_REQUIRE_POLICY_ROUNDTRIP=1 in CI to make this a hard failure)"
        );
        return;
    }

    // Wire jobs-api against the real policy-api over HTTP. This is
    // the same shape boss_jobs_api.rs builds in production.
    let policy: Arc<dyn PolicyClient> = Arc::new(ReqwestPolicyClient::new(url));

    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let kind_registry: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::new());

    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: Some(kind_registry),
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

    let req = Request::builder()
        .method("GET")
        .uri("/api/workflows")
        .header(
            "x-boss-user",
            serde_json::json!({
                "id": "emp-smoke",
                "role": "smoke-tester",
                "access_tier": "operator",
                "territory_account_ids": [],
                "direct_report_ids": [],
                "department": "executive",
            })
            .to_string(),
        )
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "smoke-tester should be allowed to read Workflows via the real \
         policy-api round trip — got {} (BOSS_POLICY_URL={})",
        resp.status(),
        policy_url(),
    );
}
