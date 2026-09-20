//! Integration tests for the HTTP introspection API.
//!
//! Uses `tower::ServiceExt::oneshot` to exercise the router directly — no
//! sockets, no reqwest dependency. This also locks in the JSON response
//! shape, which is a public contract for the dashboard.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use boss_core::agent::{AgentId, AgentSpec, Cost, Message};
use boss_core::port::{CostLedger, MessageQueue};
use boss_cybernetics::http::{HttpState, router};
use boss_events::bus::InMemoryEventBus;
use boss_events::dispatcher::StubDispatcher;
use boss_events::ledger::InMemoryCostLedger;
use boss_events::queue::InMemoryMessageQueue;
use boss_events::registry::InMemoryAgentRegistry;
use serde_json::Value;
use tower::ServiceExt;

fn spec(slug: &str, budget: u64) -> AgentSpec {
    AgentSpec {
        id: AgentId::try_new(slug).unwrap(),
        display_name: slug.to_uppercase(),
        system_prompt: String::new(),
        model: "test".into(),
        hourly_budget_usd_micros: budget,
        max_concurrent_runs: 1,
    }
}

fn build_state(
    specs: Vec<AgentSpec>,
) -> (
    HttpState,
    Arc<InMemoryMessageQueue>,
    Arc<InMemoryCostLedger>,
) {
    let queue = Arc::new(InMemoryMessageQueue::new());
    let ledger = Arc::new(InMemoryCostLedger::new());
    let dispatcher = Arc::new(StubDispatcher::default());
    let registry = Arc::new(InMemoryAgentRegistry::new(specs));
    let _bus = Arc::new(InMemoryEventBus::new(16));
    let clock = Arc::new(boss_clock_client::WallClockClient);
    let state = HttpState {
        vm_id: Arc::from("vm-test"),
        queue: queue.clone(),
        dispatcher,
        ledger: ledger.clone(),
        registry,
        clock,
    };
    (state, queue, ledger)
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    (status, v)
}

#[tokio::test]
async fn health_returns_vm_and_status() {
    let (state, _, _) = build_state(vec![]);
    let app = router(state);
    let (status, body) = get_json(app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["vm_id"], "vm-test");
    assert_eq!(body["status"], "ok");
    assert!(body.get("timestamp").is_some());
}

#[tokio::test]
async fn agents_returns_registry_sorted_by_slug() {
    let (state, _, _) = build_state(vec![spec("zed", 100), spec("alpha", 200)]);
    let app = router(state);
    let (status, body) = get_json(app, "/agents").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], "alpha");
    assert_eq!(arr[0]["display_name"], "ALPHA");
    assert_eq!(arr[1]["id"], "zed");
}

#[tokio::test]
async fn queues_reports_per_agent_depth() {
    let (state, queue, _) = build_state(vec![spec("planner", 100)]);
    let agent = AgentId::try_new("planner").unwrap();
    queue
        .enqueue(Message::new(agent.clone(), "k", serde_json::json!({})))
        .await
        .unwrap();
    queue
        .enqueue(Message::new(agent, "k", serde_json::json!({})))
        .await
        .unwrap();
    let app = router(state);
    let (status, body) = get_json(app, "/queues").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["agent"], "planner");
    assert_eq!(arr[0]["depth"], 2);
}

#[tokio::test]
async fn runs_returns_empty_when_no_dispatches() {
    let (state, _, _) = build_state(vec![]);
    let app = router(state);
    let (status, body) = get_json(app, "/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn costs_default_window_is_hour() {
    let (state, _, ledger) = build_state(vec![spec("planner", 1_000), spec("doctor", 1_000)]);
    let planner = AgentId::try_new("planner").unwrap();
    ledger
        .record(
            &planner,
            Cost {
                input_tokens: 1,
                output_tokens: 2,
                usd_micros: Some(300),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let (status, body) = get_json(app, "/costs").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // sorted by slug
    assert_eq!(arr[0]["agent"], "doctor");
    assert_eq!(arr[0]["cost"]["usd_micros"], 0);
    assert_eq!(arr[0]["window"], "hour");
    assert_eq!(arr[1]["agent"], "planner");
    assert_eq!(arr[1]["cost"]["usd_micros"], 300);
}

#[tokio::test]
async fn costs_window_day_is_accepted() {
    let (state, _, _) = build_state(vec![spec("planner", 1_000)]);
    let app = router(state);
    let (status, body) = get_json(app, "/costs?window=day").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["window"], "day");
}

#[tokio::test]
async fn costs_invalid_window_returns_4xx() {
    let (state, _, _) = build_state(vec![]);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::get("/costs?window=bogus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_client_error());
}
