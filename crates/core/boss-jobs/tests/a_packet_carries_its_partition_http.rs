//! `GET /api/jobs?partition=real|simulated|shadow` and the N-1
//! `simulated=` spelling, at the layer that consumes them; and the
//! admission edge's refusal of a shadow body (packet 508cc38c, car 1 of
//! 4).
//!
//! What is only visible HERE, between the query string and the port:
//!
//!   1. `partition=` reaches `JobFilter.partition`, and each word keeps
//!      exactly its own packets. A filter that fell through would
//!      return everything, which looks like working software — and
//!      "0 shadow packets" and "every packet" must never share a body.
//!
//!   2. The N-1 spelling keeps its meaning: `simulated=false` is the
//!      real lane (so an old caller excludes shadow packets without
//!      knowing the word), `simulated=true` is exactly the simulated
//!      company (the sim never sees the shadow lane, Q5).
//!
//!   3. An unknown word is a 400 naming the vocabulary, not the
//!      unfiltered count — the control leg every probe of this door
//!      stands on.
//!
//!   4. The wire carries both keys per packet: `partition` and the
//!      derived `simulated` (not-real), so an old client reading a
//!      shadow packet reads it as not-real.
//!
//!   5. Nothing admits a shadow packet yet: a POST body claiming
//!      `partition: shadow` is refused. Shadow admission is the
//!      experiment lane's (car 3), not a body flag.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::partition::Partition;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
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
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day(2026, 9, 15)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
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

fn packet(n: u8, title: &str, partition: Partition) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        kind: "keg-return".into(),
        workflow_version: 4,
        subject: Subject::new("asset", "KEG-1"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 1),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition,
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
    let mut t: Vec<String> = body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|j| j["title"].as_str().unwrap_or_default().to_string())
        .collect();
    t.sort();
    t
}

async fn seed(jobs: &InMemoryJobs) {
    for j in [
        packet(1, "real one", Partition::Real),
        packet(2, "real two", Partition::Real),
        packet(3, "sim one", Partition::Simulated),
        packet(4, "shadow one", Partition::Shadow),
    ] {
        jobs.create_job(&j).await.expect("seed");
    }
}

#[tokio::test]
async fn partition_keeps_exactly_one_lane_and_absent_is_everything() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let all = list(&app, "limit=10").await;
    assert_eq!(all["total"], 4);

    let real = list(&app, "partition=real&limit=10").await;
    assert_eq!(real["total"], 2);
    assert_eq!(titles(&real), vec!["real one", "real two"]);

    let sim = list(&app, "partition=simulated&limit=10").await;
    assert_eq!(sim["total"], 1);
    assert_eq!(titles(&sim), vec!["sim one"]);

    let shadow = list(&app, "partition=shadow&limit=10").await;
    assert_eq!(shadow["total"], 1);
    assert_eq!(titles(&shadow), vec!["shadow one"]);
}

#[tokio::test]
async fn the_legacy_simulated_spelling_keeps_its_meaning_and_excludes_shadow() {
    let (app, jobs) = app();
    seed(&jobs).await;

    // simulated=false is the real lane: the shadow packet is excluded
    // exactly as the simulated one is — an old caller fails closed.
    let real = list(&app, "simulated=false&limit=10").await;
    assert_eq!(real["total"], 2);
    assert_eq!(titles(&real), vec!["real one", "real two"]);

    // simulated=true is exactly the simulated company: no shadow.
    let sim = list(&app, "simulated=true&limit=10").await;
    assert_eq!(sim["total"], 1);
    assert_eq!(titles(&sim), vec!["sim one"]);

    // partition wins when both are sent.
    let both = list(&app, "simulated=true&partition=shadow&limit=10").await;
    assert_eq!(titles(&both), vec!["shadow one"]);
}

#[tokio::test]
async fn an_unknown_partition_is_refused_naming_the_vocabulary() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let (status, body) = get(&app, "partition=nonsense&limit=10").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("real, simulated, shadow"),
        "the 400 names the vocabulary: {body}"
    );
}

#[tokio::test]
async fn every_packet_carries_both_spellings_on_the_wire() {
    let (app, jobs) = app();
    seed(&jobs).await;

    let shadow = list(&app, "partition=shadow&limit=10").await;
    let row = &shadow["data"][0];
    assert_eq!(row["partition"], "shadow");
    assert_eq!(
        row["simulated"], true,
        "the derived bool is not-real, so an N-1 client reads a shadow packet as not real"
    );
    let real = list(&app, "partition=real&limit=10").await;
    assert_eq!(real["data"][0]["partition"], "real");
    assert_eq!(real["data"][0]["simulated"], false);
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

#[tokio::test]
async fn a_body_claiming_the_shadow_partition_is_refused_at_admission() {
    let (app, jobs) = app();

    let mut body = serde_json::to_value(packet(9, "dry run", Partition::Shadow)).unwrap();
    body["status"] = serde_json::json!("open");
    let (status, text) = post_job(&app, body).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "nothing admits a shadow packet yet (car 3): {text}"
    );
    assert!(text.contains("experiment lane"), "{text}");
    let (_, total) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    assert_eq!(total, 0, "the refusal wrote nothing");

    // The two lanes that exist keep admitting: an N-1 body that sends
    // only `simulated: true` is the simulated company, and a plain
    // body is real.
    let mut legacy = serde_json::to_value(packet(8, "sim body", Partition::Real)).unwrap();
    legacy.as_object_mut().unwrap().remove("partition");
    legacy["simulated"] = serde_json::json!(true);
    legacy["status"] = serde_json::json!("open");
    let (status, text) = post_job(&app, legacy).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let created: serde_json::Value = serde_json::from_str(&text).unwrap();
    let id = JobId::from_uuid(Uuid::parse_str(created["id"].as_str().unwrap()).unwrap());
    let stored = jobs.get_job(&id).await.unwrap().unwrap();
    assert_eq!(
        stored.partition,
        Partition::Simulated,
        "an N-1 body that sends only the bool is admitted as the simulated company"
    );
}
