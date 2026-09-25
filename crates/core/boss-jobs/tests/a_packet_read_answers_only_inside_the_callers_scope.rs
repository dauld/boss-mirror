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
//! THE CALLER'S OWN WORK IS THE EXCEPTION, AND ONE RULE DECIDES IT
//! (review of 0c0405ac, 2026-09-25). A step handed to the caller, or
//! claimable by the role the caller holds, is theirs to see and to
//! open even on a packet they do not own — the queue shows the row and
//! the packet read admits the packet, so a row can never name a packet
//! that then answers 404. The first cut kept only assigned rows, which
//! emptied My Day's claimable pool for every `self`- or
//! territory-scoped role, and left the rows it did keep unopenable.
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
/// The ids differ in their first eight characters (bar the two TWINs),
/// so each short id resolves to exactly one of them.
const OWN: &str = "46a00001-0000-0000-0000-000000000000";
/// Owned by somebody else, and carrying nothing for the brewer —
/// outside a `self` scope in every sense.
const OTHERS: &str = "46a00002-0000-0000-0000-000000000000";
/// Owned by somebody else, carrying a step handed to the brewer.
const HANDED: &str = "46a00003-0000-0000-0000-000000000000";
/// Owned by somebody else, carrying an unassigned step whose
/// `authority_role` is the brewer's role — the claimable pool.
const CLAIMABLE: &str = "46a00004-0000-0000-0000-000000000000";
/// Two packets sharing an 8-character prefix, neither the brewer's.
const TWIN_A: &str = "46a00005-0000-0000-0000-00000000000a";
const TWIN_B: &str = "46a00005-0000-0000-0000-00000000000b";
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
/// Another `self`-scoped role, whose claimable pool is not the brewer's.
fn cellar_tech() -> Option<User> {
    Some(user(
        "emp-cellar",
        "cellar-tech",
        AccessTier::User,
        Some("cellar"),
    ))
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
            .allow("cellar-tech", Action::Read, Resource::job(), Scope::Self_)
            .allow(
                "brewhouse-lead",
                Action::Read,
                Resource::job(),
                Scope::Department("brewhouse".into()),
            )
            .build(),
    )
}

/// `day` of September 2026 the packet was opened: the queue is ordered
/// oldest first, so the day decides which row a `limit` reaches first.
fn packet(job: &str, owner: &str, day: u32) -> Job {
    Job {
        id: id(job),
        kind: "brew-day".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "FV-1"),
        title: format!("packet {job}"),
        owner_id: owner.into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, day).expect("day"),
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

