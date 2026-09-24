//! A second completion of an already-completed step writes nothing.
//!
//! Measured on car 6b23d135, 2026-09-24T22:18:10Z (backlog 29a7ea09).
//! A direct PUT completed the car's `disproved` outcome terminal, and
//! `close_job_on_terminal` stamped `jobs.job.closed` with
//! `outcome=disproved` at .556307. The same terminal had gone READY a
//! moment earlier, so the dispatcher rule `complete-marker-on-step-ready`
//! PUT `{"status":"completed"}` onto it too. That second PUT read the
//! step AFTER the first had completed it — its .576623 row is a bare
//! `jobs.step.updated`, with no `jobs.step.completed` beside it — so it
//! was an idempotent re-send and changed nothing about the step. But it
//! still ran the rest of the handler: it rewrote the step row, re-ran
//! the re-evaluator, and ran the all-steps-terminal catch-all close,
//! which read the JOB before the first writer's close had committed,
//! saw it open, and wrote the whole row back as closed at .586476 —
//! with no `outcome`. Both closes are whole-row read-modify-write, so
//! the later commit erased the earlier one's key.
//!
//! A re-send that changes nothing is a no-op, and a no-op writes
//! nothing: no step row, no event, no re-evaluation, no close. The
//! catch-all then only ever runs for a write that moved something,
//! which is the only write that can have made a packet closable.
//!
//! The first test reproduces the measured interleaving deterministically:
//! it stops the first writer between its step write and its job close
//! (the window the second writer's read fell into) by making the step
//! write through the repository, then sends the dispatcher's body.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const KIND: &str = "a-marker-completed-twice";

/// One piece of work, then an `outcome` marker that is a declared
/// terminal — the shape of the ship-a-change `disproved` step: a
/// no-role, zero-duration marker the dispatcher auto-completes when it
/// goes ready, and which a direct writer may complete first.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "A marker completed twice",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "disproved".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Disproved".into(),
                terminal: Some(Terminal {
                    outcome: "disproved".into(),
                }),
                ..Default::default()
            },
        ],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

struct Harness {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
}

fn harness() -> Harness {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    Harness {
        app: router(state),
        jobs,
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn open_job(app: &axum::Router) -> JobId {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": KIND,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": "A car whose marker is completed twice",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    JobId::from_uuid(Uuid::parse_str(job["id"].as_str().expect("job id")).expect("uuid"))
}

async fn step(jobs: &InMemoryJobs, job_id: &JobId, slug: &str) -> boss_core::job::Step {
    jobs.list_steps(job_id)
        .await
        .expect("list steps")
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step `{slug}`"))
}

/// The dispatcher's own body (`jobs.complete_step`): the status alone.
async fn put_completed(
    app: &axum::Router,
    job_id: &JobId,
    step: &boss_core::job::Step,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{}", step.id))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

fn events_for(jobs: &InMemoryJobs, job_id: &JobId) -> Vec<boss_core::event::Event> {
    let want = job_id.to_string();
    jobs.recorded_events()
        .into_iter()
        .filter(|e| {
            e.payload.get("job_id").and_then(|v| v.as_str()) == Some(want.as_str())
                || e.payload.get("id").and_then(|v| v.as_str()) == Some(want.as_str())
        })
        .collect()
}

/// The measured interleaving. The first writer has completed the
/// terminal and not yet closed the packet; the dispatcher's re-send
/// lands in that window. It must write nothing — in particular it must
/// not close the packet, because its close carries no `outcome`, and
/// once a packet is closed the terminal's own close no-ops, so the
/// outcome the terminal declared is never recorded.
#[tokio::test]
async fn a_resend_between_the_terminal_write_and_its_close_does_not_close_the_packet() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "work completes: {body}");
    let marker = step(&h.jobs, &job_id, "disproved").await;
    assert_eq!(
        marker.status,
        StepStatus::Ready,
        "precondition: the marker goes ready behind the work"
    );

    // The first writer, stopped after its step write: the terminal is
    // completed on the row, the packet is not yet closed.
    let mut first = marker.clone();
    first.status = StepStatus::Completed;
    first.completed_on = Some(chrono::Utc::now().date_naive());
    h.jobs
        .update_step(&first)
        .await
        .expect("first writer's step write");

    let job_before = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    let events_before = events_for(&h.jobs, &job_id).len();

    // The dispatcher's re-send.
    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an idempotent re-send still answers 204: {body}"
    );

    let job_after = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    let written: Vec<String> = events_for(&h.jobs, &job_id)
        .into_iter()
        .skip(events_before)
        .map(|e| e.kind)
        .collect();
    assert!(
        written.is_empty(),
        "a re-send that changes nothing must record nothing; it recorded {written:?}"
    );
    assert_eq!(
        job_after.status, job_before.status,
        "the re-send closed the packet without the terminal's outcome \
         (metadata after: {})",
        job_after.metadata
    );
    assert_eq!(job_after.metadata, job_before.metadata);
}

/// After the terminal's own close, the re-send is still a no-op: the
/// outcome stays, and nothing is written — not even the bare
/// `jobs.step.updated` the measured re-send left at .576623.
#[tokio::test]
async fn a_resend_after_the_close_records_nothing_and_keeps_the_outcome() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let work = step(&h.jobs, &job_id, "work").await;
    let (status, body) = put_completed(&h.app, &job_id, &work).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "work completes: {body}");
    let marker = step(&h.jobs, &job_id, "disproved").await;
    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the terminal completes: {body}"
    );

    let closed = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(closed.metadata["outcome"], "disproved", "precondition");
    let events_before = events_for(&h.jobs, &job_id).len();

    let (status, body) = put_completed(&h.app, &job_id, &marker).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the re-send answers 204: {body}"
    );

    let written: Vec<String> = events_for(&h.jobs, &job_id)
        .into_iter()
        .skip(events_before)
        .map(|e| e.kind)
        .collect();
    assert!(
        written.is_empty(),
        "a re-send that changes nothing must record nothing; it recorded {written:?}"
    );
    let after = h.jobs.get_job(&job_id).await.unwrap().expect("job");
    assert_eq!(after.metadata["outcome"], "disproved");
    assert_eq!(after, closed, "the packet row is untouched");
}
