//! The station authoring path — stations editable at RUN TIME.
//!
//! David, 2026-08-13: "stations need to be editable at run time. They
//! should be data in a registry." The registry and the port already
//! existed; until these routes landed, redrawing a queue meant a SQL
//! seed and a deploy — the exact leak the three-layer reading names,
//! a protocol that cannot be replaced without a deploy having sunk
//! into the substrate.
//!
//! What is asserted here, in the order it matters:
//!
//!  1. A draft can be authored, published, and immediately serves its
//!     queue — no deploy, no restart.
//!  2. **Publish refuses an unviable draft** (422 + the problem list),
//!     and the incumbent keeps serving. This is the whole reason the
//!     lint exists: an always-empty queue and a quiet day render
//!     identically, so nothing downstream can notice the mistake.
//!  3. `_validate` and publish agree — the dry run's problem list is
//!     byte-identical to the refusal's, which is precisely what the
//!     2026-08-13 workflow outage got wrong (`_validate` could name
//!     the problem the whole time; publish never asked it).
//!  4. A draft may be saved unviable. Drafts are work in progress.
//!  5. Authoring is privilege-gated on `workflow`, not `job`: a
//!     caller who may READ every queue still may not redraw one.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, StationKind, StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape. A test
// that hand-builds the header is testing a copy: if a field loses its
// serde default or a new required one lands, a hand-built payload keeps
// passing here while production rejects it — the failure surfaces as a
// live 4xx instead of a red test, which is the wrong way round
// (7c3649e2).
fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
}

/// A viable draft: an ordinary shared batch queue.
fn viable_draft(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "Authored at run time",
        StationKind::Batch,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
        now(),
    )
}

/// An unviable draft: `actor` kind that never binds `@me`, so every
/// executor would see the same queue while the row claims it is
/// personal. Exactly the shape that made the census read a per-actor
/// station as depth 0.
fn unviable_draft(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "Claims to be personal, is not",
        StationKind::Actor,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
        now(),
    )
}

