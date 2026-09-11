//! `GET /api/stations/flow` — the drain rate `GET /api/stations/load`
//! says it is missing.
//!
//! `stations_load`'s own header: *"Depth is close to meaningless
//! without a drain rate — ten role queues each read exactly 48 the day
//! this was written and none was a bottleneck, they were a bug."* This
//! endpoint is that rate, and the contracts it has to keep are the
//! ones that stop it becoming a number the surface invented:
//!
//! 1. **Arrivals and departures come from the log, not a new stamp** —
//!    `step.ready.<kind>` and `step.done.<kind>`, the two events a
//!    step-waiting station's membership already turns on.
//! 2. **The window is wall clock and it is a filter, not a hint** — an
//!    event older than the window is not counted.
//! 3. **A station whose membership the log cannot be attributed to
//!    reports blind, with the clause that blinded it named** — never a
//!    zero, which would read as "nothing is waiting".
//! 4. **A countable station with no traffic reports zero** — absence is
//!    a fact, and it is the reading that says a queue is not draining.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryStations, JobsRepository, StationKind, StationSpec};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const STEP_A: &str = "11111111-0000-4000-8000-000000000001";
const STEP_B: &str = "22222222-0000-4000-8000-000000000002";
const STEP_OLD: &str = "33333333-0000-4000-8000-000000000003";

// SERIALISE THE REAL TYPE, never a copy of its wire shape (7c3649e2).
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

/// The projected constraint station shape: `q.<role>.<step-kind>`.
fn constraint_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "q.platform-admin.task",
        "platform-admin — task",
        StationKind::Constraint,
        StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_equals: BTreeMap::from([(
                    "authority_role".to_string(),
                    "platform-admin".to_string(),
                )]),
                ..Default::default()
            }),
            ..Default::default()
        },
        Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

/// A queue whose membership turns on Job metadata — the loading dock's
/// shape. Countable depth, uncountable flow.
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
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        },
        Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

/// A countable station that saw no traffic at all in the window.
fn quiet_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "q.cto.sign-off",
        "cto — sign-off",
        StationKind::Constraint,
        StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(StepMatch {
                kind: Some("sign-off".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        },
        Utc::now(),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "a packet with obligations".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

fn step(id: &str, status: StepStatus) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "task".into(),
        title: "Measure the claim".into(),
        spec_slug: Some("triage".into()),
        assignee_id: None,
        status,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "authority_role": "platform-admin" }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

fn step_event(kind: &str, step_id: &str, at: DateTime<Utc>) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: at,
        source: "jobs".into(),
        kind: kind.into(),
        payload: serde_json::json!({
            "job_id": JOB,
            "step_id": step_id,
            "kind": "task",
        }),
    }
}

async fn app() -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let stations = Arc::new(InMemoryStations::new());
    for spec in [constraint_station(), dock_station(), quiet_station()] {
        stations.seed(spec).expect("station row lands");
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();

    let now = Utc::now();
    jobs.create_job_at(&packet(), now - Duration::hours(30), &[])
        .await
        .expect("packet lands");
    // Two obligations arrived inside the window; one of them was
    // served inside it. The third arrived before the window opened and
    // must not be counted.
    for (id, status, at) in [
        (STEP_A, StepStatus::Ready, now - Duration::hours(3)),
        (STEP_B, StepStatus::Completed, now - Duration::hours(2)),
        (STEP_OLD, StepStatus::Ready, now - Duration::hours(48)),
    ] {
        jobs.add_step_at(&step(id, status), at, &[])
            .await
            .expect("step lands");
    }
    jobs.record_events(&[
        step_event("step.ready.task", STEP_A, now - Duration::hours(3)),
        step_event("step.ready.task", STEP_B, now - Duration::hours(4)),
        step_event("step.done.task", STEP_B, now - Duration::hours(2)),
        step_event("step.ready.task", STEP_OLD, now - Duration::hours(48)),
        step_event("step.done.task", STEP_OLD, now - Duration::hours(47)),
    ])
    .await
    .expect("events record");

    let state = JobsApiState {
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: Some(stations),
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: Utc::now(),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
        cadence: None,
        delivery: None,
    };
    router(state)
}

async fn flow(app: &axum::Router, query: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/stations/flow{query}"))
                .header("x-boss-user", user_header("emp-david", "operator"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn row<'a>(body: &'a serde_json::Value, station: &str) -> &'a serde_json::Value {
    body["data"]
        .as_array()
        .expect("data is a collection")
        .iter()
        .find(|r| r["station"] == station)
        .unwrap_or_else(|| panic!("no row for {station}"))
}

/// Contracts 1, 2 and 4, on one read.
#[tokio::test]
async fn a_countable_station_reports_what_the_log_recorded_inside_the_window() {
    let app = app().await;
    let body = flow(&app, "?window_hours=24").await;
    assert_eq!(body["window_hours"], 24);

    let busy = row(&body, "q.platform-admin.task");
    assert_eq!(busy["basis"], "step-events");
    // STEP_A and STEP_B arrived in the window; STEP_OLD did not.
    assert_eq!(busy["arrived"], 2, "arrivals inside the window");
    assert_eq!(busy["served"], 1, "departures inside the window");
    assert_eq!(busy["net"], 1, "the queue grew by one");
    assert!(busy["unavailable_reason"].is_null());

    // Absence is a fact: a countable queue with no traffic reads zero,
    // which is the reading that says it is not draining.
    let quiet = row(&body, "q.cto.sign-off");
    assert_eq!(quiet["basis"], "step-events");
    assert_eq!(quiet["arrived"], 0);
    assert_eq!(quiet["served"], 0);
}

/// Contract 3: blind is stated, never rendered as a zero.
#[tokio::test]
async fn a_station_the_log_cannot_be_attributed_to_names_the_clause() {
    let app = app().await;
    let body = flow(&app, "?window_hours=24").await;
    let dock = row(&body, "test-dock");
    assert_eq!(dock["basis"], "unavailable");
    assert!(dock["arrived"].is_null(), "no invented arrival count");
    assert!(dock["served"].is_null(), "no invented service count");
    assert!(dock["net"].is_null());
    assert!(
        dock["unavailable_reason"]
            .as_str()
            .expect("a named reason")
            .contains("metadata"),
        "the reason names the clause that blinded it"
    );
}

/// Contract 2, from the other side: widen the window and the older
/// pair enters the count. A window that did not filter would have read
/// the same both times.
#[tokio::test]
async fn a_wider_window_admits_the_older_traffic() {
    let app = app().await;
    let body = flow(&app, "?window_hours=72").await;
    let busy = row(&body, "q.platform-admin.task");
    assert_eq!(busy["arrived"], 3);
    assert_eq!(busy["served"], 2);
    assert_eq!(busy["net"], 1);
}

/// The default window is stated on the envelope rather than assumed by
/// the caller — a surface that renders "last 24h" has to be able to
/// read which window it actually got.
#[tokio::test]
async fn the_envelope_names_its_own_window() {
    let app = app().await;
    let body = flow(&app, "").await;
    assert!(
        body["window_hours"].as_i64().is_some_and(|h| h > 0),
        "the envelope names the window it counted"
    );
    assert!(
        body["since"].as_str().is_some(),
        "and the instant it opened"
    );
}
