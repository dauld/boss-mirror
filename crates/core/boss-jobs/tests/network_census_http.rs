//! `POST /api/network/census` — the census handler's one write.
//!
//! The packet-loss census (docs/design/packet-loss.md, decided in
//! review 9fb9904f) is COMPUTED in the dispatcher's `network.census`
//! handler, over the jobs API's own read surfaces. What it computes
//! has to land on the audit log as one event per firing — the
//! measured series Q3 asked for — and dispatcher handlers own no
//! database, only HTTP. This endpoint is the door: it accepts the
//! counts and records exactly one `jobs.network.census` marker event
//! through the repository's standalone-event path (`record_events`,
//! the same reliable-delivery path the step.ready markers use).
//!
//! Three properties pinned here:
//! - an operator-tier caller's counts land as ONE event whose payload
//!   is the counts, actor-stamped;
//! - a non-operator caller is refused and nothing is recorded — the
//!   census is operator machinery, like the cadence surface;
//! - a non-object body is a 400, not an empty event.

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

// SERIALISE THE REAL TYPE, never a copy of its wire shape. A test
// that hand-builds the header is testing a copy: if a field loses its
// serde default or a new required one lands, a hand-built payload keeps
// passing here while production rejects it — the failure surfaces as a
// live 4xx instead of a red test, which is the wrong way round
// (7c3649e2).
fn user_header(id: &str, tier: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: "platform-admin".to_string(),
        // Round-trip the tier through the real enum too, so an
        // unknown tier is a test failure rather than a string that
        // silently means nothing.
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
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

async fn post_census(app: &axum::Router, user: Option<&str>, body: &str) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/network/census")
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

#[tokio::test]
async fn operator_counts_land_as_one_actor_stamped_event() {
    let (app, jobs) = app();
    let counts = serde_json::json!({
        "census_day": "2026-08-21",
        "open_total": 3,
        "orphaned_count": 1,
    });
    let status = post_census(
        &app,
        Some(&user_header("rule:network-census-daily", "operator")),
        &counts.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let events = jobs.recorded_events();
    let census: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.network.census")
        .collect();
    assert_eq!(census.len(), 1, "exactly one event per firing");
    let e = census[0];
    assert_eq!(e.source, "jobs");
    assert_eq!(e.payload["open_total"], 3);
    assert_eq!(e.payload["orphaned_count"], 1);
    assert_eq!(e.payload["census_day"], "2026-08-21");
    // The dispatcher's rule identity rides as `_actor`, the same way
    // EventStamp injects it everywhere else.
    assert_eq!(e.payload["_actor"], "automation:rule:network-census-daily");
}

#[tokio::test]
async fn a_non_operator_caller_is_refused_and_nothing_is_recorded() {
    let (app, jobs) = app();
    let status = post_census(
        &app,
        Some(&user_header("emp-1", "user")),
        r#"{"open_total": 1}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        jobs.recorded_events().is_empty(),
        "a refused census must record nothing"
    );
}

#[tokio::test]
async fn an_unidentified_caller_is_refused() {
    let (app, jobs) = app();
    // No x-boss-user header at all: the extractor defaults to guest,
    // and a guest is not an operator.
    let status = post_census(&app, None, r#"{"open_total": 1}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(jobs.recorded_events().is_empty());
}

#[tokio::test]
async fn a_non_object_body_is_a_400_not_an_empty_event() {
    let (app, jobs) = app();
    let status = post_census(
        &app,
        Some(&user_header("rule:network-census-daily", "operator")),
        r#"[1, 2, 3]"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(jobs.recorded_events().is_empty());
}
