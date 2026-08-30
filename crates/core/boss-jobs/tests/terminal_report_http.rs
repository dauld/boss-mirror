//! `GET /api/workflows/{kind}/terminal-report` — Tier 1 of the
//! experiments program (docs/design/network-experiments.md): measure
//! what version pinning already records.
//!
//! Per workflow version of one kind, over the jobs projection alone:
//! packet count (total + by status), outcome distribution over CLOSED
//! packets, and open→close cycle-time stats (median + p90 in days,
//! from `opened_on` / `closed_on` — the dates the row actually
//! carries and the rebuilder reproduces). Versions sort newest first.
//!
//! This replaces the ad-hoc SQL that measured the brewery protocol
//! iterations (tasting-panel v1→v2, keg-return v1→v4, morning-brew
//! v1→v2) — which is why the `simulated` filter defaults to `all` and
//! is echoed back: the brewery experiments are simulated traffic and
//! must stay visible in the report that exists to measure them.

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

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
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    // The report wears the same read gate as every other GET under
    // /api/workflows: Action::Read on Resource::workflow. "reporter"
    // holds it; "outsider" holds nothing.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("reporter", Action::Read, Resource::workflow(), Scope::All)
            .build(),
    );
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
    };
    (router(state), jobs)
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[allow(clippy::too_many_arguments)]
fn packet(
    kind: &str,
    version: i32,
    status: JobStatus,
    opened: NaiveDate,
    closed: Option<NaiveDate>,
    outcome: Option<&str>,
    simulated: bool,
) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        kind: kind.to_string(),
        workflow_version: version,
        subject: Subject::new("custom", "subj-1"),
        title: format!("{kind} packet"),
        owner_id: "emp-1".into(),
        status,
        priority: Priority::Standard,
        opened_on: opened,
        due_on: None,
        closed_on: closed,
        metadata: match outcome {
            Some(o) => serde_json::json!({ "outcome": o }),
            None => serde_json::json!({}),
        },
        tags: vec![],
        simulated,
    }
}

async fn seed(jobs: &InMemoryJobs, packets: Vec<Job>) {
    for p in packets {
        jobs.create_job(&p).await.unwrap();
    }
}

async fn get_report(
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

/// The tasting-panel week, in miniature: v2 superseded v1 mid-week,
/// in-flight packets stayed pinned, and the two versions closed with
/// different outcome mixes.
fn tasting_panel_fixture() -> Vec<Job> {
    vec![
        // v2: three dated closes (cycle days 1, 3, 5), one still open,
        // and one close the operator never dated — it counts as closed
        // and carries its outcome, but contributes no cycle sample.
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 10),
            Some(d(2026, 8, 11)),
            Some("approved"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 10),
            Some(d(2026, 8, 13)),
            Some("approved"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 12),
            Some(d(2026, 8, 17)),
            Some("rejected"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Open,
            d(2026, 8, 15),
            None,
            None,
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 14),
            None,
            Some("approved"),
            false,
        ),
        // v1: two dated closes (cycle days 2, 8) — one with no
        // declared outcome (the catch-all close) — and a cancellation,
        // which is terminal but NOT closed, so it stays out of the
        // outcome and cycle measurements.
        packet(
            "tasting-panel",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 3)),
            Some("rejected"),
            false,
        ),
        packet(
            "tasting-panel",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 9)),
            None,
            false,
        ),
        packet(
            "tasting-panel",
            1,
            JobStatus::Cancelled,
            d(2026, 8, 2),
            None,
            None,
            false,
        ),
        // A different kind entirely — must not leak into the report.
        packet(
            "keg-return",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 2)),
            Some("returned"),
            false,
        ),
    ]
}

fn approx(v: &serde_json::Value, want: f64) -> bool {
    v.as_f64().is_some_and(|x| (x - want).abs() < 1e-9)
}

