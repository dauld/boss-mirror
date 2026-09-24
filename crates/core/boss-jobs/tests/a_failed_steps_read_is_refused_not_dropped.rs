//! A packet whose steps cannot be read is NOT a packet with no steps.
//!
//! WHY THIS FILE EXISTS (backlog f6c97006). Car c11e9d3c found the
//! station queue answering a failed steps read with
//! `list_steps(..).unwrap_or_default()`: the packet read as stepless,
//! the step clause could not match, and it fell out of the queue under
//! a 200. Its builder found the same shape at seven more sites in the
//! jobs handlers. Each one here is driven through the real router with
//! the in-memory adapter's `fail_steps_read`, and each asserts the
//! answer that replaced the empty list:
//!
//! - `GET /api/jobs` — the list the ops-runner reads its `execute` step
//!   off (tests/a_listed_packet_carries_its_steps.rs): a row with
//!   `steps: []` is exactly the row it skips, so the list fails, 500,
//!   naming the packet.
//! - `GET /api/jobs/{id}` — the detail: 500 naming the packet, never a
//!   packet drawn empty.
//! - `GET /api/jobs/{id}/stream` — the first frame is an `error` naming
//!   the packet; a failed read on a later tick pushes `unavailable` and
//!   never a JobDetail with the steps gone.
//! - `POST .../claim?station=` — a station whose predicate reads steps
//!   answers 500 naming the packet, not 409 "packet is not at this
//!   station".
//!
//! The yard's two sites are pinned beside the yard's own tests
//! (tests/yard_status_http.rs), and the lint
//! `a-steps-read-failure-is-not-empty` refuses the shape under `http/`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::{InMemoryJobs, InMemoryStations, JobsRepository, StationKind, StationRegistry};
use boss_jobs::{StationSpec, registry::WorkflowStatus};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const READABLE: &str = "00000000-0000-0000-0000-0000000fa001";
const UNREADABLE: &str = "00000000-0000-0000-0000-0000000fa002";

fn id(s: &str) -> JobId {
    JobId::from_uuid(Uuid::parse_str(s).expect("uuid"))
}

