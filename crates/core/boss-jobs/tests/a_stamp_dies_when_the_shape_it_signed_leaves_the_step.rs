//! A STAMP DIES WHEN THE SHAPE IT SIGNED LEAVES THE STEP (backlog
//! c085256d, design 87329a13, option C decided 2026-09-25).
//!
//! A sign-off stamp binds the `shape_hash` of the step it attested.
//! Until this car the server honoured ANY stamp whose hash matched the
//! current shape, so an A-B-A edit revived one: David approves (S1 on
//! X), withdraws — by Reject (S2 on Y) or by Request changes (no stamp
//! at all) — and anyone who can write step metadata PATCHes X back
//! through the merge door. S1 matched again and satisfied completion.
//! The `jobs.step.stamps_invalidated` event was emitted at the edit and
//! nothing on the step recorded that S1 had died.
//!
//! Now the edit that moves the shape voids every stamp still alive on
//! the step, in the edit's own write: the stamp stays on the step for
//! provenance, carrying `voided_at` and `voided_by_event`, and the event
//! lists what it voided. One predicate, `Step::live_stamps`, judges the
//! completion, names the 409's roles and decides whether a sign-off is
//! an idempotent re-stamp. These run at both edit doors (the merge door
//! and the whole-step PUT) and over both withdrawals. The rebuild and
//! the backfill of rows already written are pinned beside the Pg
//! adapter, in `a_stamp_dies_when_the_shape_it_signed_leaves_the_step_pg`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const ROLE: &str = "qa-lead";
const APPROVED_AT: &str = "2026-09-25T07:00:00Z";

fn approver() -> User {
    User {
        id: "emp-qa".into(),
        role: ROLE.into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("qa".into()),
    }
}

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(ROLE, Action::Update, Resource::step(), Scope::All)
            .allow(
                ROLE,
                Action::SignOff,
                Resource::new(format!("step-signoff:{ROLE}")),
                Scope::All,
            )
            .build(),
    );
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

/// The content an approval signs: the decision and its instant sit
/// inside the shape, as both approval surfaces save them BEFORE the
/// stamp.
fn approved() -> Value {
    json!({"authority_role": ROLE, "decision": "approved", "decided_at": APPROVED_AT})
}

/// A sign-off step, open, holding the approved content — "X".
async fn seed(jobs: &Arc<InMemoryJobs>, id: &str) -> Step {
    let job = Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "A packet with an approval on it".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "sign-off", "Approve the change", 0)
        .with_sign_offs_required(vec![ROLE.into()]);
    step.status = StepStatus::Ready;
    step.metadata = approved();
    jobs.add_step(&step).await.unwrap();
    step
}

async fn send(app: &Router, method: &str, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&approver()).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

async fn sign(app: &Router, step: &Step) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        format!("/api/jobs/{}/steps/{}/sign-offs", step.job_id, step.id),
        json!({"role": ROLE}),
    )
    .await
}

/// The merge door — the one an approval surface saves its decision
/// through, and the one the exploit restores X through.
async fn merge(app: &Router, step: &Step, patch: Value) -> StatusCode {
    send(
        app,
        "PATCH",
        format!("/api/jobs/{}/steps/{}/metadata", step.job_id, step.id),
        patch,
    )
    .await
    .0
}

/// The whole-step PUT, the other door that moves a step's content.
async fn put(app: &Router, step: &Step, body: Value) -> (StatusCode, Value) {
    send(
        app,
        "PUT",
        format!("/api/jobs/{}/steps/{}", step.job_id, step.id),
        body,
    )
    .await
}

async fn stored(jobs: &Arc<InMemoryJobs>, step: &Step) -> Value {
    serde_json::to_value(jobs.get_step(&step.id).await.unwrap().unwrap()).unwrap()
}

fn invalidations(jobs: &Arc<InMemoryJobs>) -> Vec<Value> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.step.stamps_invalidated")
        .map(|e| e.payload)
        .collect()
}

