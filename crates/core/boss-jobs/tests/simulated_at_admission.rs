//! A Job is created as real or simulated — fixed at admission, and
//! all event/data/state lives within the Job (stations.md lineage;
//! the epoch-trim invariant in 03-jobs.sql).
//!
//! Three contracts pinned here:
//!
//! - **Admission decides once.** `POST /api/jobs` accepts an optional
//!   `simulated` flag; a request arriving on a sim chain
//!   (`x-sim-origin`) admits the Job simulated regardless of the
//!   body — a sim chain cannot mint real work. Default: real.
//! - **Immutable thereafter.** The update path ignores any
//!   `simulated` on the wire (same posture as the path-authoritative
//!   `id`): a fake brew order does not become real because somebody
//!   PUT it.
//! - **Events inherit the packet's flag.** Every job/step event
//!   records `_simulated` from the Job's flag — the packet, not the
//!   transport context of the write, is the source of truth.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn operator_header() -> String {
    serde_json::json!({
        "id": "emp-ceo",
        "role": "ceo",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn test_kind() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "sim-flag-kind",
        "Sim flag kind",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Do the work".into(),
            ..Default::default()
        }],
    )
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_clock(Arc::new(boss_clock_client::WallClockClient))
}

fn app_with_clock(
    clock: Arc<dyn boss_clock_client::ClockClient>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(test_kind()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
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
        clock,
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn job_body(simulated: Option<bool>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "kind": "sim-flag-kind",
        "subject": { "subject_kind": "custom", "id": "doc-1" },
        "title": "A packet",
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": "standard",
        "metadata": {},
        "tags": [],
    });
    if let Some(flag) = simulated {
        body["simulated"] = serde_json::json!(flag);
    }
    body
}

async fn post_job(app: &axum::Router, body: &serde_json::Value) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().unwrap().to_string()
}

