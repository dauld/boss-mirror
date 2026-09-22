//! `GET /api/yard/borders` — the IT world map's rails, end-to-end
//! through the real router against the in-memory adapters (design
//! d2154293, car 2).
//!
//! What this pins:
//!
//! 1. **Every declared border is answered once, in layout order**, each
//!    carrying the four things a border is for: what one crossing IS in
//!    words, the rate this window against the previous, what is waiting
//!    to cross with each hold's reason, and the machine that moves it
//!    with its last-fired time.
//! 2. **A machine's silence is read from its OWN record.** The boarding
//!    rule's last firing comes from `cadence_firings`, the one place a
//!    firing is written — not from the crossings, which is what a
//!    surface that had no heartbeat would have to guess from.
//! 3. **Unknown is not zero.** A denied caller gets a well-formed
//!    payload; a border whose rows could not be read says so. Neither
//!    renders as a quiet rail.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::borders::BORDERS;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence, NewFiring};
use boss_jobs::delivery::{DeliveryPolicyRepository, InMemoryDeliveryPolicy};
use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, InMemoryDispatcherFirings, LastFiring,
};
use boss_jobs::http::{JobsApiState, router};
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

/// The boarding rule, declaring a 30-minute heartbeat — what the
/// gates -> track border's machine is judged silent against.
fn depth_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-board-on-dock-depth".into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        every_minutes: Some(30),
        at_times: None,
        min_dock_depth: Some(4),
        cooldown_minutes: Some(120),
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

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

