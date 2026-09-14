//! `GET /api/jobs?metadata_has=<key>` and `?metadata=<flat object>` —
//! the two metadata filters at the layer that consumes them.
//!
//! WHY THIS FILE EXISTS. Until 2026-09-14 the port could narrow on
//! metadata (`JobFilter.metadata_contains`, used by station predicates)
//! but the HTTP list handler never set it, so every reader that wanted
//! "the car whose branch is X" or "alerts carrying estate_finding"
//! fetched a page and filtered afterwards — exact only while the page
//! was bigger than the world. The recovered-alarm builder measured the
//! closed listing at 356 rows within 14 days against a 200-row page
//! (backlog 4d9aa761). A limit is not a filter.
//!
//! The adapters are covered in `postgres_filter.rs` and the
//! `in_memory` unit tests. What is only visible HERE is what sits
//! between the query string and the filter:
//!
//!   1. The params are named `metadata_has` and `metadata` and reach
//!      `filter.metadata_has` / `filter.metadata_contains`. A filter
//!      that falls through returns everything, which looks like
//!      working software.
//!
//!   2. The refusals. `metadata` accepts the shape the port handles —
//!      a flat object of string values — and a 400 names that rule
//!      rather than silently matching nothing. `metadata_has` is a
//!      plain identifier; a path or a stray character is refused for
//!      the same reason.

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

fn app() -> (Router, Arc<InMemoryJobs>) {
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
        now: day(2026, 9, 14)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
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

fn packet(n: u8, kind: &str, title: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("asset", "BOSSNET"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 1),
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        simulated: false,
    }
}

async fn get(app: &Router, query: &str) -> (StatusCode, String) {
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
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
    let (status, body) = get(app, query).await;
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

fn titles(body: &serde_json::Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|j| j["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The shape the three probes read: a kind with more packets than
/// carry the key they are after.
async fn seed(jobs: &InMemoryJobs) {
    for j in [
        packet(
            1,
            "estate-alert",
            "disk floor",
            serde_json::json!({ "estate_finding": "disk-floor", "node": "forge" }),
        ),
        packet(
            2,
            "estate-alert",
            "node reset",
            serde_json::json!({ "estate_finding": "reset-loop", "node": "w-1" }),
        ),
        packet(
            3,
            "estate-alert",
            "hand-filed",
            serde_json::json!({ "node": "cp-2" }),
        ),
        packet(4, "estate-alert", "bare", serde_json::Value::Null),
        packet(
            5,
            "ship-a-change",
            "the car",
            serde_json::json!({ "branch": "feat/x", "outcome": "arrived" }),
        ),
        packet(
            6,
            "ship-a-change",
            "another car",
            serde_json::json!({ "branch": "feat/y", "outcome": "arrived" }),
        ),
    ] {
        jobs.create_job(&j).await.expect("seed");
    }
}

#[tokio::test]
async fn metadata_has_returns_only_packets_carrying_the_key() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let all = list(&app, "kind=estate-alert").await;
    assert_eq!(all["total"], 4, "without the filter the kind is four");

    let body = list(&app, "kind=estate-alert&metadata_has=estate_finding").await;
    let got = titles(&body);
    assert_eq!(
        body["total"], 2,
        "two alerts carry estate_finding, got {got:?}"
    );
    assert_eq!(got.len(), 2);
    assert!(got.contains(&"disk floor".to_string()));
    assert!(got.contains(&"node reset".to_string()));
    assert!(
        !got.contains(&"hand-filed".to_string()),
        "a packet with other keys does not carry this one"
    );
    assert!(
        !got.contains(&"bare".to_string()),
        "null metadata carries nothing"
    );
}

#[tokio::test]
async fn metadata_containment_returns_the_one_car() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let doc = "%7B%22branch%22%3A%22feat%2Fx%22%7D"; // {"branch":"feat/x"}
    let body = list(&app, &format!("kind=ship-a-change&metadata={doc}")).await;
    assert_eq!(body["total"], 1, "got {:?}", titles(&body));
    assert_eq!(titles(&body), vec!["the car".to_string()]);

    // Two keys intersect on one packet.
    let doc = "%7B%22branch%22%3A%22feat%2Fy%22%2C%22outcome%22%3A%22arrived%22%7D";
    let body = list(&app, &format!("kind=ship-a-change&metadata={doc}")).await;
    assert_eq!(titles(&body), vec!["another car".to_string()]);

    // A document nothing contains is an empty answer, not everything.
    let doc = "%7B%22branch%22%3A%22feat%2Fz%22%7D";
    let body = list(&app, &format!("kind=ship-a-change&metadata={doc}")).await;
    assert_eq!(body["total"], 0);
}

/// The port handles flat string-valued documents — that is the whole of
/// what `metadata_equals` expresses and what the in-memory adapter
/// mirrors. Anything else is refused with the rule named, because a
/// nested document that silently matched nothing would read as "no such
/// packet" — the wrong-target failure the doors section warns about.
#[tokio::test]
async fn a_metadata_document_that_is_not_flat_strings_is_refused_naming_the_rule() {
    let (app, jobs) = app();
    seed(&jobs).await;

    // A nested object.
    let (status, body) = get(&app, "metadata=%7B%22a%22%3A%7B%22b%22%3A%22c%22%7D%7D").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("flat") && body.contains("string"),
        "the 400 must name the flat-string rule, got: {body}"
    );

    // A number value.
    let (status, body) = get(&app, "metadata=%7B%22n%22%3A1%7D").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Not an object at all.
    let (status, body) = get(&app, "metadata=%5B%22a%22%5D").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Not JSON.
    let (status, body) = get(&app, "metadata=branch%3Dfeat%2Fx").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // An empty object narrows nothing and is not an error.
    let body = list(&app, "kind=ship-a-change&metadata=%7B%7D").await;
    assert_eq!(body["total"], 2);
}

/// `metadata_has` is a top-level key — letters, digits, underscore. The
/// SQL behind it is `metadata ? $n`, which only reads top-level keys,
/// so a dotted path would silently match nothing; refuse it instead.
#[tokio::test]
async fn a_metadata_has_key_that_is_not_a_plain_identifier_is_refused() {
    let (app, jobs) = app();
    seed(&jobs).await;

    for bad in [
        "metadata_has=a.b",
        "metadata_has=",
        "metadata_has=1abc",
        "metadata_has=has%20space",
        "metadata_has=key-with-dash",
    ] {
        let (status, body) = get(&app, bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(
            body.contains("metadata_has"),
            "the 400 must name the param, got: {body}"
        );
    }

    // Underscore-led and digit-bearing identifiers are fine.
    let body = list(&app, "metadata_has=_x9").await;
    assert_eq!(body["total"], 0);
}

/// Both filters at once intersect with each other and with `kind`.
#[tokio::test]
async fn the_filters_compose_with_kind_and_each_other() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let body = list(
        &app,
        "kind=estate-alert&metadata_has=estate_finding&metadata=%7B%22node%22%3A%22w-1%22%7D",
    )
    .await;
    assert_eq!(titles(&body), vec!["node reset".to_string()]);
    assert_eq!(body["total"], 1);
}
