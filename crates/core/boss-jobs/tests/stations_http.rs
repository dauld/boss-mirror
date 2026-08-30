//! Station read surfaces + the claim-CAS capability gate
//! (docs/design/stations.md, Q1–Q4 ratified).
//!
//! - `GET /api/stations` lists the active registry rows; a caller
//!   whose job-read scope is None sees an empty collection (one
//!   policy path with /api/jobs).
//! - `GET /api/stations/{name}/queue` evaluates the predicate over
//!   the caller's policy-scoped open Jobs and orders by the
//!   discipline; the envelope names the discipline and carries the
//!   advisory `over_limit`.
//! - `POST .../claim?station=<name>` enforces the station's
//!   capability (Class-registry role vocabulary) and membership
//!   BEFORE the CAS; a claim without a station keeps today's
//!   behavior.

use boss_policy_client::types::{AccessTier, User};
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::{DisciplineKey, SELF, StationPredicate, StepMatch};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationCapability, StationKind,
    StationRegistry, StationSpec, WorkflowRegistry,
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
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

fn car_kind() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "car-kind",
        "Car kind",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "review".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Open for review".into(),
            ..Default::default()
        }],
    )
}

/// The dock, in miniature: open car-kind packets with a branch and no
/// train, whose review step is open.
fn dock_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "test-dock",
        "Test dock",
        StationKind::Batch,
        StationPredicate {
            kind: Some("car-kind".into()),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![
                    boss_core::job::StepStatus::Ready,
                    boss_core::job::StepStatus::Active,
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.wip_limit = Some(2);
    s
}

fn gated_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "brewer-gate",
        "Brewer-gated station",
        StationKind::Constraint,
        StationPredicate {
            kind: Some("car-kind".into()),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.capability = Some(StationCapability {
        roles: vec!["head-brewer".into()],
    });
    s
}

/// The filer's watchlist: the platform's one per-actor station row
/// (118-watchlist-station.sql). One row, every actor — `@me` binds to
/// whoever is asking — and a terminal window so a packet's outcome is
/// still there when the filer comes looking for it.
fn watchlist_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "my-watchlist",
        "My watchlist — packets I filed",
        StationKind::Actor,
        StationPredicate {
            metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s.discipline = vec![DisciplineKey::Recency];
    s.terminal_window_days = Some(14);
    s
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(car_kind()).unwrap();
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(dock_station()).unwrap();
    stations.seed(gated_station()).unwrap();
    stations.seed(watchlist_station()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .allow("head-brewer", Action::Read, Resource::job(), Scope::All)
            .allow("head-brewer", Action::Update, Resource::step(), Scope::All)
            .allow("reporter", Action::Read, Resource::job(), Scope::All)
            // A guest that policy would let read everything: the only
            // thing standing between them and somebody's watchlist is
            // the placeholder failing to bind, which is what the guest
            // test below is actually asserting.
            .allow("guest", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: Some(stations as Arc<dyn StationRegistry>),
        jobs: jobs.clone(),
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
    (router(state), jobs)
}

async fn post_car(
    app: &axum::Router,
    branch: &str,
    priority: &str,
    opened_on: &str,
    boarded: bool,
) -> String {
    let mut metadata = serde_json::json!({ "branch": branch });
    if boarded {
        metadata["train"] = serde_json::json!("t1");
    }
    let body = serde_json::json!({
        "kind": "car-kind",
        "subject": { "subject_kind": "custom", "id": branch },
        "title": format!("car {branch}"),
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": priority,
        "opened_on": opened_on,
        "metadata": metadata,
        "tags": [],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-ceo", "ceo"))
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().unwrap().to_string()
}

async fn get_json(
    app: &axum::Router,
    path: &str,
    user: &str,
    role: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("x-boss-user", user_header(user, role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn list_stations_returns_active_rows() {
    let (app, _jobs) = app();
    let (status, v) = get_json(&app, "/api/stations", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 3);
    let names: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["brewer-gate", "my-watchlist", "test-dock"],
        "name-ordered"
    );
    // The rows carry the queue metadata a lens needs.
    assert_eq!(v["data"][2]["kind"], "batch");
    assert_eq!(
        v["data"][2]["discipline"],
        serde_json::json!(["priority", "age"])
    );
    assert_eq!(v["data"][2]["wip_limit"], 2);
    // The per-actor row is a registry row like any other — the
    // placeholder is visible in the listing, unbound.
    assert_eq!(v["data"][1]["kind"], "actor");
    assert_eq!(v["data"][1]["terminal_window_days"], 14);
    assert_eq!(
        v["data"][1]["predicate"]["metadata_equals"]["submitted_by"],
        "@me"
    );
}

#[tokio::test]
async fn a_denied_caller_sees_no_stations() {
    let (app, _jobs) = app();
    // "intern" holds no job-read grant: scope predicate is None.
    let (status, v) = get_json(&app, "/api/stations", "emp-x", "intern").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 0);
    assert_eq!(v["data"], serde_json::json!([]));
}

#[tokio::test]
async fn queue_is_evaluated_ordered_and_advisory_flagged() {
    let (app, _jobs) = app();
    // Three parked cars + one boarded (not a member): the queue
    // orders by priority then age, reports its discipline, and flags
    // the advisory wip_limit breach (limit 2, 3 members).
    let standard_old = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let urgent_new = post_car(&app, "feat/b", "urgent", "2026-08-10", false).await;
    let urgent_old = post_car(&app, "feat/c", "urgent", "2026-08-03", false).await;
    let _boarded = post_car(&app, "feat/d", "emergency", "2026-08-01", true).await;

    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["station"], "test-dock");
    assert_eq!(v["discipline"], serde_json::json!(["priority", "age"]));
    assert_eq!(v["total"], 3, "the boarded car is not a member");
    let order: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec![
            urgent_old.as_str(),
            urgent_new.as_str(),
            standard_old.as_str()
        ],
        "priority first, then age"
    );
    assert_eq!(v["wip_limit"], 2);
    assert_eq!(v["over_limit"], true, "advisory: reported, nothing dropped");
}

#[tokio::test]
async fn queue_of_unknown_station_is_404_and_denied_caller_sees_empty() {
    let (app, _jobs) = app();
    let (status, _) = get_json(&app, "/api/stations/no-such/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-x", "intern").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 0, "policy-scoped universe: None sees nothing");
}

// ---------------------------------------------------------------------------
// The filer's watchlist — the per-actor station read
// ---------------------------------------------------------------------------

/// A filed packet, seeded straight into the repository so the test can
/// place it at an exact point in its life (open, or closed with an
/// outcome N days ago) without driving a Workflow to get there.
fn filed_packet(who: &str, title: &str, opened_days_ago: i64, closed: Option<(i64, &str)>) -> Job {
    let today = chrono::Utc::now().date_naive();
    let mut j = Job::new(
        "user-feedback",
        boss_core::job::Subject::new("custom", "/ux/jobs"),
        title,
        who,
        boss_core::job::Priority::Standard,
        today - chrono::Duration::days(opened_days_ago),
    );
    j.status = JobStatus::Open;
    j.metadata = serde_json::json!({ "submitted_by": who, "message": "…" });
    if let Some((days_ago, outcome)) = closed {
        // What `close_job_on_terminal` leaves behind.
        j.status = JobStatus::Closed;
        j.closed_on = Some(today - chrono::Duration::days(days_ago));
        j.metadata["outcome"] = serde_json::json!(outcome);
    }
    j
}

async fn get_json_anonymous(app: &axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_watchlist_holds_my_open_and_recently_closed_packets() {
    let (app, jobs) = app();
    let mine_open = filed_packet("emp-r", "Column picker forgets my choice", 9, None);
    let mine_closed = filed_packet("emp-r", "Dark mode contrast", 20, Some((1, "completed")));
    let mine_stale = filed_packet("emp-r", "Ancient gripe", 90, Some((30, "declined")));
    let theirs = filed_packet("emp-other", "Not mine", 1, None);
    for j in [&mine_open, &mine_closed, &mine_stale, &theirs] {
        jobs.create_job(j).await.unwrap();
    }

    let (status, v) = get_json(
        &app,
        "/api/stations/my-watchlist/queue",
        "emp-r",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["station"], "my-watchlist");
    assert_eq!(v["discipline"], serde_json::json!(["recency"]));
    assert_eq!(
        v["terminal_window_days"], 14,
        "the envelope names the window, so a reader can see why a \
         closed packet is in a queue"
    );
    assert_eq!(
        v["total"], 2,
        "mine, open and recently closed — not theirs, not the one that aged out"
    );

    // Newest activity first: the packet that closed yesterday leads the
    // one opened nine days ago, even though both are standard priority
    // and the closed one is the older Job.
    let titles: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["Dark mode contrast", "Column picker forgets my choice"]
    );

    // The terminal state travels WITH the packet — it is the whole
    // reason a closed entry is still on the list.
    assert_eq!(v["data"][0]["status"], "closed");
    assert_eq!(v["data"][0]["metadata"]["outcome"], "completed");
}

#[tokio::test]
async fn one_row_serves_every_actor_and_no_actor_sees_another_s() {
    let (app, jobs) = app();
    jobs.create_job(&filed_packet("emp-r", "Mine", 1, None))
        .await
        .unwrap();
    jobs.create_job(&filed_packet("emp-s", "Theirs", 1, None))
        .await
        .unwrap();

    for (who, expected) in [("emp-r", "Mine"), ("emp-s", "Theirs")] {
        let (status, v) = get_json(&app, "/api/stations/my-watchlist/queue", who, "reporter").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["total"], 1, "{who} sees exactly their own filing");
        assert_eq!(v["data"][0]["title"], expected);
    }
}

#[tokio::test]
async fn a_guest_gets_an_empty_watchlist_not_everyone_s() {
    let (app, jobs) = app();
    jobs.create_job(&filed_packet("emp-r", "Mine", 1, None))
        .await
        .unwrap();
    // A packet whose metadata literally holds the placeholder. If the
    // self clause ever went unbound, this is what would leak.
    jobs.create_job(&filed_packet("@me", "Literal placeholder", 1, None))
        .await
        .unwrap();

    let (status, v) = get_json_anonymous(&app, "/api/stations/my-watchlist/queue").await;
    assert_eq!(status, StatusCode::OK, "read-only and guest-safe: no 401");
    assert_eq!(
        v["station"], "my-watchlist",
        "the station still describes itself"
    );
    assert_eq!(
        v["total"], 0,
        "nobody to bind @me to means an EMPTY queue, never a wide one"
    );
    assert_eq!(v["data"], serde_json::json!([]));
}

#[tokio::test]
async fn a_station_without_a_window_still_holds_only_in_flight_packets() {
    // The regression guard on the widened universe: the dock is
    // unchanged by any of this — closed packets have departed the
    // network and a routing station has nothing to say about them.
    let (app, jobs) = app();
    let id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let (status, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 1);
    assert_eq!(v["terminal_window_days"], serde_json::Value::Null);

    let (all, _) = jobs
        .list_jobs(&boss_jobs::port::JobFilter::default(), 100, 0)
        .await
        .unwrap();
    let mut job = all
        .into_iter()
        .find(|j| j.id.to_string() == id)
        .expect("job exists");
    job.status = JobStatus::Closed;
    job.closed_on = Some(chrono::Utc::now().date_naive());
    jobs.update_job(&job).await.unwrap();

    let (_, v) = get_json(&app, "/api/stations/test-dock/queue", "emp-ceo", "ceo").await;
    assert_eq!(v["total"], 0, "the packet left the dock when it closed");
}

async fn claim(
    app: &axum::Router,
    job_id: &str,
    step_id: &str,
    query: &str,
    user: &str,
    role: &str,
) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post(format!("/api/jobs/{job_id}/steps/{step_id}/claim{query}"))
                .header("x-boss-user", user_header(user, role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn first_step_id(app: &axum::Router, job_id: &str) -> String {
    let (_, detail) = get_json(app, &format!("/api/jobs/{job_id}"), "emp-ceo", "ceo").await;
    detail["steps"][0]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn claim_gate_enforces_the_station_capability() {
    let (app, _jobs) = app();
    let job_id = post_car(&app, "feat/a", "standard", "2026-08-01", false).await;
    let step_id = first_step_id(&app, &job_id).await;

    // The ceo is not in the station's capability roles: 403, and the
    // step stays unclaimed (the gate runs before the CAS).
    let denied = claim(
        &app,
        &job_id,
        &step_id,
        "?station=brewer-gate",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);

    // A head-brewer is admitted; the CAS then decides as usual.
    let won = claim(
        &app,
        &job_id,
        &step_id,
        "?station=brewer-gate",
        "emp-hb",
        "head-brewer",
    )
    .await;
    assert_eq!(won, StatusCode::OK);
}

#[tokio::test]
async fn claim_from_a_station_the_packet_is_not_at_conflicts() {
    let (app, _jobs) = app();
    // Boarded car: has a train marker, so it is NOT at the dock.
    let job_id = post_car(&app, "feat/x", "standard", "2026-08-01", true).await;
    let step_id = first_step_id(&app, &job_id).await;

    let status = claim(
        &app,
        &job_id,
        &step_id,
        "?station=test-dock",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Unknown station names 404 rather than silently skipping the gate.
    let status = claim(
        &app,
        &job_id,
        &step_id,
        "?station=nowhere",
        "emp-ceo",
        "ceo",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A stationless claim keeps today's behavior: CAS only.
    let status = claim(&app, &job_id, &step_id, "", "emp-ceo", "ceo").await;
    assert_eq!(status, StatusCode::OK);
}
