//! A policy outage answers 503, never a 403 and never an empty page.
//!
//! Backlog 45553536 (triage of 5d4ad086, 2026-09-25, the rollout of
//! train #689): the policy service was dark for seconds, and for about a
//! minute every packet read answered `403 reading packets is refused:
//! policy-unreachable` while every list answered `total: 0`. Two
//! defects in one: boss-policy-client turned the outage into a Deny and
//! CACHED it for its 60 s TTL, and the jobs API rendered that Deny as a
//! permission fact. A reader could not tell "not allowed" from "could
//! not ask", and an empty list is the shape data loss takes.
//!
//! Now the client answers an outage as `PolicyClientError::Unreachable`
//! (still fail-closed: nothing gets an Allow), and every jobs-API door
//! renders it as 503 + `Retry-After`, keeping the word
//! `policy-unreachable` in the body that boss-cli's
//! `names_a_policy_outage` keys on.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, Decision, PolicyClient, PolicyClientError, Predicate, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const PACKET: &str = "45553536-0000-0000-0000-000000000001";

/// A policy client whose service cannot be asked — what the reqwest
/// adapter now answers for a refused connection, a timeout or a 5xx.
struct DarkPolicy;

#[async_trait]
impl PolicyClient for DarkPolicy {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Err(PolicyClientError::Unreachable(
            "error sending request: connection refused".into(),
        ))
    }

    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Err(PolicyClientError::Unreachable(
            "error sending request: connection refused".into(),
        ))
    }
}

fn operator() -> User {
    User {
        id: "emp-david".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("platform".into()),
    }
}

fn packet_id() -> JobId {
    JobId::from_uuid(Uuid::parse_str(PACKET).expect("uuid"))
}

async fn app(policy: Arc<dyn PolicyClient>) -> (Router, String) {
    let jobs = Arc::new(InMemoryJobs::new());
    jobs.create_job(&Job {
        id: packet_id(),
        kind: "brew-day".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "FV-1"),
        title: "a packet the operator may read".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    })
    .await
    .expect("packet");
    let mut step = Step::new(packet_id(), "task", "do the thing", 0).with_assignee("emp-david");
    step.status = StepStatus::Ready;
    let step_id = step.id.to_string();
    jobs.add_step(&step).await.expect("step");

    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), step_id)
}

struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    body: String,
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let mut req = Request::builder().method(method).uri(uri).header(
        "x-boss-user",
        serde_json::to_string(&operator()).expect("a User serialises"),
    );
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    Answer {
        status,
        retry_after,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn assert_outage(what: &str, a: &Answer) {
    assert_eq!(
        a.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{what}: an outage is a 503, not a permission answer: {}",
        a.body
    );
    assert_eq!(
        a.retry_after.as_deref(),
        Some(
            boss_policy_client::POLICY_OUTAGE_RETRY_AFTER_SECS
                .to_string()
                .as_str()
        ),
        "{what}: carries Retry-After"
    );
    assert!(
        a.body.contains("policy-unreachable"),
        "{what}: keeps the word boss-cli keys on: {}",
        a.body
    );
    assert!(
        !a.body.contains("\"total\""),
        "{what}: not a page of anything: {}",
        a.body
    );
}

#[tokio::test]
async fn every_packet_read_answers_503_through_a_dark_policy_service() {
    let (app, _) = app(Arc::new(DarkPolicy)).await;
    for uri in [
        format!("/api/jobs/{PACKET}"),
        format!("/api/jobs/{PACKET}/events"),
        format!("/api/jobs/{PACKET}/steps"),
        "/api/jobs/assignments?all_assigned=true".to_string(),
    ] {
        let a = send(&app, "GET", &uri, None).await;
        assert_outage(&uri, &a);
    }
}

#[tokio::test]
async fn a_list_answers_503_not_an_empty_page() {
    let (app, _) = app(Arc::new(DarkPolicy)).await;
    for uri in [
        "/api/jobs",
        "/api/jobs?kind=brew-day",
        "/api/jobs/queue-age",
    ] {
        let a = send(&app, "GET", uri, None).await;
        assert_outage(uri, &a);
    }
}

#[tokio::test]
async fn a_step_write_answers_503_and_writes_nothing() {
    let (app, step_id) = app(Arc::new(DarkPolicy)).await;
    let a = send(
        &app,
        "PUT",
        &format!("/api/jobs/{PACKET}/steps/{step_id}"),
        Some(serde_json::json!({ "status": "active" })),
    )
    .await;
    assert_outage("PUT step", &a);
}

// -- The wire: the real client, a policy service that fails once. ----

/// A policy service that answers 503 to its first check, then Allow.
async fn flaky_policy() -> (String, Arc<AtomicUsize>) {
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let app = Router::new().route(
        "/api/policy/check",
        axum::routing::post(move || {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    axum::Json(Decision::Allow { scope: Scope::All }).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_during_a_policy_blip_is_503_and_the_next_read_is_answered() {
    let (url, seen) = flaky_policy().await;
    let (app, _) = app(Arc::new(boss_policy_client::ReqwestPolicyClient::new(url))).await;
    let uri = format!("/api/jobs/{PACKET}");

    let during = send(&app, "GET", &uri, None).await;
    assert_outage("during the blip", &during);

    // The policy service is back. Until 45553536 the outage was cached
    // as a Deny for 60 s, so this answered 403 for another minute.
    let after = send(&app, "GET", &uri, None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert!(after.body.contains(PACKET), "{}", after.body);
    assert_eq!(seen.load(Ordering::SeqCst), 2, "asked again, not cached");
}
