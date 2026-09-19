//! `GET /api/yard/regions` — the IT system map's KPI read, end-to-end
//! through the real router against the in-memory adapters (design
//! 0524fc95, car 1).
//!
//! What this pins:
//!
//! 1. **Eight regions, every one carrying `count`, `state` and `trend`**
//!    — the shape the map (car 2) and `boss orient` both read, so the
//!    yard, the CLI and the map answer with one voice.
//! 2. **The judgements are the yard's own.** A train the status read
//!    already calls blocked is the track's trouble here too — one
//!    predicate, not a second copy.
//! 3. **The window is a query parameter** (`window=48h`, default 24h),
//!    refused with a 400 when it cannot be read rather than silently
//!    answering the default.
//! 4. **A denied caller gets an empty, well-formed map**, not a 403 and
//!    not a false-empty — the same read gate every queue surface keeps.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence};
use boss_jobs::delivery::{DeliveryPolicyRepository, InMemoryDeliveryPolicy};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::regions::REGIONS;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const NOW: &str = "2026-09-19T12:00:00Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(role: &str) -> String {
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

fn depth_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-board-on-dock-depth".into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        every_minutes: None,
        at_times: None,
        min_dock_depth: Some(4),
        cooldown_minutes: Some(120),
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

/// The loading-dock row in miniature — the same shape
/// `yard_status_http.rs` wires, so the dock region reads the row the
/// handler reads in production.
fn dock_station_row() -> boss_jobs::StationSpec {
    let mut s = boss_jobs::StationSpec::draft(
        "loading-dock",
        "Loading dock — parked ship-a-change cars",
        boss_jobs::StationKind::Batch,
        boss_jobs::station_queue::StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(boss_jobs::station_queue::StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_unmarked: vec!["hold".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        t(NOW),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let cadence: Arc<dyn CadenceRepository> = Arc::new(InMemoryCadence::new(vec![depth_rule()]));
    let delivery: Arc<dyn DeliveryPolicyRepository> = Arc::new(InMemoryDeliveryPolicy::new(vec![]));
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations
        .seed(dock_station_row())
        .expect("seed the dock row");
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy: policy_client,
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
                now: t(NOW),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
        cadence: Some(cadence),
        delivery: Some(delivery),
        agent_budget: None,
    };
    (router(state), jobs)
}

fn job(kind: &str, id: &str, title: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.into(),
        workflow_version: 16,
        subject: Subject::new("custom", "s"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        due_on: None,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()),
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn step(job_id: &JobId, slug: &str, title: &str, status: StepStatus, metadata: Value) -> Step {
    let mut s = Step::new(*job_id, "task", title, 0);
    s.id = StepId::new();
    s.spec_slug = Some(slug.into());
    s.status = status;
    s.metadata = metadata;
    s
}

/// A red PR train in transit, a parked car on the dock, and an arrival
/// this morning.
async fn seed(jobs: &InMemoryJobs) {
    let now = t(NOW);
    let train = job(
        "pr-train",
        "11111111-1111-1111-1111-111111111111",
        "train #470",
        JobStatus::Open,
        json!({ "boarded_jobs": ["33333333-3333-3333-3333-333333333333"] }),
    );
    jobs.create_job_at(&train, now, &[]).await.unwrap();
    for s in [
        step(
            &train.id,
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:00:00Z" }),
        ),
        step(
            &train.id,
            "pr",
            "Open the batched PR",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:00:00Z" }),
        ),
        step(
            &train.id,
            "ci",
            "CI verdict",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:20:00Z", "result": "failing" }),
        ),
        step(
            &train.id,
            "merged",
            "Merged into main",
            StepStatus::Ready,
            json!({}),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    let car = job(
        "ship-a-change",
        "22222222-2222-2222-2222-222222222222",
        "A fix",
        JobStatus::Open,
        json!({ "branch": "fix/a" }),
    );
    jobs.create_job_at(&car, now, &[]).await.unwrap();
    for s in [
        step(
            &car.id,
            "gate",
            "Green, and observed working",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T10:00:00Z" }),
        ),
        step(
            &car.id,
            "review",
            "Open for review",
            StepStatus::Ready,
            json!({}),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    let arrived = job(
        "pr-train",
        "44444444-4444-4444-4444-444444444444",
        "train #469",
        JobStatus::Closed,
        json!({ "outcome": "arrived", "closed_at": "2026-09-19T09:00:00Z", "boarded_jobs": ["x"] }),
    );
    jobs.create_job_at(&arrived, now, &[]).await.unwrap();
}

async fn get(app: &axum::Router, role: &str, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("x-boss-user", user_header(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, v)
}

fn region<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no region {name} in {v}"))
}

#[tokio::test]
async fn the_read_answers_eight_regions_each_with_count_state_and_trend() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, REGIONS);
    for r in v["regions"].as_array().unwrap() {
        for key in ["count", "state", "trend", "why"] {
            assert!(r.get(key).is_some(), "{} lacks {key}: {r}", r["name"]);
        }
        for key in ["metric", "unit", "current", "previous"] {
            assert!(
                r["trend"].get(key).is_some(),
                "{} trend lacks {key}: {r}",
                r["name"]
            );
        }
    }
    assert_eq!(v["window_hours"], 24);
    assert_eq!(v["now"], NOW);

    // The track: the yard's own block reading, surfaced as trouble.
    let track = region(&v, "track");
    assert_eq!(track["count"], 1);
    assert_eq!(track["state"], "troubled");
    assert!(
        track["why"].as_str().unwrap().contains("train #470"),
        "{track}"
    );
    // The dock: the station row read, one car parked, below the depth.
    let dock = region(&v, "dock");
    assert_eq!(dock["count"], 1);
    assert_eq!(dock["state"], "clear");
    assert_eq!(dock["bound"], 4);
    // Arrivals: one this morning; per day at a 24h window.
    let arrivals = region(&v, "arrivals");
    assert_eq!(arrivals["count"], 1);
    assert_eq!(arrivals["trend"]["current"], 1.0);
    // Receiving: no workflow registry is wired, so the inbound kinds
    // cannot be named — troubled and null, never an empty region.
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], Value::Null);
    assert_eq!(receiving["state"], "troubled");
}

#[tokio::test]
async fn the_window_is_a_query_parameter_and_a_bad_one_is_refused() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions?window=48h").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["window_hours"], 48);
    assert_eq!(region(&v, "arrivals")["trend"]["current"], 0.5);

    let (status, v) = get(&app, "operator", "/api/yard/regions?window=soon").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn a_denied_caller_gets_an_empty_well_formed_map() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "guest", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["regions"].as_array().unwrap().len(), 8);
    assert_eq!(region(&v, "track")["count"], 0);
}
