//! A packet is created whole or not at all — the handler half
//! (backlog f2ba226e; the Postgres half is the `_pg` twin).
//!
//! `POST /api/jobs` used to commit the job through `create_job_at`
//! and then write each materialized step through `add_step_at`, one
//! transaction each, warning on a failed write and answering 201
//! regardless. On 2026-09-16 (pr-train 06e5610f) a slow database and
//! a client timeout left nine of ten steps and no terminal. The
//! handler now hands the job AND its steps to
//! `create_job_with_steps_at` in one call, on one stamp, and records
//! the `step.ready` markers only after that call returns.
//!
//! Pinned through the in-memory port, not the adapter: every step's
//! STEP_CREATED carries the job's own instant (one stamp — a step
//! written in a later transaction would carry a later one), the
//! whole graph is in the store when the handler answers, and no
//! `step.ready` marker is recorded before the last STEP_CREATED.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::events::{JOB_CREATED, STEP_CREATED};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The pr-train's shape on the night: ten steps, the first ready.
const STEP_COUNT: usize = 10;

fn operator_header() -> String {
    serde_json::json!({
        "id": "emp-ceo",
        "role": "ceo",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn ten_step_kind() -> WorkflowSpec {
    let steps = (0..STEP_COUNT)
        .map(|i| StepSpec {
            title: format!("s{i}"),
            kind: "task".into(),
            ready_when: if i == 0 {
                "true".into()
            } else {
                format!("s{}.completed", i - 1)
            },
            title_template: format!("Step {i}"),
            ..Default::default()
        })
        .collect();
    WorkflowSpec::platform_seed(
        "ten-step-kind",
        "Ten step kind",
        "test",
        vec!["custom".into()],
        steps,
    )
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(ten_step_kind()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
        dispatcher_firings: None,
        agent_budget: None,
    };
    (router(state), jobs)
}

async fn post_job(app: &axum::Router) -> String {
    let body = serde_json::json!({
        "kind": "ten-step-kind",
        "subject": { "subject_kind": "custom", "id": "doc-1" },
        "title": "A packet",
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": "standard",
        "metadata": {},
        "tags": [],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", operator_header())
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_whole_graph_is_admitted_on_one_stamp_and_ready_is_marked_after() {
    let (app, jobs) = app();
    let id = post_job(&app).await;
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(&id).unwrap());

    // The graph is whole when the handler answers.
    let steps = jobs.list_steps(&job_id).await.unwrap();
    assert_eq!(
        steps.len(),
        STEP_COUNT,
        "every materialized step is in the store"
    );
    assert_eq!(
        steps[0].status,
        boss_core::job::StepStatus::Ready,
        "the open-time readiness pass survives the one-call admission"
    );

    let events = jobs.recorded_events();
    let created: Vec<_> = events.iter().filter(|e| e.kind == JOB_CREATED).collect();
    assert_eq!(created.len(), 1);
    let step_created: Vec<_> = events.iter().filter(|e| e.kind == STEP_CREATED).collect();
    assert_eq!(
        step_created.len(),
        STEP_COUNT,
        "one STEP_CREATED per step — the rebuilder's source"
    );
    // ONE stamp: the job and every step carry the same instant. Ten
    // stamps minted across ten transactions (the old loop) spread
    // over thirty seconds on the night this was filed.
    let admitted_at = created[0].timestamp;
    assert!(
        step_created.iter().all(|e| e.timestamp == admitted_at),
        "every STEP_CREATED is stamped at the job's admission instant"
    );

    // The `step.ready` markers come AFTER the last STEP_CREATED — a
    // consumer reacting to one sees the whole graph.
    let last_created = events
        .iter()
        .rposition(|e| e.kind == STEP_CREATED)
        .expect("steps were created");
    let first_ready = events
        .iter()
        .position(|e| e.kind.starts_with("step.ready."))
        .expect("the ready step was marked");
    assert!(
        first_ready > last_created,
        "step.ready at {first_ready} precedes the last STEP_CREATED at {last_created}"
    );
}

/// The in-memory adapter honours the port's refusal too: a graph
/// whose events do not pair with its rows writes nothing — no job,
/// no steps, no events — rather than zipping short.
#[tokio::test]
async fn the_in_memory_port_refuses_an_unpaired_graph_without_writing() {
    use boss_core::actor::ActorId;
    use boss_core::job::{Job, Priority, Step, Subject};
    use boss_core::publisher::EventStamp;

    let jobs = InMemoryJobs::new();
    let job = Job::new(
        "ten-step-kind",
        Subject::new("custom", "doc-1"),
        "A packet",
        "emp-ceo",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
    );
    let steps: Vec<Step> = (0..3)
        .map(|i| Step::new(job.id, "task", format!("Step {i}"), i))
        .collect();
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let job_event = stamp.event(JOB_CREATED, serde_json::to_value(&job).unwrap());
    // Two events for three steps.
    let step_events: Vec<_> = steps
        .iter()
        .take(2)
        .map(|s| stamp.event(STEP_CREATED, boss_jobs::events::step_state_payload(s)))
        .collect();

    let err = jobs
        .create_job_with_steps_at(&job, &steps, stamp.timestamp, &[job_event], &step_events)
        .await
        .expect_err("3 steps, 2 events");
    assert!(err.to_string().contains("3 step"), "{err}");
    assert!(jobs.get_job(&job.id).await.unwrap().is_none());
    assert!(jobs.list_steps(&job.id).await.unwrap().is_empty());
    assert!(jobs.recorded_events().is_empty());
}
