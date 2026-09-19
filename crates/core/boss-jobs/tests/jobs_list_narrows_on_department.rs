//! `GET /api/jobs?department=<code>` — a department's jobs, at the
//! layer that consumes them.
//!
//! WHY THIS FILE EXISTS. A packet carries no department; its workflow
//! row does (`metadata.department`). Until 2026-09-18 the listing
//! never read a `department` parameter, and an unknown query param is
//! silently ignored — so `?department=sales` on prod answered 1944,
//! the unfiltered total, with a straight face (backlog cc76f755). A
//! surface built on that would have shown every department the whole
//! company's work and called it theirs.
//!
//! The join (`boss_jobs::department`) and the port's kind-set filter
//! are covered in their own unit tests and in `postgres_filter.rs`.
//! What is only visible HERE is what sits between the query string
//! and the filter:
//!
//!   1. The param is named `department` and reaches the registry: the
//!      kinds whose ACTIVE row declares the code, and only those.
//!
//!   2. The control leg. A code nothing declares is `total: 0` — the
//!      probe that proves the server converged reads exactly this,
//!      because an old server answers the unfiltered count instead.
//!
//!   3. Without a registry the question cannot be answered, and the
//!      handler says so (503, like every other registry-backed door)
//!      rather than answering everything.

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
use boss_jobs::registry::{InMemoryWorkflows, WorkflowRegistry, WorkflowSpec};
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

fn spec(kind: &str, metadata: serde_json::Value) -> WorkflowSpec {
    let mut s = WorkflowSpec::platform_seed(kind, kind, "platform", vec![], vec![]);
    s.metadata = metadata;
    s
}

/// The tenant's registry as measured on prod, 2026-09-18: two kinds
/// declare `sales`, one `marketing`, one `finance`, and the platform
/// bundle declares nothing.
fn registry() -> Arc<InMemoryWorkflows> {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for s in [
        spec(
            "receive-an-inquiry",
            serde_json::json!({ "department": "sales" }),
        ),
        spec(
            "receive-a-sponsorship",
            serde_json::json!({ "department": "sales" }),
        ),
        spec(
            "publish-the-landing-page",
            serde_json::json!({ "department": "marketing" }),
        ),
        spec(
            "receive-a-payout",
            serde_json::json!({ "department": "finance" }),
        ),
        spec("backlog-item", serde_json::json!({})),
    ] {
        kinds.seed(s).expect("seed");
    }
    kinds
}

fn app(kinds: Option<Arc<InMemoryWorkflows>>) -> (Router, Arc<InMemoryJobs>) {
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
        now: day(2026, 9, 18)
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
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: kinds.map(|k| k as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock,
        cadence: None,
        delivery: None,
        agent_budget: None,
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

fn packet(n: u8, kind: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", "algedonic"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 1),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
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
        packet(1, "receive-an-inquiry", "an inquiry"),
        packet(2, "receive-a-sponsorship", "a sponsorship"),
        packet(3, "publish-the-landing-page", "the landing page"),
        packet(4, "backlog-item", "a backlog item"),
        packet(5, "backlog-item", "another backlog item"),
    ] {
        jobs.create_job(&j).await.expect("seed");
    }
}

#[tokio::test]
async fn department_returns_the_packets_of_the_kinds_declaring_it() {
    let (app, jobs) = app(Some(registry()));
    seed(&jobs).await;

    let all = list(&app, "limit=1").await;
    assert_eq!(
        all["total"], 5,
        "the unfiltered count — what the trap answered"
    );

    let sales = list(&app, "department=sales").await;
    assert_eq!(sales["total"], 2, "got {:?}", titles(&sales));
    assert_eq!(
        titles(&sales),
        vec!["a sponsorship".to_string(), "an inquiry".to_string()]
    );

    let marketing = list(&app, "department=marketing").await;
    assert_eq!(titles(&marketing), vec!["the landing page".to_string()]);

    // A department with a declaring kind and no packets of it yet.
    let finance = list(&app, "department=finance").await;
    assert_eq!(finance["total"], 0);
}

/// The control leg. The probe that proves this landed reads
/// `?department=no-such-department-zz` and expects 0; an old server
/// ignores the param and answers the unfiltered count.
#[tokio::test]
async fn a_department_nobody_declares_is_zero_not_everything() {
    let (app, jobs) = app(Some(registry()));
    seed(&jobs).await;

    let body = list(&app, "department=no-such-department-zz&limit=1").await;
    assert_eq!(body["total"], 0, "got {:?}", titles(&body));
    assert_eq!(body["data"].as_array().map(Vec::len), Some(0));
}

/// Composes with the other filters as an intersection.
#[tokio::test]
async fn department_intersects_with_kind_and_status() {
    let (app, jobs) = app(Some(registry()));
    seed(&jobs).await;

    let body = list(&app, "department=sales&kind=receive-an-inquiry").await;
    assert_eq!(titles(&body), vec!["an inquiry".to_string()]);

    // A kind outside the department: the intersection is empty.
    let body = list(&app, "department=sales&kind=backlog-item").await;
    assert_eq!(body["total"], 0);

    let body = list(&app, "department=sales&status=closed").await;
    assert_eq!(body["total"], 0);
}

/// No registry, no answer: 503 like every other registry-backed door,
/// never the unfiltered list.
#[tokio::test]
async fn without_a_registry_the_question_is_refused_not_answered_with_everything() {
    let (app, jobs) = app(None);
    seed(&jobs).await;

    let (status, body) = get(&app, "department=sales").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("registry"), "{body}");

    // The listing without the param is unaffected.
    let all = list(&app, "limit=1").await;
    assert_eq!(all["total"], 5);
}