#[tokio::test]
async fn two_versions_report_their_outcome_mixes_newest_first() {
    let (app, jobs) = app();
    seed(&jobs, tasting_panel_fixture()).await;

    let (status, body) = get_report(
        &app,
        "/api/workflows/tasting-panel/terminal-report",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body:#}");
    assert_eq!(body["kind"], "tasting-panel");
    assert_eq!(
        body["simulated"], "all",
        "the default is labeled, not implied"
    );
    assert!(body["since"].is_null());

    let versions = body["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);

    let v2 = &versions[0];
    assert_eq!(v2["version"], 2, "versions sort newest first");
    assert_eq!(v2["total"], 5);
    assert_eq!(
        v2["by_status"],
        serde_json::json!({ "closed": 4, "open": 1 })
    );
    assert_eq!(
        v2["outcomes"],
        serde_json::json!({ "approved": 3, "rejected": 1 })
    );
    assert_eq!(v2["closed_without_outcome"], 0);
    // Cycle stats over the three DATED closes only — the undated
    // close is a closed packet without a measurable cycle.
    assert_eq!(v2["cycle_time_days"]["samples"], 3);
    assert!(
        approx(&v2["cycle_time_days"]["median"], 3.0),
        "median of [1,3,5]: {:#}",
        v2["cycle_time_days"]
    );
    assert!(
        approx(&v2["cycle_time_days"]["p90"], 4.6),
        "percentile_cont(0.9) of [1,3,5]: {:#}",
        v2["cycle_time_days"]
    );

    let v1 = &versions[1];
    assert_eq!(v1["version"], 1);
    assert_eq!(v1["total"], 3);
    assert_eq!(
        v1["by_status"],
        serde_json::json!({ "cancelled": 1, "closed": 2 })
    );
    assert_eq!(v1["outcomes"], serde_json::json!({ "rejected": 1 }));
    assert_eq!(
        v1["closed_without_outcome"], 1,
        "the catch-all close (no declared outcome) is counted, not dropped"
    );
    assert_eq!(v1["cycle_time_days"]["samples"], 2);
    assert!(approx(&v1["cycle_time_days"]["median"], 5.0));
    assert!(approx(&v1["cycle_time_days"]["p90"], 7.4));
}

#[tokio::test]
async fn simulated_filter_partitions_and_is_labeled() {
    let (app, jobs) = app();
    seed(
        &jobs,
        vec![
            packet(
                "keg-return",
                4,
                JobStatus::Closed,
                d(2026, 8, 10),
                Some(d(2026, 8, 11)),
                Some("returned"),
                false,
            ),
            packet(
                "keg-return",
                4,
                JobStatus::Closed,
                d(2026, 8, 10),
                Some(d(2026, 8, 12)),
                Some("returned"),
                true,
            ),
            packet(
                "keg-return",
                4,
                JobStatus::Closed,
                d(2026, 8, 10),
                Some(d(2026, 8, 15)),
                Some("lost"),
                true,
            ),
        ],
    )
    .await;

    // Default: every packet, labeled "all" — the brewery experiments
    // are simulated traffic and must be visible.
    let (status, body) = get_report(
        &app,
        "/api/workflows/keg-return/terminal-report",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["simulated"], "all");
    assert_eq!(body["versions"][0]["total"], 3);

    let (status, body) = get_report(
        &app,
        "/api/workflows/keg-return/terminal-report?simulated=true",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["simulated"], "true");
    assert_eq!(body["versions"][0]["total"], 2);
    assert_eq!(
        body["versions"][0]["outcomes"],
        serde_json::json!({ "lost": 1, "returned": 1 })
    );

    let (status, body) = get_report(
        &app,
        "/api/workflows/keg-return/terminal-report?simulated=false",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["simulated"], "false");
    assert_eq!(body["versions"][0]["total"], 1);

    let (status, _) = get_report(
        &app,
        "/api/workflows/keg-return/terminal-report?simulated=maybe",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "simulated is true|false|all"
    );
}

#[tokio::test]
async fn a_kind_with_no_packets_reports_an_empty_200() {
    // Absence of packets is a fact the report states, not an error —
    // a freshly published protocol has a terminal report; it is empty.
    let (app, _jobs) = app();
    let (status, body) = get_report(
        &app,
        "/api/workflows/never-used/terminal-report",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "never-used");
    assert_eq!(body["versions"], serde_json::json!([]));
}

#[tokio::test]
async fn since_filters_on_opened_date() {
    let (app, jobs) = app();
    seed(
        &jobs,
        vec![
            packet(
                "morning-brew",
                2,
                JobStatus::Closed,
                d(2026, 8, 1),
                Some(d(2026, 8, 2)),
                Some("shipped"),
                false,
            ),
            packet(
                "morning-brew",
                2,
                JobStatus::Closed,
                d(2026, 8, 10),
                Some(d(2026, 8, 12)),
                Some("shipped"),
                false,
            ),
        ],
    )
    .await;

    let (status, body) = get_report(
        &app,
        "/api/workflows/morning-brew/terminal-report?since=2026-08-05",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["since"], "2026-08-05", "the window is echoed back");
    assert_eq!(body["versions"][0]["total"], 1);
    assert_eq!(body["versions"][0]["cycle_time_days"]["samples"], 1);
    assert!(approx(
        &body["versions"][0]["cycle_time_days"]["median"],
        2.0
    ));

    let (_, full) = get_report(
        &app,
        "/api/workflows/morning-brew/terminal-report",
        "emp-1",
        "reporter",
    )
    .await;
    assert_eq!(full["versions"][0]["total"], 2);
}

#[tokio::test]
async fn the_report_wears_the_workflow_read_gate() {
    let (app, jobs) = app();
    seed(&jobs, tasting_panel_fixture()).await;
    let (status, _) = get_report(
        &app,
        "/api/workflows/tasting-panel/terminal-report",
        "emp-2",
        "outsider",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "same gate as every sibling GET under /api/workflows — nothing weaker"
    );
}