fn ceo() -> String {
    serde_json::to_string(&User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .expect("a User always serialises")
}

fn request(job: &str) -> Job {
    Job {
        id: id(job),
        kind: "ops-request".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "BOSSNET"),
        title: format!("request {job}"),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 24).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "verb": "converge" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn execute(job: &str) -> Step {
    let mut s = Step::new(id(job), "task", "execute", 1);
    s.id = StepId::new();
    s.spec_slug = Some("execute".into());
    s.status = StepStatus::Ready;
    s
}

/// A station whose predicate reads step state — so the claim door has to
/// read the packet's steps to answer "is it at this station".
fn execute_station() -> StationSpec {
    let mut s = StationSpec::draft(
        "ops-execute",
        "Ops requests ready to execute",
        StationKind::Batch,
        StationPredicate {
            kind: Some("ops-request".into()),
            step: Some(StepMatch {
                slug: Some("execute".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    s.status = WorkflowStatus::Active;
    s
}

/// Two open ops-requests, each with its execute step; the second one's
/// steps read fails from here on. Returns the router and the first
/// request's execute step id (the claim needs one).
async fn seed() -> (Router, Arc<InMemoryJobs>, Vec<StepId>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut step_ids = Vec::new();
    for job in [READABLE, UNREADABLE] {
        jobs.create_job(&request(job)).await.expect("job");
        let step = execute(job);
        step_ids.push(step.id);
        jobs.add_step(&step).await.expect("step");
    }
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(execute_station()).expect("station");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        stations: Some(stations as Arc<dyn StationRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, step_ids)
}

async fn send(app: &Router, method: &str, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-boss-user", ceo())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_failed_steps_read_fails_the_list_naming_the_packet() {
    let (app, jobs, _) = seed().await;
    let (status, body) = send(&app, "GET", "/api/jobs?kind=ops-request").await;
    assert_eq!(status, StatusCode::OK, "control: both readable: {body}");

    jobs.fail_steps_read(&id(UNREADABLE));
    let (status, body) = send(&app, "GET", "/api/jobs?kind=ops-request").await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a row with steps: [] is the row the ops-runner skips: {body}"
    );
    assert!(body.contains(UNREADABLE), "names the packet: {body}");
}

#[tokio::test]
async fn a_failed_steps_read_fails_the_detail_naming_the_packet() {
    let (app, jobs, _) = seed().await;
    jobs.fail_steps_read(&id(UNREADABLE));

    let (status, body) = send(&app, "GET", &format!("/api/jobs/{READABLE}")).await;
    assert_eq!(status, StatusCode::OK, "control: the other packet: {body}");

    let (status, body) = send(&app, "GET", &format!("/api/jobs/{UNREADABLE}")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(body.contains(UNREADABLE), "names the packet: {body}");
}

#[tokio::test]
async fn a_failed_steps_read_fails_a_station_claim_rather_than_conflicting() {
    let (app, jobs, steps) = seed().await;
    let uri = |job: &str, step: &StepId| {
        format!("/api/jobs/{job}/steps/{step}/claim?station=ops-execute")
    };

    jobs.fail_steps_read(&id(UNREADABLE));
    let (status, body) = send(&app, "POST", &uri(UNREADABLE, &steps[1])).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "unread is not 'not at this station': {body}"
    );
    assert!(body.contains(UNREADABLE), "names the packet: {body}");

    // Control: the readable packet IS at the station, so its claim wins.
    let (status, body) = send(&app, "POST", &uri(READABLE, &steps[0])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The SSE body, frame by frame: the text of frames read until one
/// carries `until`, or the deadline passes.
async fn frames_until(body: &mut Body, until: &str, deadline: Duration) -> String {
    let mut text = String::new();
    let _ = tokio::time::timeout(deadline, async {
        while let Some(Ok(frame)) = body.frame().await {
            if let Some(bytes) = frame.data_ref() {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
            if text.contains(until) {
                break;
            }
        }
    })
    .await;
    text
}

async fn open_stream(app: &Router, job: &str) -> Body {
    app.clone()
        .oneshot(
            Request::get(format!("/api/jobs/{job}/stream"))
                .header("x-boss-user", ceo())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
        .into_body()
}

#[tokio::test]
async fn a_stream_opened_on_unreadable_steps_says_so_and_pushes_no_detail() {
    let (app, jobs, _) = seed().await;
    jobs.fail_steps_read(&id(UNREADABLE));
    let mut body = open_stream(&app, UNREADABLE).await;
    let text = frames_until(&mut body, "\n\n", Duration::from_secs(5)).await;
    assert!(text.contains("event: error"), "an error frame: {text:?}");
    assert!(text.contains(UNREADABLE), "names the packet: {text:?}");
    assert!(
        !text.contains("\"steps\""),
        "no JobDetail with its steps missing: {text:?}"
    );
}

#[tokio::test]
async fn a_stream_whose_steps_read_fails_later_pushes_unavailable_not_an_empty_packet() {
    let (app, jobs, _) = seed().await;
    let mut body = open_stream(&app, UNREADABLE).await;
    let first = frames_until(&mut body, "\n\n", Duration::from_secs(5)).await;
    assert!(
        first.contains("\"spec_slug\":\"execute\""),
        "control: the first frame carries the step: {first:?}"
    );

    // The stream is suspended at its first yield; the next poll runs the
    // loop's first tick, which reads the steps again — and fails.
    jobs.fail_steps_read(&id(UNREADABLE));
    let next = frames_until(&mut body, "\n\n", Duration::from_secs(6)).await;
    assert!(
        next.contains("event: unavailable"),
        "an unavailable frame: {next:?}"
    );
    assert!(next.contains(UNREADABLE), "names the packet: {next:?}");
    assert!(
        !next.contains("\"steps\""),
        "never a JobDetail with the steps gone: {next:?}"
    );
}