fn app() -> (axum::Router, Arc<InMemoryJobs>, Arc<dyn CadenceRepository>) {
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
    // The dispatcher's firing record (b14afc48): auto-park-on-gate-green
    // fired an hour before NOW, which is what the shop-floor -> dock
    // border must answer with instead of the old "nothing records it".
    // (That hop was marshalling -> dock until backlog 94c6ffd0 put the
    // shop floor between the two — the same crossing, the same machine,
    // one territory further along.)
    let dispatcher_firings: Arc<dyn DispatcherFiringsRepository> =
        Arc::new(InMemoryDispatcherFirings::new(vec![(
            "auto-park-on-gate-green".to_string(),
            LastFiring {
                firing_id: "dispatcher:auto-park-on-gate-green:evt-1".into(),
                fired_on: "jobs.gate.green".into(),
                fired_at: t("2026-09-19T11:00:00Z"),
            },
        )]));
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations
        .seed(dock_station_row())
        .expect("seed the dock row");
    let state = JobsApiState {
        stations: Some(stations),
        cadence: Some(cadence.clone()),
        delivery: Some(delivery),
        dispatcher_firings: Some(dispatcher_firings),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy_client,
            Arc::new(boss_clock_client::FixedClockClient::new(
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
        )
    };
    (router(state), jobs, cadence)
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
        opened_at: None,
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

/// One car parked on the dock (waiting to board) and one train that
/// arrived this morning — enough traffic for a rate and a queue.
async fn seed(jobs: &InMemoryJobs) {
    let now = t(NOW);
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

fn border<'a>(v: &'a Value, from: &str, to: &str) -> &'a Value {
    v["borders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["from"] == from && b["to"] == to)
        .unwrap_or_else(|| panic!("no border {from} -> {to} in {v}"))
}

#[tokio::test]
async fn every_border_carries_its_flow_its_queue_and_its_machine() {
    let (app, jobs, _) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let pairs: Vec<(&str, &str)> = v["borders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["from"].as_str().unwrap(), b["to"].as_str().unwrap()))
        .collect();
    let declared: Vec<(&str, &str)> = BORDERS.iter().map(|s| (s.from, s.to)).collect();
    assert_eq!(pairs, declared, "the declared set, in layout order");
    for b in v["borders"].as_array().unwrap() {
        for key in [
            "crossing", "rate", "waiting", "holds", "machine", "state", "why",
        ] {
            assert!(
                b.get(key).is_some(),
                "{} -> {} lacks {key}: {b}",
                b["from"],
                b["to"]
            );
        }
        for key in ["name", "kind", "last_fired", "silent", "why"] {
            assert!(
                b["machine"].get(key).is_some(),
                "{} -> {} machine lacks {key}: {b}",
                b["from"],
                b["to"]
            );
        }
        assert!(
            !b["crossing"].as_str().unwrap().is_empty(),
            "a border says what ONE crossing of it is: {b}"
        );
    }
    assert_eq!(v["window_hours"], 24);
    assert_eq!(v["now"], NOW);

    // The dock has a car on it that has not boarded: the gates -> track
    // border is holding traffic, with the record's own reason on it.
    let boarding = border(&v, "gates", "track");
    assert_eq!(boarding["waiting"], 1, "{boarding}");
    assert_eq!(boarding["state"], "busy");
    let hold = &boarding["holds"][0];
    assert_eq!(hold["what"], "fix/a");
    assert!(
        hold["why"].as_str().unwrap().contains("boarding depth"),
        "{hold}"
    );

    // A train arrived this morning: one crossing of track -> arrivals,
    // a rate of one a day at a 24h window, and the stamp to prove it.
    let arrivals = border(&v, "track", "arrivals");
    assert_eq!(arrivals["rate"]["current"], 1.0);
    assert_eq!(arrivals["rate"]["samples"], 1);
    assert_eq!(arrivals["last_crossed"], "2026-09-19T09:00:00+00:00");
}

#[tokio::test]
async fn the_machine_is_read_from_its_own_firing_record_and_silence_is_trouble() {
    let (app, jobs, cadence) = app();
    seed(&jobs).await;

    // Never fired: the border says so, and says nothing about silence —
    // "cannot tell" must not render as "fine".
    let (_, v) = get(&app, "operator", "/api/yard/borders").await;
    let boarding = border(&v, "gates", "track");
    assert_eq!(boarding["machine"]["name"], "train-board-on-dock-depth");
    assert_eq!(boarding["machine"]["kind"], "cadence");
    assert_eq!(boarding["machine"]["last_fired"], Value::Null);
    assert_eq!(boarding["machine"]["silent"], Value::Null);
    assert_eq!(boarding["state"], "busy");

    // Fired three hours ago against a declared 30-minute heartbeat,
    // with a car waiting: the border is troubled, and the why names it.
    cadence
        .claim_firing(&NewFiring {
            firing_id: "f1".into(),
            rule_name: "train-board-on-dock-depth".into(),
            verb: "board".into(),
            basis: "queue-depth".into(),
            fired_at: t("2026-09-19T09:00:00Z"),
            detail: json!({}),
        })
        .await
        .unwrap();
    let (_, v) = get(&app, "operator", "/api/yard/borders").await;
    let boarding = border(&v, "gates", "track");
    assert_eq!(
        boarding["machine"]["last_fired"],
        "2026-09-19T09:00:00+00:00"
    );
    assert_eq!(boarding["machine"]["silent_for_minutes"], 180);
    assert_eq!(boarding["machine"]["expected_every_minutes"], 30);
    assert_eq!(boarding["machine"]["silent"], true);
    assert_eq!(boarding["state"], "troubled");
    assert!(
        boarding["why"]
            .as_str()
            .unwrap()
            .contains("silent for 180m"),
        "{boarding}"
    );

    // The dispatcher rule reads its OWN firing record (b14afc48) — the
    // hop that used to be the only one on the map unable to prove it
    // ran. Its silence stays unjudged: an event rule declares no
    // heartbeat, and a `silent: false` here would be a machine drawn
    // healthy on no evidence.
    let parked = border(&v, "shop-floor", "dock");
    assert_eq!(parked["machine"]["kind"], "dispatcher-rule");
    assert_eq!(
        parked["machine"]["last_fired"], "2026-09-19T11:00:00+00:00",
        "{parked}"
    );
    assert_eq!(parked["machine"]["expected_every_minutes"], Value::Null);
    assert_eq!(parked["machine"]["silent"], Value::Null, "{parked}");
    assert!(
        parked["machine"]["why"]
            .as_str()
            .unwrap()
            .contains("dispatcher_firings"),
        "{parked}"
    );
}

#[tokio::test]
async fn a_window_that_cannot_be_read_is_refused_and_a_denied_caller_gets_a_well_formed_map() {
    let (app, jobs, _) = app();
    seed(&jobs).await;
    let (status, _) = get(&app, "operator", "/api/yard/borders?window=nonsense").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = get(&app, "operator", "/api/yard/borders?window=48h").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["window_hours"], 48);

    // A caller the policy grants nothing: an empty, well-formed set —
    // not a 403, and not a false-empty rail with no shape at all.
    let (status, v) = get(&app, "nobody", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["borders"].as_array().unwrap().len(), BORDERS.len());
    let boarding = border(&v, "gates", "track");
    assert_eq!(boarding["waiting"], 0);
    assert_eq!(boarding["state"], "clear");
}
