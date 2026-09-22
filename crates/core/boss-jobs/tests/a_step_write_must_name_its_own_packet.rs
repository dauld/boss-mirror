//! A step write must name the packet the step actually belongs to.
//!
//! THE DEFECT (packet 730ea77e), measured live on 2026-09-21 against
//! packet 8c9e1190: `PUT /api/jobs/{id}/steps/{step_id}` finds the step
//! by its own id ALONE and never checks that the path's job id names
//! that step's packet. A fabricated job id was accepted, the step
//! flipped on the REAL packet, and the caller got 204.
//!
//! WHAT THAT COSTS, and why it is a correctness matter rather than
//! tidiness. The parent job is read as `.ok().flatten()`, so a job id
//! that resolves to nothing becomes `None` and the write proceeds on
//! `None`:
//!
//!   - PROVENANCE. The emitted `step.done` / `step.assigned` events fall
//!     back to `String::new()` for `subject_kind`, `subject_id` and
//!     `workflow_kind` — exactly the fields dispatcher rules and fact
//!     projections match on. The audit row is immutable and permanently
//!     stripped of its provenance, and every rule keyed on the packet's
//!     workflow or subject silently does not fire.
//!   - CLOSURE. `if let Some(job) = parent_job` guards BOTH
//!     `reevaluate_and_persist` and `close_job_on_terminal`, so no
//!     downstream step becomes ready and no terminal closes the packet.
//!     In the live reproduction the `closed` terminal's `ready_when` was
//!     already satisfied and the packet still sat open, wedged, with no
//!     error anywhere.
//!
//! THE SMALLEST TRUE FIX, and it is smaller than the packet supposed.
//! The packet named three handlers carrying the same `.ok().flatten()`.
//! Two of them — `patch_step_metadata` and `claim_step` — ALREADY refuse
//! a mismatch with `step not on this job`. Only the PUT, the most-used
//! write endpoint of the three, was missing the check its siblings
//! carry. So this is one `if` restoring a rule the file already states,
//! not a new rule.
//!
//! WHY 404 AND NOT 409. The path names a (job, step) pair, and when the
//! step is not on that job the pair names nothing. That is the word the
//! two sibling handlers already use, verbatim.
//!
//! WHY THE `None` JOB IS LEFT ALONE. After the containment check a
//! matching `job_id` is the step's real parent, so a `None` there means
//! an orphan step or a repository error, not a caller mistake — a
//! different defect, and one the two siblings do not treat either.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

/// The packet the step really belongs to.
const JOB: &str = "8c9e1190-d7dd-4373-9f88-3e64d8ff2a4f";
/// The same prefix with the rest zeroed — the exact shape a session
/// fabricated by hand and the API accepted.
const FABRICATED: &str = "8c9e1190-0000-0000-0000-000000000000";
/// A second REAL packet. The more dangerous half: the event would carry
/// this packet's subject and workflow while the write lands on the other.
const OTHER_JOB: &str = "00000000-0000-0000-0000-0000000000a2";
const STEP: &str = "00000000-0000-0000-0000-0000000000b1";

fn operator() -> User {
    User {
        id: "emp-op".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn allow_update() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    )
}

fn job(id: &str, kind: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", kind),
        title: "A packet with a step on it".into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn step() -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(STEP).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "generic".into(),
        title: "Build the change".into(),
        spec_slug: Some("build".into()),
        assignee_id: Some("emp-op".into()),
        status: StepStatus::Ready,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        allow_update(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    jobs.create_job(&job(JOB, "backlog-item")).await.unwrap();
    jobs.create_job(&job(OTHER_JOB, "pr-train")).await.unwrap();
    jobs.add_step(&step()).await.unwrap();
    (router(state), jobs)
}

async fn put(app: &Router, job_id: &str, body: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{job_id}/steps/{STEP}"))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_of(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// THE BUG, in the shape it was measured: a job id that names no packet
/// at all. The step must not move.
#[tokio::test]
async fn a_put_through_a_job_id_that_names_no_packet_is_refused() {
    let (app, jobs) = seed().await;

    let resp = put(&app, FABRICATED, r#"{"status":"completed"}"#).await;
    let status = resp.status();
    let body = body_of(resp).await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a step addressed through a packet that does not exist is not found there; body: {body}"
    );
    assert!(
        body.contains("step not on this job"),
        "the refusal names the containment rule, the same words the sibling handlers use; body: {body}"
    );

    let after = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("the step is still there");
    assert_eq!(
        after.status,
        StepStatus::Ready,
        "the refused write must not have flipped the step on the real packet"
    );
}

/// THE MORE DANGEROUS HALF: a job id naming a REAL but different packet.
/// Here `parent_job` resolves, so the write would not merely lose its
/// provenance — the event would carry the WRONG packet's subject and
/// workflow, and the engine would re-evaluate a packet the step is not on.
#[tokio::test]
async fn a_put_through_another_real_packet_is_refused() {
    let (app, jobs) = seed().await;

    let resp = put(&app, OTHER_JOB, r#"{"status":"completed"}"#).await;
    let status = resp.status();
    let body = body_of(resp).await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a step is not found under a packet it does not belong to; body: {body}"
    );

    let after = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("the step is still there");
    assert_eq!(
        after.status,
        StepStatus::Ready,
        "the refused write must not have flipped the step"
    );
}

/// THE CONTROL, and it is what stops this from being a test that would
/// pass against a handler that refused everything: the SAME body through
/// the step's OWN packet still lands.
#[tokio::test]
async fn the_same_write_through_the_right_packet_still_lands() {
    let (app, jobs) = seed().await;

    let resp = put(&app, JOB, r#"{"status":"completed"}"#).await;
    let status = resp.status();
    let body = body_of(resp).await;
    assert!(
        status.is_success(),
        "the correct pair must still be accepted, or the refusal above proves nothing; {status} {body}"
    );

    let after = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("the step is still there");
    assert_eq!(
        after.status,
        StepStatus::Completed,
        "the write through the right packet flips the step"
    );
}
