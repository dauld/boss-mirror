//! `GET /api/yard/moves`, its stream, and the regions read's
//! `undeclared` reading — end to end through the real router against
//! the in-memory adapters (design e765b3fc §3, car M1 on feedback
//! 84cba7e2).
//!
//! What this pins:
//!
//! 1. **The page is the record.** Moves after a seq, oldest first, with
//!    the newest seq and the undrawn routes moves took in the window —
//!    the observed-undeclared count, under its band.
//! 2. **The stream resumes from the record.** A viewer that reconnects
//!    with `Last-Event-ID` is sent the rows after it under their own
//!    seq; one that connects fresh is sent a `resync`, never a flood.
//! 3. **Each region carries the undrawn routes INTO it**, and a jobs API
//!    with no record wired leaves that reading null — never an empty
//!    list that says every move took a drawn route.
//! 4. **Unread is not empty, and scoped is not empty.** No record wired
//!    is a 503; a caller whose packet scope is narrowed is refused by
//!    name rather than served a record naming packets outside it.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::moves::{InMemoryMoves, Move, MovesFeed, MovesStore};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

fn user(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// A move made a minute ago, so it is inside every window.
fn moved(
    n: u128,
    packet: &str,
    from: Option<&str>,
    to: Option<&str>,
    declared: Option<bool>,
) -> Move {
    Move {
        at: boss_clock_client::wall_now() - chrono::Duration::minutes(1),
        packet: packet.into(),
        kind: "ship-a-change".into(),
        label: format!("fix/{packet}"),
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        declared,
        cause_event_id: Uuid::from_u128(n),
        cause_seq: n as i64,
        cause_kind: "jobs.job.updated".into(),
        handoff_from: None,
        lineage: None,
        aboard: Vec::new(),
    }
}

async fn app(wired: bool) -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("sales", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let yard_moves = if wired {
        let store = Arc::new(InMemoryMoves::new());
        store
            .record(&[
                moved(1, "car-1", Some("gates"), Some("dock"), Some(true)),
                moved(2, "car-1", Some("track"), Some("shed"), Some(false)),
                moved(3, "car-2", None, Some("dock"), None),
                moved(4, "car-3", Some("track"), Some("shed"), Some(false)),
            ])
            .await
            .unwrap();
        Some(Arc::new(MovesFeed::new(store)))
    } else {
        None
    };
    router(JobsApiState {
        yard_moves,
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn get(app: axum::Router, uri: &str, role: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-boss-user", user(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, body)
}

#[tokio::test]
async fn the_page_is_the_record_after_a_seq_with_the_undrawn_routes_it_took() {
    let (status, body) = get(app(true).await, "/api/yard/moves?since=1", "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let seqs: Vec<i64> = body["moves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, [2, 3, 4], "after seq 1, oldest first");
    assert_eq!(body["latest_seq"], 4);
    assert_eq!(body["moves"][0]["from"], "track");
    assert_eq!(
        body["moves"][0]["cause_event_id"],
        Uuid::from_u128(2).to_string()
    );
    assert_eq!(body["undeclared"]["band"], "moves-undeclared");
    assert_eq!(body["undeclared"]["moves"], 2);
    let routes = body["undeclared"]["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1, "{routes:?}");
    assert_eq!(
        (&routes[0]["from"], &routes[0]["to"], &routes[0]["moves"]),
        (&Value::from("track"), &Value::from("shed"), &Value::from(2))
    );
    assert_eq!(
        body["mover"]["state"], "starting",
        "the mover says what it is doing"
    );
}

#[tokio::test]
async fn unwired_is_a_503_and_a_scoped_caller_is_refused_by_name() {
    let (status, _) = get(app(false).await, "/api/yard/moves", "operator").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = get(app(false).await, "/api/yard/moves/stream", "operator").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, body) = get(app(true).await, "/api/yard/moves", "sales").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.as_str().unwrap().contains("narrowed"), "{body}");
    let (status, _) = get(app(true).await, "/api/yard/moves/stream", "sales").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Read the stream's frames until `want` have arrived.
async fn frames(uri: &str, last_event_id: Option<&str>, want: usize) -> String {
    let mut req = Request::builder()
        .uri(uri)
        .header("x-boss-user", user("operator"));
    if let Some(id) = last_event_id {
        req = req.header("last-event-id", id);
    }
    let resp = app(true)
        .await
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let mut text = String::new();
    while text.matches("\n\n").count() < want {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("the stream sent its frames within 5s")
            .expect("the stream is open")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            text.push_str(&String::from_utf8_lossy(&data));
        }
    }
    text
}

#[tokio::test]
async fn the_stream_resumes_from_the_record_and_a_fresh_viewer_is_sent_a_resync() {
    let resumed = frames("/api/yard/moves/stream", Some("2"), 2).await;
    assert!(resumed.contains("event: move\nid: 3\n"), "{resumed}");
    assert!(resumed.contains("event: move\nid: 4\n"), "{resumed}");
    assert!(
        !resumed.contains("id: 2\n"),
        "nothing at or before the resume point: {resumed}"
    );

    let fresh = frames("/api/yard/moves/stream", None, 1).await;
    assert!(fresh.starts_with("event: resync\n"), "{fresh}");
    assert!(fresh.contains("\"seq\":4"), "{fresh}");
    assert!(
        !fresh.contains("event: move"),
        "a fresh viewer is not sent a flood: {fresh}"
    );
}

#[tokio::test]
async fn each_region_carries_the_undrawn_routes_into_it_and_unwired_reads_null() {
    let (status, body) = get(app(true).await, "/api/yard/regions", "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let region = |name: &str| {
        body["regions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap()
            .clone()
    };
    let shed = region("shed");
    assert_eq!(shed["undeclared"][0]["from"], "track", "{shed}");
    assert_eq!(shed["undeclared"][0]["moves"], 2);
    assert_eq!(
        region("dock")["undeclared"],
        Value::Array(vec![]),
        "every move in was drawn"
    );

    let (_, body) = get(app(false).await, "/api/yard/regions", "operator").await;
    for r in body["regions"].as_array().unwrap() {
        assert_eq!(
            r["undeclared"],
            Value::Null,
            "{}: unread, not empty",
            r["name"]
        );
    }
}
