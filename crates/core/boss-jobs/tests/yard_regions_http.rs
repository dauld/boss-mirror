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
        regate_hold_minutes: None,
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

/// A station every ready TASK lands on, whoever's packet it is — the
/// shape of the live `q.platform-admin.task`, which is where receiving
/// and marshalling counted the same packets (design 62de32ae decision 4).
fn task_station_row() -> boss_jobs::StationSpec {
    let mut s = boss_jobs::StationSpec::draft(
        "q.platform-admin.task",
        "Ready tasks",
        boss_jobs::StationKind::Constraint,
        boss_jobs::station_queue::StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(boss_jobs::station_queue::StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready],
                ..Default::default()
            }),
            ..Default::default()
        },
        t(NOW),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

/// The app with the platform protocols in its workflow registry — so the
/// inbound kinds can be named — and the task station beside the dock.
fn app_with_intake() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let kinds = Arc::new(boss_jobs::InMemoryWorkflows::new());
    for spec in boss_jobs::registry::seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations
        .seed(dock_station_row())
        .expect("seed the dock row");
    stations
        .seed(task_station_row())
        .expect("seed the task station");
    let state = JobsApiState {
        stations: Some(stations),
        kind_registry: Some(kinds as Arc<dyn boss_jobs::WorkflowRegistry>),
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
    (router(state), jobs)
}

/// An open backlog-item as the protocol admits one: `filed` (its
/// trigger) completed, then `triage` — completed or still ready — and,
/// once triaged, `build` ready.
async fn backlog_item(jobs: &InMemoryJobs, n: u32, triaged: bool) -> JobId {
    let id = Uuid::from_u128(0xB0B0_0000_0000_0000_0000_0000_0000_0000 + u128::from(n)).to_string();
    let item = job(
        "backlog-item",
        &id,
        &format!("item {n}"),
        JobStatus::Open,
        json!({}),
    );
    jobs.create_job_at(&item, t(NOW), &[]).await.unwrap();
    let mut filed = step(&item.id, "filed", "filed", StepStatus::Completed, json!({}));
    filed.kind = boss_jobs::regions::TRIGGER_STEP_KIND.into();
    let triage_status = if triaged {
        StepStatus::Completed
    } else {
        StepStatus::Ready
    };
    let mut steps = vec![
        filed,
        step(&item.id, "triage", "triage", triage_status, json!({})),
    ];
    if triaged {
        steps.push(step(
            &item.id,
            "build",
            "build",
            StepStatus::Ready,
            json!({}),
        ));
    }
    for s in steps {
        jobs.add_step_at(&s, t(NOW), &[]).await.unwrap();
    }
    item.id
}

/// THE PARTITION, END TO END (design 62de32ae decision 4): two
/// backlog-items standing at the one task station — one waiting on its
/// triage, one triaged and waiting on its build — are one packet in
/// receiving and one in marshalling, and the two borders out of the pair
/// wait on one each. Before, both regions counted both, and the world's
/// "waiting at the borders" summed the overlap.
#[tokio::test]
async fn a_packet_is_in_receiving_or_marshalling_and_never_both() {
    let (app, jobs) = app_with_intake();
    backlog_item(&jobs, 1, false).await;
    backlog_item(&jobs, 2, true).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], 1, "{receiving}");
    let marshalling = region(&v, "marshalling");
    assert_eq!(marshalling["count"], 1, "{marshalling}");

    let (status, b) = get(&app, "operator", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let waiting = |from: &str, to: &str| {
        b["borders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["from"] == from && x["to"] == to)
            .map(|x| x["waiting"].clone())
            .unwrap_or_else(|| panic!("no border {from} -> {to} in {b}"))
    };
    assert_eq!(waiting("receiving", "marshalling"), 1);
    assert_eq!(waiting("marshalling", "shop-floor"), 1);
}

/// RECEIVING READS PAST ITS PAGE (design 62de32ae decision 4): the
/// inbound read took ONE page of the handler's `MAX_LIMIT` (1000) and
/// counted what came back, so past it the count was a floor that did not
/// say it was one. One more untriaged item than a page holds is counted
/// in full — and the open-packet page the steps ride is a page too, so
/// the one past it is judged on steps read for it alone.
#[tokio::test]
async fn receiving_counts_every_inbound_packet_past_one_page() {
    const PAST_ONE_PAGE: u32 = 1001;
    let (app, jobs) = app_with_intake();
    for n in 0..PAST_ONE_PAGE {
        backlog_item(&jobs, n, false).await;
    }
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], PAST_ONE_PAGE, "{receiving}");
}

