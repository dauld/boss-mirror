//! End-to-end coverage for the `workflow-publish` StepType
//! dispatch path.
//!
//! When a step of kind `workflow-publish` flips to Done via PUT
//! /api/jobs/{id}/steps/{step_id}, the handler must:
//! 1. Pull `workflow_spec` from the step metadata.
//! 2. Hand it to the registry, which gates it on viability.
//! 3. Call `WorkflowRegistry::publish_authored(spec, job_id, actor, now)`
//!    — the registry records `jobs.kind.published` (full published
//!    spec) atomically with the workflows row; the step path no
//!    longer emits its own copy.
//! 4. Persist STEP_UPDATED only AFTER the registry write succeeds.
//!
//! Decision record: `docs/architecture-decisions.md` §Jobs,
//! Workflows, Steps (Workflows bootstrap through Jobs).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::events::WORKFLOW_PUBLISHED;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn cto() -> User {
    User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("executive".into()),
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn build_app(
    kinds: Arc<dyn WorkflowRegistry>,
) -> (Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        step_registry,
        kind_registry: Some(kinds),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            publisher,
            policy,
            std::sync::Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, bus)
}

async fn seed_publish_step(
    jobs: &dyn JobsRepository,
    metadata: serde_json::Value,
) -> (JobId, StepId) {
    use boss_core::job::Job as JobRow;
    let mut job = JobRow::new(
        "workflow-design",
        Subject::new("workflow", "morning-brew"),
        "Design morning-brew",
        "emp-cto",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    let job_id = job.id;
    jobs.create_job(&job).await.unwrap();

    // Single active step that will flip to Done in the test.
    let step = Step {
        id: StepId::new(),
        job_id,
        kind: "workflow-publish".into(),
        title: "Publish".into(),
        spec_slug: None,
        assignee_id: None,
        status: StepStatus::Active,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata,
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    };
    let step_id = step.id;
    jobs.add_step(&step).await.unwrap();
    (job_id, step_id)
}

fn valid_spec(kind: &str) -> WorkflowSpec {
    // Must pass the viability gate `publish_authored` enforces:
    // a viable trigger → terminal pair.
    WorkflowSpec::platform_seed(
        kind,
        "Morning Brew",
        "production",
        vec!["location".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "brewed".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

async fn put_step_done(
    app: &Router,
    job_id: JobId,
    step_id: StepId,
    user_json: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job_id, step_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_json)
                .body(Body::from(json!({ "status":"completed" }).to_string()))
                .unwrap(),
        )
        .await
        .expect("router responds")
}

#[tokio::test]
async fn done_dispatches_publish_authored_and_emits_kind_published_event() {
    // Concrete handle: `recorded_events()` is the InMemory window
    // onto what the Pg adapter records in the row transaction.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "workflow_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    let status = resp.status();
    assert!(
        status.is_success(),
        "PUT step → done must succeed, got {status}"
    );

    // Registry now has the published kind, with the meta-Job's id
    // recorded as authoring_job_id.
    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(live.kind, "morning-brew");
    assert_eq!(live.version, 1);
    assert_eq!(live.status, WorkflowStatus::Active);
    assert_eq!(
        live.authoring_job_id.expect("authoring stamped"),
        *job_id.inner().as_uuid(),
    );

    // The audit-bearing event landed — recorded by the REGISTRY
    // adapter atomically with the workflows row (registry-events
    // car), no longer pushed into the step-update write. The
    // in-memory registry collects what the Pg adapter records
    // in-tx.
    let events = kinds.recorded_events();
    let published: Vec<_> = events
        .iter()
        .filter(|e| e.kind == WORKFLOW_PUBLISHED)
        .collect();
    assert_eq!(
        published.len(),
        1,
        "exactly one jobs.kind.published event should fire"
    );
    let payload = &published[0].payload;
    assert_eq!(payload["kind"], "morning-brew");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["status"], "active");
    // The actor is the session user who flipped the step.
    assert_eq!(payload["_actor"], "emp-cto");

    // The step path must NOT duplicate it — one write, one event.
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED),
        "the step-update write no longer carries jobs.kind.published"
    );
}

