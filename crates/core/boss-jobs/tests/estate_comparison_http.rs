//! `POST /api/estate/comparison` — the compare handler's one write.
//!
//! The estate comparison (59ef456a) is COMPUTED in the dispatcher's
//! `estate.compare` handler over the jobs API's own read surfaces —
//! handlers own no database — and lands here as one
//! `jobs.estate.compared` event per observation. Same dumb-door
//! posture as the observation door above it and the census door before
//! that: validate shape and trust, record verbatim, never recompute.
//!
//! Three properties pinned:
//! - an operator-tier caller's comparison lands as ONE actor-stamped
//!   event whose payload is the comparison;
//! - a non-operator caller is refused and nothing is recorded;
//! - a body without a `scope` string and a `findings` object is a 400
//!   — a comparison that cannot say what it compared or what it found
//!   is not a comparison.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape (7c3649e2).
fn user_header(id: &str, tier: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: "platform-admin".to_string(),
        access_tier: serde_json::from_value::<AccessTier>(serde_json::Value::String(
            tier.to_string(),
        ))
        .expect("unknown access tier"),
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("platform".to_string()),
    })
    .expect("a User always serialises")
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), jobs)
}

async fn post_comparison(app: &axum::Router, user: Option<&str>, body: &str) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/estate/comparison")
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-boss-user", u);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    resp.status()
}

fn comparison_body() -> String {
    serde_json::json!({
        "scope": "kubernetes-nodes",
        "observed_at": "2026-08-30T10:20:00Z",
        "observer": "boss-estate-observe",
        "counts": { "observed": 5, "observed_not_declared": 1 },
        "findings": {
            "observed_not_declared": [{"id": "w-1"}],
            "declared_not_observed": [],
            "drift": [],
        },
    })
    .to_string()
}

#[tokio::test]
async fn an_operator_comparison_lands_as_one_actor_stamped_event() {
    let (app, jobs) = app();
    let status = post_comparison(
        &app,
        Some(&user_header(
            "rule:estate-compare-on-observation",
            "operator",
        )),
        &comparison_body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let events = jobs.recorded_events();
    let compared: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.estate.compared")
        .collect();
    assert_eq!(compared.len(), 1, "exactly one event per comparison");
    let e = compared[0];
    assert_eq!(e.source, "jobs");
    assert_eq!(e.payload["scope"], "kubernetes-nodes");
    assert_eq!(e.payload["counts"]["observed_not_declared"], 1);
    assert_eq!(
        e.payload["findings"]["observed_not_declared"][0]["id"],
        "w-1"
    );
    assert_eq!(
        e.payload["_actor"],
        "automation:rule:estate-compare-on-observation"
    );
}

#[tokio::test]
async fn a_non_operator_caller_is_refused_and_nothing_is_recorded() {
    let (app, jobs) = app();
    let status = post_comparison(
        &app,
        Some(&user_header("emp-1", "user")),
        &comparison_body(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        jobs.recorded_events().is_empty(),
        "a refused comparison must record nothing"
    );
}

#[tokio::test]
async fn a_comparison_without_scope_or_findings_is_a_400() {
    let (app, jobs) = app();
    for body in [
        r#"{"findings": {}}"#,
        r#"{"scope": "kubernetes-nodes"}"#,
        r#"{"scope": 7, "findings": {}}"#,
        r#"{"scope": "kubernetes-nodes", "findings": []}"#,
        r#"[1,2,3]"#,
    ] {
        let status = post_comparison(
            &app,
            Some(&user_header(
                "rule:estate-compare-on-observation",
                "operator",
            )),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    }
    assert!(jobs.recorded_events().is_empty());
}
