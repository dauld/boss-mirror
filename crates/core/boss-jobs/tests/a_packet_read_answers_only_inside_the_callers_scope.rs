//! A packet read answers only inside the caller's read scope.
//!
//! Backlog 046832d3 (found by the triage of e5f7b51e, measured at
//! origin/main 9df57797): `GET /api/jobs` scoped its rows by policy, and
//! every job WRITE ran `policy.check` plus `scope_matches` on the row —
//! but six READS took no `CurrentUser` at all and answered any caller in
//! full: the detail (`/api/jobs/{id}`), its audit slice (`/events`), its
//! SSE stream (`/stream`), its steps (`/steps`), the assignments queue
//! (`/api/jobs/assignments`, including `all_assigned=true`) and the
//! refused-step-write table (`/api/jobs/step-write-refusals`). An
//! anonymous caller — no `x-boss-user`, so role `guest`, which the
//! policy grants only `workflow` Read — read every packet it could name,
//! and the ids were discoverable without auth.
//!
//! Each route now asks the list's question per row: policy Read on
//! `job`, then is THIS packet inside the scope it granted. A caller the
//! policy denies is REFUSED (403) before any id is looked up, so the
//! refusal says nothing about the id; a packet outside a granted scope
//! answers the SAME 404 as a packet that does not exist, so a scoped
//! caller cannot probe for ids either. Collections keep only the rows
//! inside scope.
//!
//! A HEADERLESS request is the `guest` the extractor defaults to, and is
//! refused here exactly as the list hands it nothing — whether a
//! headerless caller should be TRUSTED is backlog e84de48e's decision,
//! and `trust::is_trusted` is deliberately untouched.
//!
//! DEPARTMENT SCOPE, as `scope_matches` and the list both read it, is
//! judged on the CALLER's department, not the packet's: a caller granted
//! `department:brewhouse` who is in brewhouse reads every packet, one in
//! another department reads none. The tests below hold that, rather
//! than a per-packet department nobody's scope check reads today.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::refusals::{ErrorClass, StepWriteRefusal};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

/// Owned by the brewer — inside a `self` scope.
/// The three ids differ in their first eight characters, so each short
/// id resolves to exactly one of them.
const OWN: &str = "46a00001-0000-0000-0000-000000000000";
/// Owned by somebody else — outside a `self` scope.
const OTHERS: &str = "46a00002-0000-0000-0000-000000000000";
/// Never created: what "does not exist" answers, to compare against.
const ABSENT: &str = "46a000ff-0000-0000-0000-000000000000";

const BREWER: &str = "emp-brewer";
const HELPER: &str = "emp-helper";

fn id(s: &str) -> JobId {
    JobId::from_uuid(Uuid::parse_str(s).expect("uuid"))
}

fn user(id: &str, role: &str, tier: AccessTier, department: Option<&str>) -> User {
    User {
        id: id.into(),
        role: role.into(),
        access_tier: tier,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: department.map(str::to_string),
    }
}

/// Who asks. `None` is a request with no `x-boss-user` at all.
fn headerless() -> Option<User> {
    None
}
/// A caller who names themselves as a guest — the role the policy
/// grants only `workflow` Read, as the platform defaults do.
fn guest() -> Option<User> {
    Some(user("visitor", "guest", AccessTier::User, None))
}
/// The control: an operator the policy grants Read on every packet.
fn operator() -> Option<User> {
    Some(user(
        "emp-david",
        "platform-admin",
        AccessTier::Operator,
        Some("platform"),
    ))
}
/// Read on `job` at `self`: the packets it owns.
fn brewer() -> Option<User> {
    Some(user(BREWER, "brewer", AccessTier::User, Some("brewhouse")))
}
/// Read on `job` at `department:brewhouse`, from inside brewhouse.
fn lead_in_department() -> Option<User> {
    Some(user(
        "emp-lead",
        "brewhouse-lead",
        AccessTier::User,
        Some("brewhouse"),
    ))
}
/// The same grant, held by someone in another department.
fn lead_outside_department() -> Option<User> {
    Some(user(
        "emp-lead-2",
        "brewhouse-lead",
        AccessTier::User,
        Some("sales"),
    ))
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            // The platform default for guest, and nothing on job.
            .allow("guest", Action::Read, Resource::workflow(), Scope::All)
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow("brewer", Action::Read, Resource::job(), Scope::Self_)
            .allow(
                "brewhouse-lead",
                Action::Read,
                Resource::job(),
                Scope::Department("brewhouse".into()),
            )
            .build(),
    )
}