#[tokio::test]
async fn a_publish_refused_as_stale_publishes_once_when_it_is_sent_again() {
    // Backlog 558396ff, from the review of car 88123ae0. The registry
    // write runs BEFORE the step write (so a refused publish never
    // records a completed step), and since 88123ae0 the step write can
    // be refused — 409, "nothing was written; send the same request
    // again". The registry row HAD been written: each re-send published
    // the same spec as one more version, retiring the one before it.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());
    let spec = valid_spec("morning-brew");
    let metadata = json!({ "workflow_spec": serde_json::to_value(&spec).unwrap() });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    // A reviewer's note lands between the handler's read and its write.
    let mut note = serde_json::Map::new();
    note.insert("review_note".into(), json!("ship it"));
    jobs.merge_after_next_read(&step_id, note);
    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "precondition");

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert!(
        resp.status().is_success(),
        "the re-send completes the step, got {}",
        resp.status()
    );

    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(
        live.version, 1,
        "one publish step, one published version — not one per attempt"
    );
    let published = kinds
        .recorded_events()
        .iter()
        .filter(|e| e.kind == WORKFLOW_PUBLISHED)
        .count();
    assert_eq!(published, 1, "and one jobs.kind.published");
    let step = jobs.get_step(&step_id).await.unwrap().unwrap();
    assert_eq!(step.status, StepStatus::Completed);
}

#[tokio::test]
async fn a_resend_after_the_spec_was_edited_publishes_the_edited_spec() {
    // The round-2 review of car 983696b5 (SF3): the re-send answer was
    // keyed on the authoring packet alone. A publish refused as stale
    // had written v1; the spec on the step was then edited; the re-send
    // found v1 authored by this packet, answered it, and completed the
    // step recording a spec the registry never published. Only the SAME
    // spec is answered with the earlier publish.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());
    let spec = valid_spec("morning-brew");
    let metadata = json!({ "workflow_spec": serde_json::to_value(&spec).unwrap() });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let mut note = serde_json::Map::new();
    note.insert("review_note".into(), json!("one more change"));
    jobs.merge_after_next_read(&step_id, note);
    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "precondition");
    assert_eq!(
        kinds
            .get_active("morning-brew")
            .await
            .expect("active")
            .label,
        "Morning Brew",
        "precondition: the refused attempt's publish stands"
    );

    let mut edited = spec.clone();
    edited.label = "Morning Brew, revised".into();
    let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
    step.metadata["workflow_spec"] = serde_json::to_value(&edited).unwrap();
    jobs.update_step(&step).await.unwrap();

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert!(resp.status().is_success(), "{}", resp.status());
    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(
        live.label, "Morning Brew, revised",
        "the registry holds the spec the completed step records"
    );
    assert_eq!(live.version, 2, "published as a new version, not answered");
}

#[tokio::test]
async fn a_resend_after_another_packet_published_answers_its_own_row_and_keeps_theirs() {
    // Backlog 4bdb8150 (the round-3 review of car 983696b5). The re-send
    // answer consulted only the ACTIVE row. Packet A publishes and its
    // step write is refused as stale; packet B then publishes the same
    // kind; A re-sends. The active row is B's, so A's check failed and A
    // published its spec AGAIN — one more version, retiring B's newer
    // publish: last writer wins, and B's design silently stopped being
    // the protocol. A's publish happened once; its re-send is answered
    // with the row A authored, active or retired.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());
    let spec = valid_spec("morning-brew");
    let metadata = json!({ "workflow_spec": serde_json::to_value(&spec).unwrap() });
    let (job_a, step_a) = seed_publish_step(jobs.as_ref(), metadata).await;

    let mut note = serde_json::Map::new();
    note.insert("review_note".into(), json!("ship it"));
    jobs.merge_after_next_read(&step_a, note);
    let resp = put_step_done(&app, job_a, step_a, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "precondition");

    let job_b = JobId::new();
    let mut theirs = valid_spec("morning-brew");
    theirs.label = "Morning Brew, B's design".into();
    kinds
        .publish_authored(
            theirs,
            job_b,
            &boss_core::actor::ActorId::Human("emp-other".into()),
            chrono::Utc::now(),
        )
        .await
        .expect("B publishes between A's refusal and A's re-send");

    let resp = put_step_done(&app, job_a, step_a, &user_header(&cto())).await;
    assert!(
        resp.status().is_success(),
        "A's re-send completes its step: {}",
        resp.status()
    );

    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(
        live.authoring_job_id,
        Some(*job_b.inner().as_uuid()),
        "B's newer publish stays the active version"
    );
    assert_eq!(live.label, "Morning Brew, B's design");
    let by_a: Vec<_> = kinds
        .list_versions("morning-brew")
        .await
        .unwrap()
        .into_iter()
        .filter(|v| v.authoring_job_id == Some(*job_a.inner().as_uuid()))
        .collect();
    assert_eq!(
        by_a.len(),
        1,
        "one version authored by A, not one per attempt: {by_a:?}"
    );
    assert_eq!(by_a[0].status, WorkflowStatus::Retired);
    assert_eq!(
        jobs.get_step(&step_a).await.unwrap().unwrap().status,
        StepStatus::Completed
    );
}

