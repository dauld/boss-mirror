//! `GET /api/yard/moves`, its stream, and the regions read's
//! `undeclared` reading — end to end through the real router against
//! the in-memory adapters (design e765b3fc §3, car M1 on feedback
//! 84cba7e2).
//!
//! What this pins:
//!
//! 1. **The page is the record.** Moves after a seq, oldest first, with
//!    the newest seq and the routes moves took in the window that no
//!    derived route declares — the observed-undeclared count, under its
//!    band (car R2 derives the routes from the protocols and hand-offs).
//! 2. **The stream resumes from the record.** A viewer that reconnects
//!    with `Last-Event-ID` is sent the rows after it under their own
//!    seq; one that connects fresh is sent a `resync`, never a flood.
//! 3. **Each region carries the undeclared routes INTO it, and they
//!    trouble it** (car R2 switched the band on); a jobs API with no
//!    record wired, or no registries to derive the routes from, leaves
//!    that reading null and judges nothing — never an empty list that
//!    says every move took a declared route.
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
use boss_jobs::registry::InMemoryWorkflows;
use boss_jobs::stations::InMemoryStations;
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
    app_with(wired, true).await
}

/// `registries`: the tree's protocols and stations wired, so the routes
/// can be derived.
async fn app_with(wired: bool, registries: bool) -> axum::Router {
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
                // Declared by auto-park's hand-off.
                moved(1, "car-1", Some("gates"), Some("dock"), Some(true)),
                // Stamped undeclared under the ten drawn borders (car M1);
                // train-reconcile's hand-off declares it now, and the read
                // judges against the routes derived now, not the stamp.
                moved(2, "car-1", Some("track"), Some("shed"), Some(false)),
                // Onto the map at the dock: ship-a-change's own gate step.
                moved(3, "car-2", None, Some("dock"), None),
                // Nothing walks from the shed into arrivals: a car in the
                // shed leaves by its proof. (arrivals -> publish was the
                // example here until the publish rule's hand-off declared
                // it, backlog d085dc47.)
                moved(4, "car-3", Some("shed"), Some("arrivals"), Some(false)),
                moved(5, "car-4", Some("shed"), Some("arrivals"), None),
            ])
            .await
            .unwrap();
        Some(Arc::new(MovesFeed::new(store)))
    } else {
        None
    };
    let (kind_registry, stations) = if registries {
        let kinds = Arc::new(InMemoryWorkflows::new());
        for spec in boss_jobs::registry::seedable_platform_workflows() {
            kinds.seed(spec).unwrap();
        }
        let stations = Arc::new(InMemoryStations::new());
        for spec in
            boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
                .unwrap()
        {
            stations.seed(spec).unwrap();
        }
        (
            Some(kinds as Arc<dyn boss_jobs::registry::WorkflowRegistry>),
            Some(stations as Arc<dyn boss_jobs::stations::StationRegistry>),
        )
    } else {
        (None, None)
    };
    router(JobsApiState {
        yard_moves,
        kind_registry,
        stations,
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
    assert_eq!(seqs, [2, 3, 4, 5], "after seq 1, oldest first");
    assert_eq!(body["latest_seq"], 5);
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
        (
            &Value::from("shed"),
            &Value::from("arrivals"),
            &Value::from(2)
        ),
        "track -> shed is declared now, whatever its row was stamped"
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
    let resumed = frames("/api/yard/moves/stream", Some("2"), 3).await;
    assert!(resumed.contains("event: move\nid: 3\n"), "{resumed}");
    assert!(resumed.contains("event: move\nid: 4\n"), "{resumed}");
    assert!(resumed.contains("event: move\nid: 5\n"), "{resumed}");
    assert!(
        !resumed.contains("id: 2\n"),
        "nothing at or before the resume point: {resumed}"
    );

    let fresh = frames("/api/yard/moves/stream", None, 1).await;
    assert!(fresh.starts_with("event: resync\n"), "{fresh}");
    assert!(fresh.contains("\"seq\":5"), "{fresh}");
    assert!(
        !fresh.contains("event: move"),
        "a fresh viewer is not sent a flood: {fresh}"
    );
}

#[tokio::test]
async fn each_region_carries_the_undeclared_routes_into_it_and_is_troubled_by_them() {
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
    let arrivals = region("arrivals");
    assert_eq!(arrivals["undeclared"][0]["from"], "shed", "{arrivals}");
    assert_eq!(arrivals["undeclared"][0]["moves"], 2);
    assert_eq!(arrivals["state"], "troubled", "{arrivals}");
    assert_eq!(arrivals["band"]["id"], "moves-undeclared", "{arrivals}");
    assert!(
        arrivals["why"]
            .as_str()
            .unwrap()
            .contains("shed → arrivals ×2"),
        "the verdict names the route and its count: {arrivals}"
    );
    for name in ["dock", "shed", "publish"] {
        assert_eq!(
            region(name)["undeclared"],
            Value::Array(vec![]),
            "{name}: every move in took a declared route"
        );
        assert_ne!(region(name)["band"]["id"], "moves-undeclared");
    }
}

#[tokio::test]
async fn an_unwired_record_or_underivable_routes_read_null_and_judge_nothing() {
    for (wired, registries) in [(false, true), (true, false)] {
        let (_, body) = get(
            app_with(wired, registries).await,
            "/api/yard/regions",
            "operator",
        )
        .await;
        for r in body["regions"].as_array().unwrap() {
            assert_eq!(
                r["undeclared"],
                Value::Null,
                "{} (record {wired}, registries {registries}): unread, not empty",
                r["name"]
            );
            assert_ne!(r["band"]["id"], "moves-undeclared", "{r}");
        }
    }
}

/// `GET /api/yard/routes` serves the derived routes with their sources,
/// the observed counts beside them, and 503 — naming the registry — when
/// there is nothing to walk.
#[tokio::test]
async fn the_routes_read_serves_every_route_with_its_sources() {
    let (status, body) = get(app(true).await, "/api/yard/routes", "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["observed"], true);
    assert_eq!(body["refused"], Value::Array(vec![]), "{body}");
    let routes = body["routes"].as_array().unwrap();
    let find = |from: Value, to: Value| {
        routes
            .iter()
            .find(|r| r["from"] == from && r["to"] == to)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let train = find("dock".into(), "gates".into());
    assert_eq!(train["declared"], true, "{train}");
    assert!(
        train["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source"] == "workflow" && s["workflow"] == "pr-train" && s["step"] == "pr"),
        "{train}"
    );
    let undrawn = find("shed".into(), "arrivals".into());
    assert_eq!(undrawn["declared"], false, "{undrawn}");
    assert_eq!(undrawn["sources"][0]["source"], "observed");
    assert_eq!(undrawn["sources"][0]["moves"], 2);
    // The off-ramp and the connector this read did not serve until
    // backlog d085dc47: an arrived train ages out of arrivals by its own
    // terminal, and the publish rule's hand-off joins arrivals to publish.
    let exit = find("arrivals".into(), Value::Null);
    assert_eq!(exit["declared"], true, "{exit}");
    assert!(
        exit["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["workflow"] == "pr-train" && s["step"] == "arrived"),
        "{exit}"
    );
    let publish = find("arrivals".into(), "publish".into());
    assert_eq!(publish["declared"], true, "{publish}");

    let (status, body) = get(app_with(true, false).await, "/api/yard/routes", "operator").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.as_str().unwrap().contains("workflow registry"),
        "{body}"
    );
}