fn app() -> (axum::Router, Arc<InMemoryStations>) {
    let stations = Arc::new(InMemoryStations::new());
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            // The author: may redraw the network.
            .allow("ceo", Action::Create, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Read, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Update, Resource::workflow(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            // The reader: may see every queue, may redraw none.
            .allow("reporter", Action::Read, Resource::job(), Scope::All)
            .allow("reporter", Action::Read, Resource::workflow(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: Some(stations.clone() as Arc<dyn StationRegistry>),
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None::<Arc<dyn WorkflowRegistry>>,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), stations)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

fn post(path: &str, role: &str, body: Option<&StationSpec>) -> Request<Body> {
    let b = match body {
        Some(spec) => Body::from(serde_json::to_vec(spec).unwrap()),
        None => Body::empty(),
    };
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-boss-user", user_header("emp-1", role))
        .body(b)
        .unwrap()
}

fn get(path: &str, role: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-boss-user", user_header("emp-1", role))
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_station_authored_at_run_time_serves_its_queue_without_a_deploy() {
    let (app, _) = app();

    let (status, _) = send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("new-desk"))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Not live yet — a draft is staged, not serving.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0, "a draft must not serve traffic");

    let (status, published) = send(&app, post("/api/stations/new-desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(published["status"], "active");
    assert_eq!(published["version"], 1);

    // Live, in the same process, with no restart in between.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["data"][0]["name"], "new-desk");

    let (status, queue) = send(&app, get("/api/stations/new-desk/queue", "ceo")).await;
    assert_eq!(status, StatusCode::OK, "the new station evaluates: {queue}");
}

#[tokio::test]
async fn publish_refuses_an_unviable_draft_and_the_incumbent_keeps_serving() {
    let (app, stations) = app();

    // An incumbent, live.
    stations.seed(viable_draft("desk")).unwrap();
    let (status, _) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::OK);

    // A bad v2 staged over it.
    let mut bad = unviable_draft("desk");
    bad.version = 2;
    let (status, _) = send(&app, post("/api/stations", "ceo", Some(&bad))).await;
    assert_eq!(status, StatusCode::CREATED, "a draft may be saved unviable");

    let (status, body) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unviable draft must not reach the ACTIVE slot: {body}"
    );
    assert_eq!(body["ok"], false);
    let problems = body["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0]["field"], "predicate");
    assert!(
        problems[0]["message"].as_str().unwrap().contains("@me"),
        "the refusal must name the fix: {}",
        problems[0]["message"]
    );

    // The incumbent is untouched — a refused publish is not an outage.
    let (status, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["data"][0]["version"], 1, "v1 still serves");
    assert_eq!(body["data"][0]["status"], "active");
}

#[tokio::test]
async fn the_dry_run_and_the_publish_refusal_report_the_same_problems() {
    // The 2026-08-13 outage in one assertion: an editor that shows
    // "no problems" must publish cleanly, and a refused publish must
    // show what the editor would have shown.
    let (app, stations) = app();

    let (status, dry) = send(
        &app,
        post(
            "/api/stations/_validate",
            "ceo",
            Some(&unviable_draft("desk")),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "lint failures are data, not an error"
    );
    assert_eq!(dry["ok"], false);

    stations.seed(unviable_draft("desk")).unwrap();
    let (status, refused) = send(&app, post("/api/stations/desk/publish", "ceo", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(
        dry, refused,
        "the dry run and the refusal must be the same body"
    );
}

#[tokio::test]
async fn a_viable_spec_dry_runs_clean() {
    let (app, _) = app();
    let (status, dry) = send(
        &app,
        post(
            "/api/stations/_validate",
            "ceo",
            Some(&viable_draft("desk")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["ok"], true);
    assert_eq!(dry["problems"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn versions_are_append_only_and_readable() {
    let (app, _) = app();

    send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("desk"))),
    )
    .await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let mut v2 = viable_draft("desk");
    v2.version = 2;
    v2.title = "Second cut".into();
    send(&app, post("/api/stations", "ceo", Some(&v2))).await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let (status, versions) = send(&app, get("/api/stations/desk/versions", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    let rows = versions.as_array().unwrap();
    assert_eq!(rows.len(), 2, "history is kept, not overwritten");
    assert_eq!(rows[0]["version"], 1);
    assert_eq!(rows[0]["status"], "retired");
    assert_eq!(rows[1]["version"], 2);
    assert_eq!(rows[1]["status"], "active");

    // The retired row is still readable — what this queue used to be.
    let (status, v1) = send(&app, get("/api/stations/desk/versions/1", "ceo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1["title"], "Authored at run time");
}

#[tokio::test]
async fn retire_closes_the_station_and_is_idempotent() {
    let (app, _) = app();
    send(
        &app,
        post("/api/stations", "ceo", Some(&viable_draft("desk"))),
    )
    .await;
    send(&app, post("/api/stations/desk/publish", "ceo", None)).await;

    let (status, _) = send(&app, post("/api/stations/desk/retire", "ceo", None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = send(&app, get("/api/stations", "ceo")).await;
    assert_eq!(body["total"], 0, "a retired station serves nothing");

    let (status, _) = send(&app, post("/api/stations/desk/retire", "ceo", None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "retire is idempotent");
}

#[tokio::test]
async fn reading_every_queue_does_not_confer_redrawing_one() {
    let (app, stations) = app();
    stations.seed(viable_draft("desk")).unwrap();

    // The reader sees the registry.
    let (status, _) = send(&app, get("/api/stations", "reporter")).await;
    assert_eq!(status, StatusCode::OK);

    // And may not author, publish, or retire.
    for req in [
        post("/api/stations", "reporter", Some(&viable_draft("mine"))),
        post("/api/stations/desk/publish", "reporter", None),
        post("/api/stations/desk/retire", "reporter", None),
    ] {
        let uri = req.uri().to_string();
        let (status, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be gated");
    }
}