#[tokio::test]
async fn an_earlier_version_by_another_packet_is_not_mistaken_for_this_publish() {
    // The idempotence key is the AUTHORING PACKET, not the kind: a kind
    // already active from someone else's design is published over, as
    // it always was.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let other = boss_core::job::JobId::new();
    kinds
        .publish_authored(
            valid_spec("morning-brew"),
            other,
            &boss_core::actor::ActorId::Human("emp-other".into()),
            chrono::Utc::now(),
        )
        .await
        .expect("an earlier publish by another packet");
    let (app, jobs, _bus) = build_app(kinds.clone());
    let metadata = json!({
        "workflow_spec": serde_json::to_value(valid_spec("morning-brew")).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert!(resp.status().is_success(), "{}", resp.status());
    let live = kinds.get_active("morning-brew").await.expect("active");
    assert_eq!(live.version, 2);
    assert_eq!(live.authoring_job_id, Some(*job_id.inner().as_uuid()));
}

#[tokio::test]
async fn missing_workflow_spec_metadata_returns_400_no_publish() {
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let (job_id, step_id) =
        seed_publish_step(jobs.as_ref(), json!({ "previous_kind_version": 0 })).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "missing workflow_spec must abort the step write"
    );

    // No publish event should have recorded — neither by the
    // registry (the write never happened) nor by the step path.
    assert!(
        kinds.recorded_events().is_empty(),
        "the registry must record nothing when dispatch fails"
    );
    let events = jobs.recorded_events();
    assert!(
        events.iter().all(|e| e.kind != WORKFLOW_PUBLISHED),
        "no jobs.kind.published event must fire when dispatch fails"
    );

    // STEP_UPDATED must NOT have landed — the dispatch fails before
    // update_step_at is called, preserving audit_log integrity.
    let updated_count = events
        .iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .count();
    assert_eq!(
        updated_count, 0,
        "STEP_UPDATED must not be emitted when dispatch aborts"
    );

    // Registry untouched.
    assert!(
        kinds.list_active(None).await.unwrap().is_empty(),
        "registry must stay empty when dispatch aborts"
    );
}

#[tokio::test]
async fn malformed_workflow_spec_returns_400() {
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    let (job_id, step_id) = seed_publish_step(
        jobs.as_ref(),
        json!({ "workflow_spec": "not even an object" }),
    )
    .await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    assert!(kinds.recorded_events().is_empty());
    let events = jobs.recorded_events();
    assert!(events.iter().all(|e| e.kind != WORKFLOW_PUBLISHED));
}

#[tokio::test]
async fn unviable_workflow_spec_returns_422_and_publishes_nothing() {
    // The Step dispatch path sets a registry row ACTIVE without a
    // draft ever existing, so it answers to the publish gate too
    // (2026-08-13). The refusal is 422 with the lint problems, and
    // the step must NOT flip to done behind a failed registry write.
    let kinds = Arc::new(InMemoryWorkflows::new());
    let (app, jobs, _bus) = build_app(kinds.clone());

    // Viable shape minus the outcome — the incident's exact defect.
    let mut spec = valid_spec("morning-brew");
    spec.steps[1].terminal = None;
    let metadata = json!({ "workflow_spec": serde_json::to_value(&spec).unwrap() });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    assert!(
        kinds.get_active("morning-brew").await.is_err(),
        "an unviable spec must not reach the active slot by any path"
    );
    assert!(kinds.recorded_events().is_empty());
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED)
    );
}

#[tokio::test]
async fn publish_step_without_kind_registry_returns_503() {
    // Mirror prod's degraded mode: the registry handle is unset.
    // Dispatch should refuse rather than silently no-op so the
    // operator notices the misconfiguration.
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Update, Resource::step(), Scope::All)
            .allow("cto", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        step_registry,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            publisher,
            policy,
            std::sync::Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state);

    let spec = valid_spec("morning-brew");
    let metadata = json!({
        "workflow_spec": serde_json::to_value(&spec).unwrap(),
    });
    let (job_id, step_id) = seed_publish_step(jobs.as_ref(), metadata).await;

    let resp = put_step_done(&app, job_id, step_id, &user_header(&cto())).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// Silence unused import warnings when this file is the only one
// touching these names.
#[allow(dead_code)]
fn _ensure_uuid_used(_id: Uuid) {}