/// A ready step nobody holds, claimable by whoever holds `role`.
fn claimable(job: &str, title: &str, order: i32, role: &str) -> Step {
    let mut s = Step::new(id(job), "task", title, order);
    s.status = StepStatus::Ready;
    s.metadata = serde_json::json!({ "authority_role": role });
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

/// OWN (the brewer's) carries a step handed to the helper; OTHERS one
/// handed to the helper and one claimable by cellar-tech; HANDED one
/// handed to the brewer; CLAIMABLE one claimable by brewer. OTHERS is
/// the oldest, so a queue's first row is on a packet the brewer cannot
/// read. One refused write on OWN and OTHERS, and one whose id never
/// parsed.
async fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    for (job, owner, day) in [
        (OWN, BREWER, 25),
        (OTHERS, "emp-other", 24),
        (HANDED, "emp-other", 25),
        (CLAIMABLE, "emp-other", 25),
        (TWIN_A, "emp-other", 25),
        (TWIN_B, "emp-other", 25),
    ] {
        jobs.create_job(&packet(job, owner, day))
            .await
            .expect("packet");
    }
    for step in [
        ready(OWN, "own-for-helper", 0, HELPER),
        ready(OTHERS, "others-for-helper", 0, HELPER),
        claimable(OTHERS, "others-for-cellar", 1, "cellar-tech"),
        ready(HANDED, "handed-to-brewer", 0, BREWER),
        claimable(CLAIMABLE, "claimable-by-brewer", 0, "brewer"),
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
    routes.push(format!("/api/jobs/{OTHERS}/convert"));
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
    for title in ["own-for-helper", "handed-to-brewer", "others-for-helper"] {
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
    assert!(body.contains("handed-to-brewer"), "{body}");
    assert!(!body.contains("others-for-helper"), "{body}");
}

#[tokio::test]
async fn a_limit_counts_the_rows_the_caller_can_see() {
    // The oldest helper row is on OTHERS, which the brewer cannot read.
    // A limit taken BEFORE the scope cut spent itself on that row and
    // answered nothing; the limit is on what the caller is handed.
    let app = app().await;
    let (status, body) = send(
        &app,
        &format!("/api/jobs/assignments?assignee_id={HELPER}&limit=1"),
        &brewer(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 1, "{body}");
    assert!(body.contains("own-for-helper"), "{body}");
}

// ---------------------------------------------------------------------
// The caller's own work: handed to them, or claimable by their role.
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_claimable_pool_survives_on_packets_the_caller_does_not_own() {
    // My Day's own call: `assignee_id=<me>&roles=<my role>`
    // (apps/web/src/me/assignments.ts). A `self`-scoped brewer owns
    // neither packet, and both rows are its work.
    let app = app().await;
    let (status, body) = send(
        &app,
        &format!("/api/jobs/assignments?assignee_id={BREWER}&roles=brewer"),
        &brewer(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 2, "{body}");
    assert!(body.contains("claimable-by-brewer"), "{body}");
    assert!(body.contains("handed-to-brewer"), "{body}");
    assert!(!body.contains("others-for"), "{body}");

    // A role is the one the caller HOLDS, not one it names: asking for
    // cellar-tech's pool as a brewer finds nothing.
    let (status, body) = send(&app, "/api/jobs/assignments?roles=cellar-tech", &brewer()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 0, "{body}");

    // Control: the cellar-tech asking for its own pool sees it.
    let (_, body) = send(
        &app,
        "/api/jobs/assignments?assignee_id=emp-cellar&roles=cellar-tech",
        &cellar_tech(),
    )
    .await;
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["total"], 1, "{body}");
    assert!(body.contains("others-for-cellar"), "{body}");
}

#[tokio::test]
async fn a_packet_carrying_the_callers_work_opens() {
    let app = app().await;
    for job in [HANDED, CLAIMABLE] {
        for uri in packet_routes(job) {
            let (status, body) = send(&app, &uri, &brewer()).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        }
    }
    let (_, detail) = send(&app, &format!("/api/jobs/{}", &HANDED[..8]), &brewer()).await;
    assert!(detail.contains("handed-to-brewer"), "{detail}");
    let (status, frame) = first_frame(&app, CLAIMABLE, &brewer()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(frame.contains("claimable-by-brewer"), "{frame}");

    // Claimable by a role the caller does not hold is not its work.
    for uri in packet_routes(CLAIMABLE) {
        let (status, body) = send(&app, &uri, &cellar_tech()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }
}

#[tokio::test]
async fn every_row_the_queue_hands_the_caller_opens() {
    // One rule for both surfaces: a row My Day shows names a packet the
    // caller can open (web opens a step through the packet read).
    let app = app().await;
    for query in [
        "/api/jobs/assignments?all_assigned=true".to_string(),
        format!("/api/jobs/assignments?assignee_id={BREWER}&roles=brewer"),
        format!("/api/jobs/assignments?assignee_id={HELPER}"),
    ] {
        let (status, body) = send(&app, &query, &brewer()).await;
        assert_eq!(status, StatusCode::OK, "{query}: {body}");
        let v: Value = serde_json::from_str(&body).expect("json");
        let rows = v["data"].as_array().expect("rows");
        assert!(!rows.is_empty(), "{query}: {body}");
        for row in rows {
            let job = row["job_id"].as_str().expect("job_id");
            let (status, detail) = send(&app, &format!("/api/jobs/{job}"), &brewer()).await;
            assert_eq!(status, StatusCode::OK, "{query} row on {job}: {detail}");
        }
    }
}

// ---------------------------------------------------------------------
// The conversion preview is a packet read, and answers as one.
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_conversion_preview_asks_the_read_scope_before_the_id() {
    let app = app().await;
    let convert = |job: &str| format!("/api/jobs/{job}/convert");

    // Refused callers: the same words for a real packet and an absent one.
    for who in [headerless(), guest()] {
        let real = send(&app, &convert(OTHERS), &who).await;
        assert_eq!(real.0, StatusCode::FORBIDDEN, "{}", real.1);
        assert_eq!(real, send(&app, &convert(ABSENT), &who).await);
    }

    // A scoped caller: outside scope reads as absent, by full id and by
    // the short one.
    let outside = send(&app, &convert(OTHERS), &brewer()).await;
    assert_eq!(outside.0, StatusCode::NOT_FOUND, "{}", outside.1);
    assert_eq!(outside, send(&app, &convert(ABSENT), &brewer()).await);
    assert_eq!(
        send(&app, &convert(&OTHERS[..8]), &brewer()).await,
        send(&app, &convert(&ABSENT[..8]), &brewer()).await,
    );

    // Inside scope the preview goes on to be judged — this fixture has
    // no workflow registry, so it stops there, past the scope.
    for (job, who) in [(OWN, brewer()), (OTHERS, operator())] {
        let (status, body) = send(&app, &convert(job), &who).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{job}: {body}");
    }
}

#[tokio::test]
async fn an_ambiguous_prefix_outside_scope_reads_as_absent() {
    let app = app().await;
    let prefix = &TWIN_A[..8];
    assert_eq!(prefix, &TWIN_B[..8], "the fixture's twins share a prefix");
    // Control: a caller who reads every packet is told it is ambiguous.
    let (status, body) = send(&app, &format!("/api/jobs/{prefix}"), &operator()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // A scoped caller cannot learn that two packets it cannot read share
    // a prefix: it is told what an absent prefix is told.
    for route in ["", "/events", "/convert"] {
        let twin = send(&app, &format!("/api/jobs/{prefix}{route}"), &brewer()).await;
        let absent = send(
            &app,
            &format!("/api/jobs/{}{route}", &ABSENT[..8]),
            &brewer(),
        )
        .await;
        assert_eq!(twin.0, StatusCode::NOT_FOUND, "{route}: {}", twin.1);
        assert_eq!(twin, absent, "{route}");
    }
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
