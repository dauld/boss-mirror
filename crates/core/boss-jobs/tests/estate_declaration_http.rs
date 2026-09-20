//! `POST /api/estate/nodes/batch` — the tree's estate declaration
//! (backlog ee368d0c, design 42277636 first wave).
//!
//! Until 2026-09-18 a node reached a database only as a schema
//! migration, so every fresh OSS database booted with this LAN's seven
//! machines. Now infra/estate/estate.toml declares them and the
//! launcher publishes the file on every pod start through this door.
//!
//! Properties pinned:
//! - the door is operator machinery: a guest and a user-tier session
//!   are refused and nothing lands;
//! - a declaration lands insert-if-absent and `GET /api/estate/nodes`
//!   then reads it, roles sorted;
//! - a second publish inserts nothing, records nothing;
//! - a role added to the file lands on a node already there (the
//!   forge gained `cluster-operator` after its row existed) and leaves
//!   one `node.declared` naming what landed;
//! - a bad row refuses the whole batch by name.

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
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn header(id: &str, role: &str, tier: AccessTier) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: tier,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("platform".to_string()),
    })
    .expect("a User always serialises")
}

fn launcher() -> String {
    header(
        "automation:estate-seed",
        "platform-admin",
        AccessTier::Operator,
    )
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
        dispatcher_firings: None,
        agent_budget: None,
    };
    (router(state), jobs)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    user: Option<String>,
) -> (StatusCode, Value, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(u) = user {
        req = req.header("x-boss-user", u);
    }
    let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

fn declaration() -> Value {
    json!({ "nodes": [
        { "id": "cp-1", "label": "cp-1", "address": "192.0.2.11", "role": "talos-control-plane",
          "cpu": 8, "memory_gb": 15, "disk_gb": 236 },
        { "id": "forge", "label": "Forge host", "address": "192.0.2.15", "role": "forge",
          "roles": ["cluster-operator"], "cpu": 16, "memory_gb": 30, "disk_gb": 228,
          "notes": "the pipeline host" }
    ]})
}

#[tokio::test]
async fn a_declaration_lands_and_the_estate_read_serves_it() {
    let (app, jobs) = app();
    let (status, body, _) = send(&app, "GET", "/api/estate/nodes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!([]), "a fresh registry declares nothing");

    let (status, body, text) = send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(declaration()),
        Some(launcher()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        body,
        json!({ "received": 2, "inserted": 2, "roles_inserted": 1 })
    );

    let (_, body, _) = send(&app, "GET", "/api/estate/nodes", None, None).await;
    let nodes = body["data"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    let forge = nodes.iter().find(|n| n["id"] == "forge").unwrap();
    assert_eq!(forge["role"], "forge");
    assert_eq!(forge["roles"], json!(["cluster-operator"]));
    assert_eq!(forge["retired"], false);
    let cp1 = nodes.iter().find(|n| n["id"] == "cp-1").unwrap();
    assert_eq!(
        cp1["roles"],
        json!([]),
        "a node declaring no roles reads an empty set"
    );

    let events = jobs.recorded_events();
    let declared: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "node.declared")
        .collect();
    assert_eq!(declared.len(), 2, "one fact per node the batch changed");
    assert_eq!(declared[1].payload["id"], "forge");
    assert_eq!(declared[1].payload["declared_by"], "automation:estate-seed");
    assert_eq!(
        declared[1].payload["landed"],
        json!({ "node": true, "roles": ["cluster-operator"] })
    );

    // A second publish of the same file: nothing new, nothing recorded.
    let (_, body, _) = send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(declaration()),
        Some(launcher()),
    )
    .await;
    assert_eq!(
        body,
        json!({ "received": 2, "inserted": 0, "roles_inserted": 0 })
    );
    assert_eq!(
        jobs.recorded_events()
            .iter()
            .filter(|e| e.kind == "node.declared")
            .count(),
        2
    );
}

/// The forge's `cluster-operator` arrived after its row existed
/// (202609121800 after 144): a role added to the file lands on the
/// kept node, and the fact says the node was kept and the role landed.
#[tokio::test]
async fn a_role_added_later_lands_on_a_kept_node_and_the_fact_says_so() {
    let (app, jobs) = app();
    let mut first = declaration();
    first["nodes"][1]["roles"] = json!([]);
    send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(first),
        Some(launcher()),
    )
    .await;
    let mut later = declaration();
    later["nodes"][1]["notes"] = json!("a note the kept row must not take");
    let (_, body, _) = send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(later),
        Some(launcher()),
    )
    .await;
    assert_eq!(
        body,
        json!({ "received": 2, "inserted": 0, "roles_inserted": 1 })
    );
    let (_, body, _) = send(&app, "GET", "/api/estate/nodes", None, None).await;
    let forge = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "forge")
        .cloned()
        .unwrap();
    assert_eq!(forge["roles"], json!(["cluster-operator"]));
    assert_eq!(
        forge["notes"], "the pipeline host",
        "the kept row keeps its notes"
    );
    let last = jobs
        .recorded_events()
        .into_iter()
        .rfind(|e| e.kind == "node.declared")
        .unwrap();
    assert_eq!(
        last.payload["landed"],
        json!({ "node": false, "roles": ["cluster-operator"] })
    );
}

#[tokio::test]
async fn a_bad_row_refuses_the_batch_by_name_and_nothing_lands() {
    let (app, jobs) = app();
    let mut bad = declaration();
    bad["nodes"][0]["address"] = json!("");
    let (status, _, text) = send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(bad),
        Some(launcher()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text.contains("cp-1") && text.contains("address"), "{text}");
    let mut unknown = declaration();
    unknown["nodes"][0]["free_gb"] = json!(100);
    let (status, _, _) = send(
        &app,
        "POST",
        "/api/estate/nodes/batch",
        Some(unknown),
        Some(launcher()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "observed state is not a declaration: an unknown key is refused"
    );
    assert!(jobs.list_estate_nodes().await.unwrap().is_empty());
    assert!(jobs.recorded_events().is_empty());
}

#[tokio::test]
async fn the_declaration_door_is_operator_machinery() {
    let (app, jobs) = app();
    for user in [
        None,
        Some(header(
            "guest@example.test",
            "audit-readonly",
            AccessTier::User,
        )),
        Some(header("emp-someone", "member", AccessTier::User)),
    ] {
        let (status, _, _) = send(
            &app,
            "POST",
            "/api/estate/nodes/batch",
            Some(declaration()),
            user,
        )
        .await;
        assert_ne!(
            status,
            StatusCode::OK,
            "a reader of the estate must not be able to declare it"
        );
    }
    assert!(jobs.list_estate_nodes().await.unwrap().is_empty());
}

use boss_jobs::port::JobsRepository;