/// THE CROSSING INTO MARSHALLING IS THE INTAKE, END TO END (design
/// 62de32ae, car C). A backlog-item triaged this morning and CLOSED on
/// that act is one crossing of receiving -> marshalling, stamped at its
/// triage — which the handler can only see if it reads the steps of a
/// CLOSED inbound row, not just the open ones (it read none of them,
/// and the rail counted closures instead).
#[tokio::test]
async fn a_closed_packets_intake_is_a_crossing_into_marshalling() {
    let (app, jobs) = app_with_intake();
    let id = Uuid::from_u128(0xC105_0000_0000_0000_0000_0000_0000_0001).to_string();
    let item = job(
        "backlog-item",
        &id,
        "triaged and closed",
        JobStatus::Closed,
        json!({ "closed_at": "2026-09-19T10:00:00Z" }),
    );
    jobs.create_job_at(&item, t(NOW), &[]).await.unwrap();
    let mut filed = step(&item.id, "filed", "filed", StepStatus::Completed, json!({}));
    filed.kind = boss_jobs::regions::TRIGGER_STEP_KIND.into();
    filed.completed_at = Some(t("2026-09-18T20:00:00Z"));
    let mut triage = step(
        &item.id,
        "triage",
        "triage",
        StepStatus::Completed,
        json!({}),
    );
    triage.completed_at = Some(t("2026-09-19T08:00:00Z"));
    for s in [filed, triage] {
        jobs.add_step_at(&s, t(NOW), &[]).await.unwrap();
    }

    let (status, b) = get(&app, "operator", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let rail = b["borders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["from"] == "receiving" && x["to"] == "marshalling")
        .unwrap_or_else(|| panic!("no receiving -> marshalling rail in {b}"));
    assert_eq!(rail["rate"]["samples"], 1, "{rail}");
    assert_eq!(
        rail["last_crossed"], "2026-09-19T08:00:00+00:00",
        "stamped at the intake, not the close: {rail}"
    );
    assert_eq!(rail["waiting"], 0, "a closed packet stands nowhere: {rail}");
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
        stations: Some(stations),
        cadence: Some(cadence),
        delivery: Some(delivery),
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
async fn the_read_answers_every_region_each_with_count_state_and_trend() {
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

    // The red-PR train is under its gate, not yet merged — so it stands
    // at the GATES (design e765b3fc §2a, car R1), and the yard's own
    // block reading troubles the gates, where it is drawn. The track,
    // which holds merged trains, is empty.
    let gates = region(&v, "gates");
    assert_eq!(gates["count"], 1, "{gates}");
    assert_eq!(gates["state"], "troubled");
    assert!(
        gates["why"].as_str().unwrap().contains("train #470"),
        "{gates}"
    );
    let track = region(&v, "track");
    assert_eq!(track["count"], 0, "{track}");
    assert_eq!(track["state"], "clear");
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

/// THE STUCK BLOCK RIDES THE SAME READ (backlog 4142d821, design
/// cf820810 car 2): one entry per third, in the operator surface's order,
/// computed here so the HUD and `boss orient` read it rather than
/// recompute it. With no workflow registry wired the intake cannot be
/// read, and the queue-management third says so in `unknown` — a floor,
/// never a confident zero.
#[tokio::test]
async fn the_read_carries_a_stuck_block_per_third_and_an_unread_intake_is_unknown() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let thirds: Vec<&str> = v["stuck"]
        .as_array()
        .unwrap_or_else(|| panic!("no stuck block: {v}"))
        .iter()
        .map(|t| t["third"].as_str().unwrap())
        .collect();
    assert_eq!(thirds, ["queue-management", "actors-building", "delivery"]);
    let queue = &v["stuck"][0];
    assert!(
        queue["unknown"].as_array().unwrap().iter().any(|u| u
            .as_str()
            .unwrap()
            .contains("inbound kinds could not be read")),
        "{queue}"
    );
    assert!(
        queue["regions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "receiving"),
        "{queue}"
    );
    // The parked car declares no edge and nothing has landed: delivery
    // has nothing stuck and nothing waiting.
    let delivery = &v["stuck"][2];
    assert_eq!(delivery["stuck"], 0, "{delivery}");
    assert_eq!(delivery["waiting"], 0, "{delivery}");
    assert_eq!(delivery["oldest_hours"], Value::Null, "{delivery}");
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
    assert_eq!(v["regions"].as_array().unwrap().len(), REGIONS.len());
    assert_eq!(region(&v, "track")["count"], 0);
}

/// THE HOST THAT ANSWERED NOTHING IS STILL DRAWN (backlog 49ed87b4).
/// The runners used to be named only by the ops-requests they happened
/// to have answered, so a host that died was drawn nowhere — and an
/// absent glyph is indistinguishable from a runner that does not exist.
/// This pins the read that closes it: the handler asks the ESTATE
/// REGISTRY which hosts should have a runner, and each declared host
/// stands in the PLANT — the strip of machinery that serves every region
/// (design 62de32ae, decision 11) — whether or not it has said anything.
#[tokio::test]
async fn a_declared_runner_host_stands_on_the_map_with_no_request_of_its_own() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let node = |id: &str, roles: &[&str]| boss_jobs::port::EstateNodeInput {
        id: id.to_string(),
        label: id.to_string(),
        address: "10.20.0.15".to_string(),
        role: "forge".to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        cpu: None,
        memory_gb: None,
        disk_gb: None,
        notes: None,
    };
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    jobs.declare_estate_nodes(
        &[
            node("forge", &[boss_jobs::regions::OPS_RUNNER_ROLE]),
            node("w-1", &[]),
        ],
        &stamp,
    )
    .await
    .expect("the declaration lands");

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let machines = v["plant"].as_array().unwrap();
    let ids: Vec<&str> = machines.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["runner:host:forge"],
        "the declared host is drawn; a node that declares no runner is not invented"
    );
    assert_eq!(machines[0]["state"], "unknown");
    assert!(
        machines[0]["why"]
            .as_str()
            .unwrap()
            .contains("declares an ops-runner"),
        "{}",
        machines[0]["why"]
    );
}
