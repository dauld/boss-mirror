//! `GET /api/jobs` refuses a query parameter it does not read — a 400
//! that names it — instead of answering the unfiltered list.
//!
//! WHY THIS FILE EXISTS. The department-retro collect procedure told
//! every weekly retro to read `…&closed_since=<week start>`. No such
//! parameter exists (the retention window is `closed_within=<days>`),
//! and the listing dropped it WITHOUT A WORD: measured three times on
//! 2026-09-23, `closed_since=2099-01-01` still returned a packet closed
//! on 09-17, and ship-a-change read 467 all-time where the week held
//! 239 (backlog 7f3e871a). Five W39 retros had already collected on
//! those inflated reads. It is the third time the same shape was paid
//! for — `?department=sales` answered every packet before `department`
//! existed (cc76f755), and the KB page's `?account_id=` has asked for a
//! subject's jobs and been handed everyone's since the param was
//! renamed `subject_id` — each one a well-formed, confident, wrong
//! answer. A filter the server cannot apply is a question it cannot
//! answer, and the only honest reply to that is an error.
//!
//! The mechanism is `#[serde(deny_unknown_fields)]` on the handler's
//! query struct, so the struct stays the ONE list of what the listing
//! accepts (CLAUDE.md §9a: no second list to fall out of step with it).
//! What is pinned here is the wire: the status, and that the refusal
//! names both the offending parameter and the one to use instead.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let today = NaiveDate::from_ymd_opt(2026, 9, 23).expect("valid date");
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: today.and_hms_opt(12, 0, 0).expect("noon").and_utc(),
        simulated: false,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    router(JobsApiState::minimal(jobs, bus, publisher, policy, clock))
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

async fn get(query: &str) -> (StatusCode, String) {
    let resp = app()
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
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The measured case: the retro's week bound. Refused, by name, and the
/// refusal lists the parameter that does exist — the reader who typed
/// `closed_since` learns `closed_within` from the error, not a doc.
#[tokio::test]
async fn closed_since_is_refused_by_name() {
    let (status, body) = get("kind=ship-a-change&closed_since=2099-01-01").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("closed_since"),
        "the 400 names the parameter it refused: {body}"
    );
    assert!(
        body.contains("closed_within"),
        "the 400 names what the listing does accept: {body}"
    );
}

/// The KB page's shape — a per-kind alias for the subject filter that
/// the listing never had. Refused rather than answered with every job.
#[tokio::test]
async fn a_per_kind_subject_alias_is_refused() {
    for alias in ["account_id", "asset_id"] {
        let (status, body) = get(&format!("{alias}=acct-1&limit=50")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{alias}: {body}");
        assert!(body.contains(alias), "the 400 names `{alias}`: {body}");
    }
}

/// The control: every parameter the listing DOES read still answers.
/// `department` is left out only because it needs a registry this app
/// does not carry (a 503 of its own, pinned in
/// jobs_list_narrows_on_department.rs); it is a field of the same
/// struct, so it is accepted by the same derive.
#[tokio::test]
async fn every_parameter_the_listing_reads_is_still_accepted() {
    let every = "limit=5&offset=0&kind=k&kind_prefix=k&status=open&owner_id=o\
                 &subject_id=s&waiting_on=w&closed_within=7&partition=real\
                 &simulated=false&metadata=%7B%22branch%22%3A%22b%22%7D&metadata_has=branch";
    let (status, body) = get(every).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get("").await;
    assert_eq!(status, StatusCode::OK, "no parameters at all: {body}");
}