fn packet(job: &str, owner: &str) -> Job {
    Job {
        id: id(job),
        kind: "brew-day".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "FV-1"),
        title: format!("packet {job}"),
        owner_id: owner.into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn ready(job: &str, title: &str, order: i32, assignee: &str) -> Step {
    let mut s = Step::new(id(job), "task", title, order).with_assignee(assignee);
    s.status = StepStatus::Ready;
    s
}

fn refusal(job: Option<&str>, detail: &str) -> StepWriteRefusal {
    StepWriteRefusal {
        job_id: job.map(|j| Uuid::parse_str(j).expect("uuid")),
        step_id: None,
        actor_id: "emp-someone".into(),
        method: "PUT".into(),
        path: format!("/api/jobs/{}/steps/x", job.unwrap_or("not-an-id")),
        status_code: 409,
        error_class: ErrorClass::State,
        detail: detail.into(),
    }
}

/// Two packets: OWN (the brewer's) carries a step handed to the helper;
/// OTHERS carries one handed to the brewer and one to the helper. One
/// refused write on each, and one whose id never parsed.
async fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    jobs.create_job(&packet(OWN, BREWER)).await.expect("own");
    jobs.create_job(&packet(OTHERS, "emp-other"))
        .await
        .expect("others");
    for step in [
        ready(OWN, "own-for-helper", 0, HELPER),
        ready(OTHERS, "others-for-brewer", 0, BREWER),
        ready(OTHERS, "others-for-helper", 1, HELPER),
    ] {
        jobs.add_step(&step).await.expect("step");
    }
    for r in [
        refusal(Some(OWN), "refused-on-own"),
        refusal(Some(OTHERS), "refused-on-others"),
        refusal(None, "refused-on-no-packet"),
    ] {
        jobs.record_step_write_refusal(&r).await.expect("refusal");
    }

    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy(),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

fn get(uri: &str, who: &Option<User>) -> Request<Body> {
    let mut req = Request::get(uri);
    if let Some(u) = who {
        req = req.header(
            "x-boss-user",
            serde_json::to_string(u).expect("a User serialises"),
        );
    }
    req.body(Body::empty()).expect("request")
}

async fn send(app: &Router, uri: &str, who: &Option<User>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(get(uri, who)).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The single-packet reads, each a path on one packet.
fn packet_routes(job: &str) -> [String; 3] {
    [
        format!("/api/jobs/{job}"),
        format!("/api/jobs/{job}/events"),
        format!("/api/jobs/{job}/steps"),
    ]
}

/// Every route this item names, as a caller the policy denies reaches it.
fn every_route() -> Vec<String> {
    let mut routes: Vec<String> = packet_routes(OTHERS).into();
    routes.push(format!("/api/jobs/{OTHERS}/stream"));
    routes.push("/api/jobs/assignments?all_assigned=true".into());
    routes.push(format!("/api/jobs/assignments?assignee_id={BREWER}"));
    routes.push("/api/jobs/step-write-refusals".into());
    routes
}

// ---------------------------------------------------------------------
// Refused: a caller the policy grants nothing on `job`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_headerless_request_is_refused_on_every_route() {
    let app = app().await;
    for uri in every_route() {
        let (status, body) = send(&app, &uri, &headerless()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(
            !body.contains("packet 46a0"),
            "{uri} leaked a title: {body}"
        );
    }
}

#[tokio::test]
async fn a_guest_is_refused_on_every_route() {
    let app = app().await;
    for uri in every_route() {
        let (status, body) = send(&app, &uri, &guest()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(
            !body.contains("packet 46a0"),
            "{uri} leaked a title: {body}"
        );
    }
}

#[tokio::test]
async fn a_refusal_says_nothing_about_the_id() {
    // Decided before the id is looked up: a real packet and an absent
    // one are refused in the same words.
    let app = app().await;
    for (real, absent) in packet_routes(OTHERS).iter().zip(packet_routes(ABSENT)) {
        let a = send(&app, real, &guest()).await;
        let b = send(&app, &absent, &guest()).await;
        assert_eq!(a, b, "{real} vs {absent}");
    }
}

// ---------------------------------------------------------------------
// Control: an operator reads everything, as before.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_operator_reads_every_route() {
    let app = app().await;
    for job in [OWN, OTHERS] {
        for uri in packet_routes(job) {
            let (status, body) = send(&app, &uri, &operator()).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        }
    }
    let (_, detail) = send(&app, &format!("/api/jobs/{OTHERS}"), &operator()).await;
    assert!(detail.contains("others-for-helper"), "{detail}");

    let (status, all) = send(&app, "/api/jobs/assignments?all_assigned=true", &operator()).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    for title in ["own-for-helper", "others-for-brewer", "others-for-helper"] {
        assert!(all.contains(title), "{title} in {all}");
    }

    let (status, refused) = send(&app, "/api/jobs/step-write-refusals", &operator()).await;
    assert_eq!(status, StatusCode::OK, "{refused}");
    for detail in [
        "refused-on-own",
        "refused-on-others",
        "refused-on-no-packet",
    ] {
        assert!(refused.contains(detail), "{detail} in {refused}");
    }
}

// ---------------------------------------------------------------------
// Scoped: a `self` caller sees its own packets, and nothing else exists.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_scoped_caller_reads_its_own_packet() {
    let app = app().await;
    for uri in packet_routes(OWN) {
        let (status, body) = send(&app, &uri, &brewer()).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }
}

#[tokio::test]
async fn a_packet_outside_scope_answers_exactly_as_an_absent_one() {
    let app = app().await;
    for (outside, absent) in packet_routes(OTHERS).iter().zip(packet_routes(ABSENT)) {
        let (status, body) = send(&app, outside, &brewer()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{outside}: {body}");
        assert_eq!(
            (status, body),
            send(&app, &absent, &brewer()).await,
            "{outside} must not be told apart from {absent}"
        );
    }
    // The 8-character prefix every surface prints resolves to the same
    // answer: no way round the scope through the short id.
    let short = send(&app, &format!("/api/jobs/{}", &OTHERS[..8]), &brewer()).await;
    let absent = send(&app, &format!("/api/jobs/{}", &ABSENT[..8]), &brewer()).await;
    assert_eq!(short.0, StatusCode::NOT_FOUND, "{}", short.1);
    assert_eq!(short, absent);
}

#[tokio::test]
async fn the_assignments_queue_keeps_only_rows_in_scope() {
    let app = app().await;
    // Someone else's queue: only the row on the brewer's own packet.
    let (status, body) = send(
        &app,
        &format!("/api/jobs/assignments?assignee_id={HELPER}"),
        &brewer(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("own-for-helper"), "{body}");
    assert!(!body.contains("others-for-helper"), "{body}");

    // The whole assigned backlog: the in-scope row, plus the step handed
    // to the brewer itself — work assigned to a caller is theirs to see
    // even on a packet they do not own.
    let (status, body) = send(&app, "/api/jobs/assignments?all_assigned=true", &brewer()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 2, "{body}");
    assert!(body.contains("own-for-helper"), "{body}");
    assert!(body.contains("others-for-brewer"), "{body}");
    assert!(!body.contains("others-for-helper"), "{body}");
}

#[tokio::test]
async fn the_refusal_table_keeps_only_rows_in_scope() {
    let app = app().await;
    let (status, body) = send(&app, "/api/jobs/step-write-refusals", &brewer()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 1, "{body}");
    assert!(body.contains("refused-on-own"), "{body}");
    // A refusal naming no packet is inside no packet scope.
    assert!(!body.contains("refused-on-no-packet"), "{body}");
}

// ---------------------------------------------------------------------
// Department scope, read the way scope_matches reads it.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_department_grant_reads_from_inside_its_department_only() {
    let app = app().await;
    for uri in packet_routes(OTHERS) {
        let (status, body) = send(&app, &uri, &lead_in_department()).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let (status, body) = send(&app, &uri, &lead_outside_department()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }
    let (_, body) = send(
        &app,
        "/api/jobs/assignments?all_assigned=true",
        &lead_outside_department(),
    )
    .await;
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 0, "{body}");
}

// ---------------------------------------------------------------------
// The stream: the same detail, frame by frame.
// ---------------------------------------------------------------------

/// The SSE body's text until a frame carries `until`, or the deadline.
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

async fn first_frame(app: &Router, job: &str, who: &Option<User>) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(get(&format!("/api/jobs/{job}/stream"), who))
        .await
        .expect("response");
    let status = resp.status();
    let mut body = resp.into_body();
    (
        status,
        frames_until(&mut body, "\n\n", Duration::from_secs(5)).await,
    )
}

#[tokio::test]
async fn the_stream_pushes_a_packet_only_inside_scope() {
    let app = app().await;

    let (status, frame) = first_frame(&app, OTHERS, &operator()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(frame.contains("others-for-helper"), "control: {frame}");

    let (status, frame) = first_frame(&app, OWN, &brewer()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(frame.contains("own-for-helper"), "own packet: {frame}");

    // Outside scope reads as the absent packet reads: one `error` frame.
    let (_, outside) = first_frame(&app, OTHERS, &brewer()).await;
    let (_, absent) = first_frame(&app, ABSENT, &brewer()).await;
    assert!(outside.contains("event: error"), "{outside}");
    assert!(!outside.contains("others-for-helper"), "{outside}");
    assert_eq!(outside, absent);
}
