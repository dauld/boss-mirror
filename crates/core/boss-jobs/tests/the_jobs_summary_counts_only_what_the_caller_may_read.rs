//! `GET /api/jobs/summary` counts only the packets the caller may read.
//!
//! Backlog 19f08bd6 (found by the adversarial review of the superseded
//! jobs-api read-scope car, triaged at origin/main ca828eeb on
//! 2026-09-26): `jobs_summary` took no `CurrentUser` and asked no
//! policy question, so it answered per-kind counts across EVERY packet
//! to any caller, and `?status=closed` reached the closed totals.
//! Measured live the same morning: a headerless request to the jobs
//! port answered 200, total 21215 across 45 kinds — the same answer as
//! the signed-in operator — while on that same connection
//! `/api/jobs/{id}` answered 403 "reading packets is refused: no active
//! rule for role guest on job:read". Every sibling packet read had gone
//! through `job_read_scope` since 046832d3; this one was never in it.
//!
//! Now the summary asks `job_read_scope` like the rest: a caller the
//! policy DENIES is refused 403 (a headerless request is the `guest`
//! the extractor defaults to), an outage answers 503 (pinned in
//! `a_policy_outage_answers_503_not_a_refusal.rs`), and an allowed
//! caller's counts are taken under the SAME scope the list takes its
//! rows under — so a scoped caller's summary equals what `/api/jobs`
//! totals for it, kind by kind and status by status.
//!
//! `/api/jobs/live` shares the counting port but is the landing page's
//! public window by design (open counts only); it is pinned here as
//! unchanged, so scoping the summary cannot quietly narrow it too.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
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

const BREWER: &str = "emp-brewer";
const OTHER: &str = "emp-other";

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

/// A request with no `x-boss-user` at all.
fn headerless() -> Option<User> {
    None
}
/// A caller who names themselves a guest: `workflow` Read only.
fn guest() -> Option<User> {
    Some(user("visitor", "guest", AccessTier::User, None))
}
/// Read on every packet — the control, and the operator's own view.
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
/// Read at `department:brewhouse`, held from outside brewhouse — the
/// list hands this caller nothing, and so must the summary.
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

/// `(kind, owner, status)` for every packet the store holds: the
/// brewer owns three (two open across two kinds, one closed); somebody
/// else owns five (three open, two closed), so every count the brewer
/// may see differs from the whole store's.
const PACKETS: &[(&str, &str, JobStatus)] = &[
    ("brew-day", BREWER, JobStatus::Open),
    ("cellar-check", BREWER, JobStatus::Open),
    ("brew-day", BREWER, JobStatus::Closed),
    ("brew-day", OTHER, JobStatus::Open),
    ("brew-day", OTHER, JobStatus::Open),
    ("sale", OTHER, JobStatus::Open),
    ("sale", OTHER, JobStatus::Closed),
    ("cellar-check", OTHER, JobStatus::Closed),
];

fn packet(n: usize, kind: &str, owner: &str, status: JobStatus) -> Job {
    let id = Uuid::parse_str(&format!("19f08bd6-0000-0000-0000-{n:012}")).expect("uuid");
    Job {
        id: JobId::from_uuid(id),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("asset", "FV-1"),
        title: format!("packet {n}"),
        owner_id: owner.into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 20).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 21).expect("day")),
        metadata: Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

async fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    for (n, (kind, owner, status)) in PACKETS.iter().enumerate() {
        jobs.create_job(&packet(n, kind, owner, *status))
            .await
            .expect("packet");
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

async fn send(app: &Router, uri: &str, who: &Option<User>) -> (StatusCode, String) {
    let mut req = Request::get(uri);
    if let Some(u) = who {
        req = req.header(
            "x-boss-user",
            serde_json::to_string(u).expect("a User serialises"),
        );
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn json(app: &Router, uri: &str, who: &Option<User>) -> Value {
    let (status, body) = send(app, uri, who).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    serde_json::from_str(&body).expect("json body")
}

/// The whole store's count of `kind` at `status` (None = any status).
fn store_count(kind: Option<&str>, status: Option<JobStatus>, owner: Option<&str>) -> i64 {
    PACKETS
        .iter()
        .filter(|(k, o, s)| {
            kind.is_none_or(|want| *k == want)
                && status.is_none_or(|want| *s == want)
                && owner.is_none_or(|want| *o == want)
        })
        .count() as i64
}

const STATUSES: [Option<(&str, JobStatus)>; 3] = [
    None,
    Some(("open", JobStatus::Open)),
    Some(("closed", JobStatus::Closed)),
];

fn summary_uri(status: Option<&str>) -> String {
    match status {
        Some(s) => format!("/api/jobs/summary?status={s}"),
        None => "/api/jobs/summary".to_string(),
    }
}

// ---------------------------------------------------------------------
// Refused: a caller the policy grants nothing on `job`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_headerless_summary_is_refused() {
    let app = app().await;
    for status in STATUSES {
        let uri = summary_uri(status.map(|(s, _)| s));
        let (code, body) = send(&app, &uri, &headerless()).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(
            body.contains("reading packets is refused"),
            "{uri}: the refusal names itself as the packet-read one: {body}"
        );
        assert!(
            !body.contains("\"counts\"") && !body.contains("brew-day"),
            "{uri} leaked counts: {body}"
        );
    }
}

#[tokio::test]
async fn a_guest_summary_is_refused() {
    let app = app().await;
    for status in STATUSES {
        let uri = summary_uri(status.map(|(s, _)| s));
        let (code, body) = send(&app, &uri, &guest()).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(!body.contains("brew-day"), "{uri} leaked counts: {body}");
    }
}

// ---------------------------------------------------------------------
// Answered: the counts inside the scope the caller holds.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_all_scope_caller_gets_the_whole_stores_counts() {
    let app = app().await;
    for status in STATUSES {
        let uri = summary_uri(status.map(|(s, _)| s));
        let want_status = status.map(|(_, s)| s);
        let body = json(&app, &uri, &operator()).await;
        assert_eq!(
            body["total"].as_i64(),
            Some(store_count(None, want_status, None)),
            "{uri}: {body}"
        );
        for kind in ["brew-day", "cellar-check", "sale"] {
            let want = store_count(Some(kind), want_status, None);
            let got = body["counts"][kind].as_i64().unwrap_or(0);
            assert_eq!(got, want, "{uri} {kind}: {body}");
        }
    }
}

#[tokio::test]
async fn a_self_scoped_callers_summary_equals_what_the_list_totals_for_it() {
    let app = app().await;
    for status in STATUSES {
        let uri = summary_uri(status.map(|(s, _)| s));
        let want_status = status.map(|(_, s)| s);
        let summary = json(&app, &uri, &brewer()).await;

        // Against the fixture: only the brewer's own packets.
        assert_eq!(
            summary["total"].as_i64(),
            Some(store_count(None, want_status, Some(BREWER))),
            "{uri}: {summary}"
        );

        // Against the list, the same caller, the same question: the
        // summary is a count of what /api/jobs hands this caller.
        let list_uri = match status {
            Some((s, _)) => format!("/api/jobs?status={s}"),
            None => "/api/jobs".to_string(),
        };
        let list = json(&app, &list_uri, &brewer()).await;
        assert_eq!(
            summary["total"], list["total"],
            "{uri} vs {list_uri}: {summary} / {list}"
        );
        for kind in ["brew-day", "cellar-check", "sale"] {
            let list_uri = match status {
                Some((s, _)) => format!("/api/jobs?kind={kind}&status={s}"),
                None => format!("/api/jobs?kind={kind}"),
            };
            let list = json(&app, &list_uri, &brewer()).await;
            let got = summary["counts"][kind].as_i64().unwrap_or(0);
            assert_eq!(
                Some(got),
                list["total"].as_i64(),
                "{uri} {kind} vs {list_uri}: {summary} / {list}"
            );
        }
        assert!(
            summary["counts"].get("sale").is_none(),
            "{uri}: a kind the caller owns none of is absent, not zero-filled: {summary}"
        );
    }
}

#[tokio::test]
async fn a_scope_that_admits_nothing_counts_nothing() {
    // Department scope held from another department: the list answers
    // an empty 200, and the summary answers the same zero, not the
    // store's counts.
    let app = app().await;
    for status in STATUSES {
        let uri = summary_uri(status.map(|(s, _)| s));
        let body = json(&app, &uri, &lead_outside_department()).await;
        assert_eq!(body["total"].as_i64(), Some(0), "{uri}: {body}");
        assert_eq!(
            body["counts"].as_object().map(|m| m.len()),
            Some(0),
            "{uri}: {body}"
        );
    }
}

// ---------------------------------------------------------------------
// Unchanged: the landing's public window.
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_live_window_still_counts_every_open_packet_for_anyone() {
    let app = app().await;
    let body = json(&app, "/api/jobs/live", &headerless()).await;
    assert_eq!(
        body["open_total"].as_i64(),
        Some(store_count(None, Some(JobStatus::Open), None)),
        "{body}"
    );
    assert_eq!(body["counts"]["brew-day"].as_i64(), Some(3), "{body}");
}