/// THE COMPLETION REFUSES, and says which role must sign again.
async fn assert_completion_refused(app: &Router, step: &Step, why: &str) {
    let (status, body) = put(app, step, json!({"status": "completed"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
    assert_eq!(
        body["missing_or_stale_roles"],
        json!([ROLE]),
        "{why}: {body}"
    );
}

/// WITHDRAWAL BY REQUEST CHANGES, which writes no stamp. The surface
/// saves the decision and returns (`changes_requested_completes` unset),
/// so the only withdrawal on record is the content change — and it is
/// enough: the approval died when X left the step, and restoring X
/// byte for byte does not bring it back. A fresh signature on X is a
/// new stamp, not an "idempotent" answer pointing at the dead one.
#[tokio::test]
async fn an_approval_withdrawn_by_request_changes_does_not_come_back() {
    let (app, jobs) = build_app();
    let step = seed(&jobs, "00000000-0000-0000-0000-00000000c801").await;

    let (status, body) = sign(&app, &step).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let s1_shape = body["sign_offs"][0]["shape_hash"].clone();
    // An unchanged step re-signed is the same signature, answered as one.
    let (_, again) = sign(&app, &step).await;
    assert_eq!(again["sign_offs"].as_array().unwrap().len(), 1, "{again}");

    // Request changes: the decision saved, nothing stamped.
    let st = merge(
        &app,
        &step,
        json!({"decision": "changes-requested", "decided_at": "2026-09-25T07:05:00Z"}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let row = stored(&jobs, &step).await;
    assert!(
        row["sign_offs"][0]["voided_at"].is_string(),
        "the edit that moved the shape voided the stamp on the step itself: {row}"
    );
    assert!(row["sign_offs"][0]["voided_by_event"].is_string(), "{row}");
    let inv = invalidations(&jobs);
    assert_eq!(inv.len(), 1, "one invalidation, from the one edit: {inv:?}");
    assert_eq!(
        inv[0]["voided"][0]["shape_hash"], s1_shape,
        "the event carries the stamp it voided: {inv:?}"
    );
    assert_eq!(
        inv[0]["voided"][0]["voided_by_event"], row["sign_offs"][0]["voided_by_event"],
        "and the stamp names that event"
    );

    // The A-B-A: X restored byte for byte through the merge door.
    let st = merge(
        &app,
        &step,
        json!({"decision": "approved", "decided_at": APPROVED_AT}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let row = stored(&jobs, &step).await;
    assert_eq!(
        boss_core::job::step_shape_hash(&step.title, &row["metadata"]),
        s1_shape.as_str().unwrap(),
        "the step is back on exactly the shape S1 signed"
    );
    assert!(
        row["sign_offs"][0]["voided_at"].is_string(),
        "a void is permanent — a later write does not lift it: {row}"
    );
    assert_completion_refused(&app, &step, "a dead stamp satisfied completion").await;

    // The approver signs X afresh: a NEW stamp, and then it completes.
    let (status, body) = sign(&app, &step).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["sign_offs"].as_array().unwrap().len(),
        2,
        "a dead stamp does not make a fresh signature idempotent: {body}"
    );
    assert!(body["sign_offs"][1]["voided_at"].is_null(), "{body}");
    let (status, body) = put(&app, &step, json!({"status": "completed"})).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// WITHDRAWAL BY REJECT, which stamps the rejection. Both stamps die:
/// S1 when the rejection moved the content, S2 when X came back.
#[tokio::test]
async fn an_approval_withdrawn_by_reject_does_not_come_back() {
    let (app, jobs) = build_app();
    let step = seed(&jobs, "00000000-0000-0000-0000-00000000c802").await;

    assert_eq!(sign(&app, &step).await.0, StatusCode::OK);
    let st = merge(
        &app,
        &step,
        json!({"decision": "rejected", "decided_at": "2026-09-25T07:05:00Z"}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (status, body) = sign(&app, &step).await;
    assert_eq!(status, StatusCode::OK, "the rejection is signed: {body}");
    assert_eq!(body["sign_offs"].as_array().unwrap().len(), 2, "{body}");

    let st = merge(
        &app,
        &step,
        json!({"decision": "approved", "decided_at": APPROVED_AT}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let row = stored(&jobs, &step).await;
    let stamps = row["sign_offs"].as_array().unwrap();
    assert!(
        stamps.iter().all(|s| s["voided_at"].is_string()),
        "the approval and the rejection both died with their shapes: {row}"
    );
    assert_eq!(invalidations(&jobs).len(), 2);
    assert_completion_refused(&app, &step, "the revived approval satisfied completion").await;
}

/// THE WHOLE-STEP PUT is the other door that moves content, and it
/// voids the same way: its own `jobs.step.updated` carries the void (the
/// row the rebuild replays), and so does the row.
#[tokio::test]
async fn an_approval_edited_away_through_the_put_does_not_come_back() {
    let (app, jobs) = build_app();
    let step = seed(&jobs, "00000000-0000-0000-0000-00000000c803").await;
    assert_eq!(sign(&app, &step).await.0, StatusCode::OK);

    let mut rejected = approved();
    rejected["decision"] = json!("rejected");
    let (status, body) = put(&app, &step, json!({"metadata": rejected})).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let updated: Vec<Value> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .map(|e| e.payload)
        .collect();
    let last = updated.last().expect("the PUT recorded its state event");
    assert!(
        last["sign_offs"][0]["voided_at"].is_string(),
        "the state event the rebuild replays carries the void: {last}"
    );
    assert_eq!(invalidations(&jobs).len(), 1);

    let (status, body) = put(&app, &step, json!({"metadata": approved()})).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let row = stored(&jobs, &step).await;
    assert!(
        row["sign_offs"][0]["voided_at"].is_string(),
        "restoring the content through the PUT lifts nothing: {row}"
    );
    assert_completion_refused(&app, &step, "the PUT revived a dead stamp").await;
}
