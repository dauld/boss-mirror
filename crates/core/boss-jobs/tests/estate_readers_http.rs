//! `GET /api/estate/observations` + `/api/estate/comparisons` — the
//! read half of the estate loop (d471a8ce).
//!
//! The estate loop went live on 2026-08-30 with its first observation
//! and comparison recorded — and both were readable only through an
//! in-pod port-forward, which left two satisfied proven arbiters with
//! no surface a recorded probe could re-run against. These readers are
//! that surface, and the IT page's data source.
//!
//! Properties pinned:
//! - a GUEST reads both (same posture as /api/estate/nodes — no auth
//!   gate, these serve the public IT surface);
//! - an empty log answers `{"data": []}`, not an error;
//! - each reader serves ONLY its own kind — an observation never
//!   appears among comparisons and vice versa;
//! - `?limit=` is respected and rows come newest-first, verbatim as
//!   recorded;
//! - `?scope=` selects one series, and selects it BEFORE the limit —
//!   the property that makes a slow-cadence scope readable at all;
//! - `?since=` / `?until=` bound a half-open window in the same WHERE
//!   clause, `until=` pages back past the cap, and `total` counts the
//!   window — the properties a post-mortem needs (bf362f25).

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::port::JobsRepository;
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

// SERIALISE THE REAL TYPE, never a copy of its wire shape (7c3649e2).
fn operator_header() -> String {
    serde_json::to_string(&User {
        id: "automation:estate-observer".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("platform".to_string()),
    })
    .expect("a User always serialises")
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

/// GET as a guest — deliberately NO x-boss-user header.
async fn get_as_guest(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn post_observation(app: &axum::Router, marker: &str) {
    post_observation_in(app, "kubernetes-nodes", marker).await
}

async fn post_observation_in(app: &axum::Router, scope: &str, marker: &str) {
    let body = serde_json::json!({
        "observed_at": "2026-08-30T23:32:14Z",
        "observer": "boss-estate-observe",
        "scope": scope,
        "marker": marker,
        "nodes": [{"id": "w-1", "cpu": 32}],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/estate/observation")
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "observation records");
}

async fn post_comparison(app: &axum::Router, marker: &str) {
    let body = serde_json::json!({
        "scope": "kubernetes-nodes",
        "marker": marker,
        "findings": {"drift": [], "observed_not_declared": []},
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/estate/comparison")
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "comparison records");
}

fn markers(body: &serde_json::Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .map(|row| row["payload"]["marker"].as_str().unwrap_or("?").to_string())
        .collect()
}

#[tokio::test]
async fn an_empty_log_answers_empty_data_to_a_guest() {
    let (app, _) = app();
    for uri in ["/api/estate/observations", "/api/estate/comparisons"] {
        let (status, body) = get_as_guest(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} is guest-readable");
        assert_eq!(
            body["data"],
            serde_json::json!([]),
            "{uri} answers empty, not an error"
        );
    }
}

#[tokio::test]
async fn each_reader_serves_only_its_own_kind() {
    let (app, _) = app();
    post_observation(&app, "obs-1").await;
    post_comparison(&app, "cmp-1").await;

    let (_, obs) = get_as_guest(&app, "/api/estate/observations").await;
    let (_, cmp) = get_as_guest(&app, "/api/estate/comparisons").await;
    assert_eq!(markers(&obs), vec!["obs-1"], "observations reader");
    assert_eq!(markers(&cmp), vec!["cmp-1"], "comparisons reader");
}

#[tokio::test]
async fn limit_is_respected_and_rows_come_newest_first() {
    let (app, _) = app();
    for marker in ["obs-1", "obs-2", "obs-3"] {
        post_observation(&app, marker).await;
    }
    let (_, body) = get_as_guest(&app, "/api/estate/observations?limit=2").await;
    assert_eq!(
        markers(&body),
        vec!["obs-3", "obs-2"],
        "two newest, newest first"
    );
}

#[tokio::test]
async fn scope_selects_one_series_out_of_a_mixed_log() {
    let (app, _) = app();
    post_observation_in(&app, "codebase", "code-1").await;
    post_observation_in(&app, "kubernetes-nodes", "k8s-1").await;
    post_observation_in(&app, "host", "host-1").await;

    let (status, body) = get_as_guest(&app, "/api/estate/observations?scope=codebase").await;
    assert_eq!(status, StatusCode::OK, "scope= is guest-readable too");
    assert_eq!(markers(&body), vec!["code-1"], "only the codebase series");

    let (_, body) = get_as_guest(&app, "/api/estate/observations?scope=host").await;
    assert_eq!(markers(&body), vec!["host-1"], "only the host series");

    // A scope nobody has recorded is empty, not an error and not
    // everything — a typo in a URL must never silently widen the read.
    let (status, body) = get_as_guest(&app, "/api/estate/observations?scope=nonesuch").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"],
        serde_json::json!([]),
        "unknown scope is empty"
    );
}

/// THE DEFECT, REPRODUCED (measured live 2026-09-02).
///
/// `/api/estate/observations` caps at 50 rows newest-first across ALL
/// scopes. The estate observer runs every 15 minutes, so it fills all
/// 50 within ~12.5 hours; the nightly codebase observation is pushed
/// out of the window by fast-cadence neighbours and becomes
/// unreadable through the only exposed reader — invisible by
/// construction, not by outage. On the day this was measured the
/// ceiling held 49 `kubernetes-nodes` rows + 1 `host`, spanning
/// 04:30Z–17:30Z, and no `codebase` row could have survived there.
///
/// So the filter has to be applied where the LIMIT is — the same rule
/// `TailQuery::simulated` states in boss-events. Post one slow-cadence
/// observation, bury it under a full ceiling of fast ones, and it must
/// still be readable BY SCOPE.
#[tokio::test]
async fn a_slow_scope_survives_a_full_ceiling_of_a_fast_one() {
    let (app, _) = app();
    post_observation_in(&app, "codebase", "nightly").await;
    for i in 0..50 {
        post_observation_in(&app, "kubernetes-nodes", &format!("k8s-{i}")).await;
    }

    // Unfiltered, even at the hard ceiling, the nightly row is gone.
    let (_, unfiltered) = get_as_guest(&app, "/api/estate/observations?limit=50").await;
    assert!(
        !markers(&unfiltered).contains(&"nightly".to_string()),
        "precondition: the fast scope fills the whole ceiling"
    );

    // By scope it is reachable — and reachable at the DEFAULT limit,
    // because the filter runs before the limit rather than after it.
    let (_, scoped) = get_as_guest(&app, "/api/estate/observations?scope=codebase").await;
    assert_eq!(
        markers(&scoped),
        vec!["nightly"],
        "the slow scope is readable regardless of its neighbours' cadence"
    );
}

#[tokio::test]
async fn scope_and_limit_compose_on_the_comparisons_reader_too() {
    let (app, _) = app();
    post_comparison(&app, "cmp-1").await;
    post_comparison(&app, "cmp-2").await;

    let (_, body) = get_as_guest(
        &app,
        "/api/estate/comparisons?scope=kubernetes-nodes&limit=1",
    )
    .await;
    assert_eq!(
        markers(&body),
        vec!["cmp-2"],
        "newest of the scope, limited"
    );

    let (_, body) = get_as_guest(&app, "/api/estate/comparisons?scope=codebase").await;
    assert_eq!(
        body["data"],
        serde_json::json!([]),
        "comparisons filter by scope on the same key"
    );
}

/// Record `n` observations of one scope directly through the port, one
/// minute apart from `start`, oldest first — explicit timestamps, so a
/// window can be asserted to the row rather than to the wall clock.
async fn record_minutely(jobs: &InMemoryJobs, scope: &str, start: &str, n: i64) {
    let t0: chrono::DateTime<chrono::Utc> = start.parse().expect("an RFC 3339 instant");
    let events: Vec<boss_core::event::Event> = (0..n)
        .map(|i| {
            boss_core::event::Event::new(
                "jobs",
                boss_jobs::events::ESTATE_OBSERVED,
                serde_json::json!({"scope": scope, "marker": format!("{scope}-{i}")}),
                t0 + chrono::Duration::minutes(i),
            )
        })
        .collect();
    jobs.record_events(&events).await.expect("events record");
}

/// THE POST-MORTEM, REPRODUCED (backlog bf362f25, post-mortem 3c3b202c).
///
/// The readers answered the newest rows and nothing older: 30 hours
/// after an incident the oldest reachable observation was 2026-09-22
/// 20:52Z, and the window the post-mortem needed was behind it — while
/// the rows themselves were still in the log (the audit log is
/// append-only; measured 2026-09-23 through the events tail, every
/// estate row back to the log's first hour, 2026-09-16 23:58Z, was
/// there). A page with no way past its own ceiling is the defect
/// `scope=` fixed for a slow series, in time instead of cadence.
///
/// So `until=` (exclusive) is a before-cursor — pass the oldest
/// timestamp on the page you hold and the next page is the rows before
/// it — and `total` says how many rows the WINDOW holds, so a reader
/// knows whether its page is the whole answer (rows == total) or the
/// head of a longer one.
#[tokio::test]
async fn until_pages_back_past_the_ceiling_and_total_counts_the_window() {
    let (app, jobs) = app();
    record_minutely(&jobs, "host-units", "2026-09-22T00:00:00Z", 120).await;

    let (status, first) = get_as_guest(&app, "/api/estate/observations?limit=50").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(markers(&first).len(), 50, "the ceiling still holds");
    assert_eq!(markers(&first)[0], "host-units-119", "newest first");
    assert_eq!(
        first["total"], 120,
        "total counts every row the query matches, not the page"
    );

    // Walk back with the oldest timestamp on the page as the cursor.
    let cursor = first["data"][49]["timestamp"]
        .as_str()
        .expect("rows carry their timestamp")
        .to_string();
    let (status, second) = get_as_guest(
        &app,
        &format!("/api/estate/observations?limit=50&until={cursor}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the row's own timestamp is a cursor"
    );
    assert_eq!(
        markers(&second)[0],
        "host-units-69",
        "the next page starts at the row just before the cursor"
    );
    assert_eq!(second["total"], 70, "the window before the cursor");

    // The oldest rows are reachable at all — the property that was missing.
    let (_, oldest) = get_as_guest(
        &app,
        "/api/estate/observations?until=2026-09-22T00:05:00Z&limit=50",
    )
    .await;
    assert_eq!(
        markers(&oldest),
        vec![
            "host-units-4",
            "host-units-3",
            "host-units-2",
            "host-units-1",
            "host-units-0"
        ],
        "every row before the cursor, newest first"
    );
    assert_eq!(oldest["total"], 5);
}

/// `since=` is inclusive and `until=` exclusive — the half-open window
/// `/api/events/tail` states — and both compose with `scope=`, all of it
/// in the WHERE clause, so `total` answers for exactly the window asked.
#[tokio::test]
async fn since_and_until_bound_a_window_that_composes_with_scope() {
    let (app, jobs) = app();
    record_minutely(&jobs, "host-units", "2026-09-22T00:00:00Z", 30).await;
    record_minutely(&jobs, "host", "2026-09-22T00:00:30Z", 30).await;

    let (status, body) = get_as_guest(
        &app,
        "/api/estate/observations?scope=host-units\
         &since=2026-09-22T00:10:00Z&until=2026-09-22T00:13:00Z",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        markers(&body),
        vec!["host-units-12", "host-units-11", "host-units-10"],
        "since inclusive, until exclusive, one scope"
    );
    assert_eq!(body["total"], 3);

    // A window with nothing in it is empty with total 0, not an error.
    let (status, body) =
        get_as_guest(&app, "/api/estate/comparisons?since=2026-09-22T00:00:00Z").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], serde_json::json!([]));
    assert_eq!(body["total"], 0, "the comparisons reader counts too");
}

/// An instant that does not parse is REFUSED, never read as absent: an
/// unparsed `until` silently dropped would answer the newest page to a
/// reader asking for last Tuesday, confidently and wrongly.
#[tokio::test]
async fn an_unreadable_instant_is_refused_not_ignored() {
    let (app, _) = app();
    for uri in [
        "/api/estate/observations?until=yesterday",
        "/api/estate/comparisons?since=2026-13-01",
    ] {
        let (status, _) = get_as_guest(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is refused");
    }
}
