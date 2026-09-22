//! `opened_at` is a schema field, server-stamped at admission — not a
//! filer convention (backlog 6c2eba00, design f2cdff23).
//!
//! `opened_on` is a DATE, so the finest honest answer it supports is a
//! whole day, and every surface that asks "how long has this been
//! waiting" reads an instant instead: the ops-runner's `oldest_wait_s`
//! gauge, dock wait and gate duration on the region map, the overdue
//! alarms, the silence sweep. Until this car that instant was
//! `metadata.opened_at`, written by whoever filed the packet — present
//! on everything the doors file, absent on anything filed by a caller
//! that does not know the convention. `date -u -d ''` answers midnight
//! rather than erroring, so a missing stamp reads as decades old
//! rather than as unknown.
//!
//! What is only visible HERE, between the admission edge and the port:
//!
//!   1. A packet filed with no `opened_at` comes back carrying one,
//!      from the same clock that dates `opened_on`.
//!   2. The field is SERVER-owned: a body that supplies its own value
//!      is overwritten, the way `workflow_version` is.
//!   3. A later PUT cannot move it — the instant a packet arrived is
//!      decided once, like its partition.
//!   4. The `metadata.opened_at` stamp is still written, so the
//!      readers already on it keep working unchanged.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{JobId, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// The clock the API reads — the one that dates `opened_on` too, so
/// the two answers must agree about the day.
fn admitted_at() -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(2026, 9, 15)
        .expect("valid date")
        .and_hms_micro_opt(12, 34, 56, 789_012)
        .expect("valid time")
        .and_utc()
}

fn app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: admitted_at(),
        simulated: false,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    let state = JobsApiState::minimal(jobs.clone(), bus, publisher, policy, clock);
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

/// A filer's body: every required envelope key, and no `opened_on` —
/// the shape `boss job file` and the dispatcher's rules send.
fn filed_body() -> serde_json::Value {
    serde_json::json!({
        "kind": "backlog-item",
        "subject": {"id": "bosspipeline", "subject_kind": "custom"},
        "title": "a packet filed through a door",
        "owner_id": "emp-david",
        "status": "open",
        "priority": "standard",
        "tags": [],
        "metadata": {}
    })
}

async fn post_job(app: &Router, body: serde_json::Value) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn create(app: &Router, body: serde_json::Value) -> JobId {
    let (status, text) = post_job(app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let created: serde_json::Value = serde_json::from_str(&text).expect("json");
    JobId::from_uuid(Uuid::parse_str(created["id"].as_str().expect("id")).expect("uuid"))
}

#[tokio::test]
async fn a_packet_filed_with_no_stamp_is_admitted_with_one() {
    let (app, jobs) = app();
    let id = create(&app, filed_body()).await;

    let stored = jobs.get_job(&id).await.expect("read").expect("the packet");
    assert_eq!(
        stored.opened_at,
        Some(admitted_at()),
        "the admission instant is a field, not something the filer had to remember"
    );
    assert_eq!(
        stored.opened_on,
        admitted_at().date_naive(),
        "the date and the instant come from the same clock"
    );

    // The readers already on the metadata convention (borders.rs's
    // dock-to-gates rail, the terminal report's cycle time) must keep
    // working unchanged across the promotion.
    assert_eq!(
        stored.metadata.get("opened_at").and_then(|v| v.as_str()),
        Some(admitted_at().to_rfc3339().as_str()),
        "the metadata stamp is still written for the readers already on it"
    );
}

#[tokio::test]
async fn a_body_that_stamps_itself_does_not_get_to_say_when_it_arrived() {
    let (app, jobs) = app();

    // A packet claiming it arrived a year ago. Nothing about admission
    // is a client's to assert — the same ownership `workflow_version`
    // and the experiment arm take.
    let mut body = filed_body();
    body["opened_at"] = serde_json::json!("2025-01-01T00:00:00Z");
    let id = create(&app, body).await;

    let stored = jobs.get_job(&id).await.expect("read").expect("the packet");
    assert_eq!(
        stored.opened_at,
        Some(admitted_at()),
        "a forged arrival instant is overwritten by the server's"
    );
}

#[tokio::test]
async fn a_later_write_cannot_move_when_the_packet_arrived() {
    let (app, jobs) = app();
    let id = create(&app, filed_body()).await;

    let mut job = jobs.get_job(&id).await.expect("read").expect("the packet");
    job.title = "retitled".into();
    job.opened_at = Some(admitted_at() - chrono::Duration::days(400));
    jobs.update_job(&job).await.expect("update");

    let stored = jobs.get_job(&id).await.expect("read").expect("the packet");
    assert_eq!(stored.title, "retitled", "the update did land");
    assert_eq!(
        stored.opened_at,
        Some(admitted_at()),
        "the storage enforces the immutability rather than trusting every caller to"
    );
}

/// The whole point of promoting it: a packet filed by a caller that
/// never heard of the convention still has an arrival instant, and the
/// duration surfaces read a field rather than a habit. Subject and
/// priority are pinned here only so the body is a legal envelope.
#[tokio::test]
async fn a_caller_that_never_heard_of_the_convention_still_produces_a_stamp() {
    let (app, jobs) = app();
    let mut body = filed_body();
    body["metadata"] = serde_json::json!({"area": "boss-core"});
    body["priority"] = serde_json::json!(Priority::Urgent);
    body["subject"] = serde_json::json!(Subject::new("custom", "bosspipeline"));
    let id = create(&app, body).await;

    let stored = jobs.get_job(&id).await.expect("read").expect("the packet");
    assert!(
        stored.opened_at.is_some(),
        "nothing a filer does or omits can leave the arrival instant absent"
    );
}