async fn get_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/jobs/{id}"))
                .header("x-boss-user", operator_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn admission_default_is_real() {
    let (app, jobs) = app();
    let id = post_job(&app, &job_body(None)).await;
    let detail = get_job(&app, &id).await;
    assert_eq!(detail["simulated"], false);

    // The create event carries the packet's flag — explicitly false,
    // so downstream never has to guess absent-vs-real.
    let created: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.created")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].payload["_simulated"], false);
    assert_eq!(created[0].payload["simulated"], false);
}

#[tokio::test]
async fn admission_accepts_an_explicit_simulated_flag() {
    let (app, jobs) = app();
    let id = post_job(&app, &job_body(Some(true))).await;
    let detail = get_job(&app, &id).await;
    assert_eq!(detail["simulated"], true);

    // Every event about the Job inherits the flag — the JOB_CREATED
    // state event AND the materialized steps' STEP_CREATED events.
    let events = jobs.recorded_events();
    let created = events
        .iter()
        .find(|e| e.kind == "jobs.job.created")
        .expect("job.created recorded");
    assert_eq!(created.payload["_simulated"], true);
    let step_created: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.step.created")
        .collect();
    assert!(!step_created.is_empty(), "spec step materialized");
    for e in &step_created {
        assert_eq!(
            e.payload["_simulated"], true,
            "step events inherit the Job's flag: {}",
            e.kind
        );
    }
}

#[tokio::test]
async fn a_sim_chain_cannot_mint_real_work() {
    let (app, _jobs) = app();
    // The body says nothing (and even an explicit false is
    // overridden): a request on a sim chain admits simulated.
    let id = boss_core::sim_origin::with_sim_chain(true, async {
        post_job(&app, &job_body(Some(false))).await
    })
    .await;
    let detail = get_job(&app, &id).await;
    assert_eq!(
        detail["simulated"], true,
        "x-sim-origin admission overrides the body"
    );
}

#[tokio::test]
async fn simulated_is_immutable_after_admission() {
    let (app, jobs) = app();
    let id = post_job(&app, &job_body(None)).await;

    // Round-trip the stored Job with the flag flipped — the update
    // path must keep the admission value.
    let mut detail = get_job(&app, &id).await;
    detail["simulated"] = serde_json::json!(true);
    detail.as_object_mut().unwrap().remove("steps");
    let resp = app
        .clone()
        .oneshot(
            Request::put(format!("/api/jobs/{id}"))
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(serde_json::to_vec(&detail).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = get_job(&app, &id).await;
    assert_eq!(after["simulated"], false, "update cannot flip the flag");

    // And the JOB_UPDATED event recorded the admission truth, not the
    // wire's attempted flip.
    let updated: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.updated")
        .collect();
    assert!(!updated.is_empty());
    for e in &updated {
        assert_eq!(e.payload["simulated"], false);
        assert_eq!(e.payload["_simulated"], false);
    }
}

#[tokio::test]
async fn step_writes_inherit_the_packet_flag_not_the_chain() {
    let (app, jobs) = app();
    // Simulated packet, created on a sim chain (the usual pairing).
    let id = boss_core::sim_origin::with_sim_chain(true, async {
        post_job(&app, &job_body(None)).await
    })
    .await;

    let detail = get_job(&app, &id).await;
    let step_id = detail["steps"][0]["id"].as_str().unwrap().to_string();

    // A real operator completes a step on the simulated packet — NO
    // sim chain on this request. The write is still simulated,
    // because the packet is.
    let resp = app
        .clone()
        .oneshot(
            Request::put(format!("/api/jobs/{id}/steps/{step_id}"))
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"status": "completed"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let events = jobs.recorded_events();
    let step_updated: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .collect();
    assert!(!step_updated.is_empty());
    for e in &step_updated {
        assert_eq!(
            e.payload["_simulated"], true,
            "a write to a simulated packet is simulated, whoever makes it"
        );
    }
    // The auto-close JOB_UPDATED (single-step kind: completing the
    // step closes the Job) inherits too.
    let job_updated: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.job.updated")
        .collect();
    for e in &job_updated {
        assert_eq!(e.payload["_simulated"], true);
    }
}

/// The sim-stamp retirement pin (David, 2026-08-22, packet a7a4cae5):
/// an event emitted while the deploy's clock is skewed to sim time
/// still carries WALL time on the record. Before this, every
/// clock-routed writer stamped `audit_log.timestamp` with the sim
/// instant while the allowlisted wall writers didn't, and one
/// incident's rows read hours apart. The sim timeline still reaches
/// the packet — as DATA: `opened_on` defaults from the sim-aware
/// clock port. The stamp does not.
#[tokio::test]
async fn a_skewed_sim_clock_never_reaches_the_record_stamp() {
    let sim_instant = chrono::DateTime::parse_from_rfc3339("2025-04-01T08:30:00Z")
        .unwrap()
        .to_utc();
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::FixedClockClient::new(boss_clock_client::ClockNow {
            now: sim_instant,
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
            warp_factor: None,
        }),
    );
    let (app, jobs) = app_with_clock(clock);

    let before = chrono::Utc::now();
    let id = post_job(&app, &job_body(None)).await;
    let after = chrono::Utc::now();

    // The business date follows the authoritative (sim) clock — the
    // timeline lives in the payload.
    let detail = get_job(&app, &id).await;
    assert_eq!(
        detail["opened_on"], "2025-04-01",
        "opened_on defaults from the sim-aware clock port"
    );

    // The record stamp is wall-clock now, not the skewed sim instant.
    let created: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.created")
        .collect();
    assert_eq!(created.len(), 1);
    let ts = created[0].timestamp;
    assert!(
        ts >= before && ts <= after,
        "record stamp must be wall-clock now, got {ts} (sim clock was {sim_instant})"
    );
    assert!(
        (ts - sim_instant).num_days().abs() > 300,
        "sanity: the skew is wide enough that a sim-time leak would be unmistakable"
    );
}
