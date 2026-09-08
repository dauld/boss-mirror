//! `GET /api/jobs?closed_within=<days>` — the terminal retention
//! window at the layer that actually consumes it.
//!
//! WHY THIS FILE EXISTS. A board renders each card in the column of its
//! current step, so terminal packets have to be fetched or the terminal
//! columns are empty. `TriageBoard.svelte` therefore asked for
//! `kind=user-feedback&limit=200` with no status filter at all, and got
//! all 173 feedback packets in order to display the 14 that are live —
//! 92% finished work, and 27 rows short of silently truncating at its
//! own limit. That is what David saw as "so many jobs on the Feedback
//! board" while the actual backlog was fourteen.
//!
//! The adapters are covered in `postgres_filter.rs` and the
//! `in_memory` unit tests. What is only visible HERE is the two things
//! between the query string and the filter:
//!
//!   1. The param is named `closed_within` and reaches
//!      `filter.closed_since`. `postgres_filter.rs` opens by describing
//!      exactly this failure mode — client says `subject_id`, handler
//!      reads `asset_id`, filter silently falls through and the call
//!      returns everything. A retention window that falls through
//!      returns the whole history, which looks like working software.
//!
//!   2. Days are resolved into a date against the AUTHORITATIVE clock,
//!      not the process wall clock. BOSS runs simulated time; a window
//!      measured against wall time would cut a simulated board at the
//!      wrong place and there would be nothing to see in the response
//!      that said so.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

/// An app whose "today" is frozen at `today`, so a window measured in
/// days lands on an asserted date instead of drifting with the suite's
/// run time.
fn app_at(today: NaiveDate) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: today.and_hms_opt(12, 0, 0).expect("noon").and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
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

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn feedback(n: u8, status: JobStatus, closed_on: Option<NaiveDate>) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "BOSSNET"),
        title: format!("feedback {n}"),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: day(2026, 1, 1),
        due_on: None,
        closed_on,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated: false,
    }
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "GET /api/jobs?{query}");
    serde_json::from_slice(&resp.into_body().collect().await.expect("body").to_bytes())
        .expect("json")
}

fn titles(body: &serde_json::Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|j| j["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Seed the shape of the real board: a few live packets buried under a
/// long tail of finished ones.
async fn seed(jobs: &InMemoryJobs) {
    for j in [
        feedback(1, JobStatus::Open, None),
        feedback(2, JobStatus::Blocked, None),
        feedback(3, JobStatus::Closed, Some(day(2026, 8, 14))),
        // 4 and 7 straddle the cutoff for a 14-day window taken on
        // 16 Aug: 2 Aug is exactly the boundary, 1 Aug is one day past
        // it. Off-by-one in either direction moves one of them.
        feedback(4, JobStatus::Closed, Some(day(2026, 8, 2))),
        feedback(7, JobStatus::Closed, Some(day(2026, 8, 1))),
        feedback(5, JobStatus::Closed, Some(day(2026, 5, 1))),
        feedback(6, JobStatus::Cancelled, Some(day(2026, 3, 9))),
    ] {
        jobs.create_job(&j).await.expect("seed");
    }
}

#[tokio::test]
async fn closed_within_keeps_live_work_and_a_recent_tail() {
    let (app, jobs) = app_at(day(2026, 8, 16));
    seed(&jobs).await;

    let all = list(&app, "kind=user-feedback").await;
    assert_eq!(
        all["total"], 7,
        "without the window the board still sees everything"
    );

    // Today is 16 Aug, so a 14-day window cuts at 2 Aug inclusive:
    // "closed within 14 days" includes the day 14 days back.
    let windowed = list(&app, "kind=user-feedback&closed_within=14").await;
    let got = titles(&windowed);
    assert_eq!(
        windowed["total"], 4,
        "expected 2 live + the 2 closed on or after 2 Aug, got {got:?}"
    );
    assert!(
        got.contains(&"feedback 1".to_string()),
        "open work is never dropped"
    );
    assert!(
        got.contains(&"feedback 2".to_string()),
        "blocked work is live work"
    );
    assert!(got.contains(&"feedback 3".to_string()), "closed 2 days ago");
    assert!(
        got.contains(&"feedback 4".to_string()),
        "closed exactly 14 days ago is inside"
    );
    assert!(
        !got.contains(&"feedback 7".to_string()),
        "closed 15 days ago is outside"
    );
    assert!(
        !got.contains(&"feedback 6".to_string()),
        "an old cancellation is not recent"
    );
}

/// The same request, a month and a half later, returns less.
///
/// This is the assertion that the window is a function of the clock
/// rather than of the query string — and, because the clock here is
/// injected and frozen, that it reads the AUTHORITATIVE clock instead
/// of the process wall clock. BOSS runs simulated time; a board on a
/// simulated day must cut where that day says.
#[tokio::test]
async fn the_window_moves_with_the_authoritative_clock() {
    let (later, jobs) = app_at(day(2026, 9, 30));
    seed(&jobs).await;

    let windowed = list(&later, "kind=user-feedback&closed_within=14").await;
    let got = titles(&windowed);
    assert_eq!(
        windowed["total"], 2,
        "by 30 Sept nothing has closed recently — only the live pair remain, got {got:?}"
    );
    assert!(
        !got.contains(&"feedback 3".to_string()),
        "closed 47 days before 'today'"
    );
}

/// `closed_within=0` means "live work only, nothing finished".
///
/// Worth pinning because zero is the value most likely to be produced
/// by a caller doing arithmetic, and `Some(0)` must not be read as the
/// `None` that means "no window".
#[tokio::test]
async fn zero_days_is_a_real_window_not_an_absent_one() {
    let (app, jobs) = app_at(day(2026, 8, 16));
    seed(&jobs).await;

    let body = list(&app, "kind=user-feedback&closed_within=0").await;
    assert_eq!(
        body["total"],
        2,
        "only the open and blocked packets, got {:?}",
        titles(&body)
    );
}

/// A hostile or careless `closed_within` cannot make the endpoint fail.
///
/// A board is a read; there is no state to corrupt, and a 400 on a
/// query param would blank the page. Negative days clamp to 0 (live
/// only) and absurd ones clamp to ten years (effectively no window).
#[tokio::test]
async fn out_of_range_days_clamp_instead_of_erroring() {
    let (app, jobs) = app_at(day(2026, 8, 16));
    seed(&jobs).await;

    let negative = list(&app, "kind=user-feedback&closed_within=-30").await;
    assert_eq!(
        negative["total"], 2,
        "negative clamps to 0 — live work only"
    );

    let absurd = list(&app, "kind=user-feedback&closed_within=99999999").await;
    assert_eq!(
        absurd["total"], 7,
        "ten years reaches every packet seeded here"
    );
}
