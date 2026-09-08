//! When the SubjectExistenceCheck is plumbed, POST /api/jobs rejects
//! ghost subject ids with 400 BadRequest before the row lands in
//! storage. NotFound = reject; Unavailable = fail-open (warn + accept)
//! so a flaky upstream doesn't wedge the create path.
//!
//! These tests use the `InMemorySubjectExistenceCheck` from
//! `boss_jobs::subject_existence::test_helpers` so they don't
//! require an upstream stack.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::event::Event;
use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::subject_existence::{SubjectExistenceCheck, SubjectExistenceError};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, Resource, Scope};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use async_trait::async_trait;
use std::sync::Mutex;

/// Pre-populated existence checker. `accounts` / `assets` start
/// empty; tests insert known ids before exercising the handler.
struct TestExistence {
    accounts: Mutex<std::collections::HashSet<String>>,
    assets: Mutex<std::collections::HashSet<String>>,
}

impl TestExistence {
    fn new() -> Self {
        Self {
            accounts: Mutex::new(std::collections::HashSet::new()),
            assets: Mutex::new(std::collections::HashSet::new()),
        }
    }
    fn with_account(self, id: &str) -> Self {
        self.accounts.lock().unwrap().insert(id.to_string());
        self
    }
    fn with_asset(self, id: &str) -> Self {
        self.assets.lock().unwrap().insert(id.to_string());
        self
    }
}

#[async_trait]
impl SubjectExistenceCheck for TestExistence {
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
        let id = subject.id.clone();
        let set = match subject.kind.as_str() {
            "account" => &self.accounts,
            "asset" => &self.assets,
            // Other kinds intentionally return Ok — keeps the
            // tests focused on the path under test.
            _ => return Ok(()),
        };
        if set.lock().unwrap().contains(&id) {
            Ok(())
        } else {
            Err(SubjectExistenceError::NotFound(id))
        }
    }
}

/// Existence checker that always returns Unavailable, so we can
/// assert the fail-open path.
struct AlwaysUnavailable;

#[async_trait]
impl SubjectExistenceCheck for AlwaysUnavailable {
    async fn check(&self, _subject: &Subject) -> Result<(), SubjectExistenceError> {
        Err(SubjectExistenceError::Unavailable("upstream 503".into()))
    }
}

fn build_app(existence: Option<Arc<dyn SubjectExistenceCheck>>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(boss_jobs::step_registry::StepRegistry::v1());
    // Permissive policy — every test caller gets Create on Job.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: existence,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn ceo_header() -> String {
    json!({
        "id": "emp-cto",
        "role": "ceo",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn job_with_subject(subject: Subject) -> Job {
    Job {
        id: boss_core::job::JobId::from_uuid(Uuid::new_v4()),
        kind: "ad-hoc".into(),
        workflow_version: 1,
        subject,
        title: "Smoke".into(),
        owner_id: "emp-cto".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated: false,
    }
}

async fn post_job(app: Router, job: &Job) -> axum::http::Response<Body> {
    let body = serde_json::to_vec(job).unwrap();
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", ceo_header())
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn create_rejected_when_account_does_not_exist() {
    let existence: Arc<dyn SubjectExistenceCheck> = Arc::new(TestExistence::new());
    let (app, _jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("account", "acc-bigseed-9999"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("subject does not exist"),
        "expected NotFound message, got: {text}"
    );
}

#[tokio::test]
async fn create_accepted_when_account_exists() {
    let existence: Arc<dyn SubjectExistenceCheck> =
        Arc::new(TestExistence::new().with_account("acc-bigseed-0001"));
    let (app, jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("account", "acc-bigseed-0001"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        jobs.list_jobs(&Default::default(), 100, 0)
            .await
            .unwrap()
            .0
            .len(),
        1
    );
}

#[tokio::test]
async fn create_accepted_when_system_exists() {
    let existence: Arc<dyn SubjectExistenceCheck> =
        Arc::new(TestExistence::new().with_asset("SN-12345"));
    let (app, _jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("asset", "SN-12345"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn create_rejected_when_system_does_not_exist() {
    let existence: Arc<dyn SubjectExistenceCheck> = Arc::new(TestExistence::new());
    let (app, _jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("asset", "SN-NOPE"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_fails_closed_when_check_is_unavailable() {
    // Q2 (subject-model design, resolved 2026-07-15): abort by
    // default. An unverifiable subject is the phantom class the gate
    // exists to stop — and with the Pg adapter, "unavailable" means
    // the same database the create would write to anyway.
    let existence: Arc<dyn SubjectExistenceCheck> = Arc::new(AlwaysUnavailable);
    let (app, jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("account", "acc-anything"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        jobs.list_jobs(&Default::default(), 100, 0)
            .await
            .unwrap()
            .0
            .len(),
        0,
        "no job may land while the gate cannot verify"
    );
}

#[tokio::test]
async fn check_skipped_when_no_validator_plumbed() {
    // Existence checker is None: every subject is accepted regardless
    // of whether it exists in any backing store.
    let (app, _jobs) = build_app(None);

    let job = job_with_subject(Subject::new("account", "acc-bigseed-9999"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn vendor_subject_falls_through_validator() {
    // Vendor doesn't have a per-id GET endpoint upstream so the
    // validator skips it. Even when the validator is plumbed, vendor
    // subjects pass.
    let existence: Arc<dyn SubjectExistenceCheck> = Arc::new(TestExistence::new());
    let (app, _jobs) = build_app(Some(existence));

    let job = job_with_subject(Subject::new("vendor", "vnd-anything"));
    let resp = post_job(app, &job).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

// Silence unused-import warnings on the few imports the helper
// types pull in but don't use directly here.
#[allow(dead_code)]
fn _silence_imports(_: Event) {}
