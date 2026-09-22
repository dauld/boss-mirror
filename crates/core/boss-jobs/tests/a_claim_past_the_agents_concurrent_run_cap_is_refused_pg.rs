//! The claim door bounds how many runs an agent has IN FLIGHT
//! (backlog 57c108c2, 2026-09-20) — the half the money gate
//! structurally cannot cover.
//!
//! `agent_runs` is written at FINISH, so a run that has been claimed
//! and not reported is priced at nothing and the hourly reservation
//! admits the next claim, and the next. Measured 2026-09-19: station
//! `a.platform-admin.opus-5-1m` held 47 ready packets, and a puller
//! firing `boss dispatch --next` on an interval would have claimed
//! every one of them under a budget that could not yet see the
//! previous claim. This is the bound: the actor's OPEN `agent-run`
//! packets, judged against `agents.max_concurrent_runs` — the column
//! that until now was read only by the recorder, after a run finished,
//! where it can record a fact and stop nothing.
//!
//! IT BOUNDED ACTIVE CLAIMS UNTIL c314921e, AND THE PROXY DEADLOCKED
//! THE QUEUE. A backlog-item's `build` does not drain at the handback;
//! it drains when its car closes, and a `ship-a-change` does not close
//! until it is PROVEN in prod. So the bound measured the proof backlog
//! — 7 of 6 in flight against an open-run population of ZERO — and one
//! of the seven was a car whose own probe wanted a fresh dispatch, so
//! the event that would have proven it was the event it forbade.
//!
//! `boss dispatch` claims the step and THEN files the run, so a puller
//! firing on an interval is still bounded: each dispatch's run exists
//! before the next one's claim is judged. Two dispatches overlapping
//! inside that one window can both be admitted — a narrower race than
//! the proxy's, and the direction a serial operator never sees.
//!
//! Against the Pg adapters, through the same claim endpoint `boss
//! dispatch` calls, on the rows the schema ships (`agent-claude`,
//! migration 20260915212644, caps NULL).
//!
//! Contracts:
//! 1. At the cap the claim is a 409 naming the count and the cap; the
//!    step is still `ready`, unassigned — it never entered the race.
//! 2. Under the cap the claim is admitted exactly as before.
//! 3. A person claiming the same step is admitted: the cap is the
//!    agent's, and a person doing the work by hand is allowed.
//! 4. A row with NO cap (the seeded default) bounds nothing.
//! 5. A step still Active from a run that is OVER holds no slot —
//!    the regression that deadlocked the queue.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::{AgentRunLog, PgAgentRuns};
use boss_jobs::agents::{AgentInput, AgentsRegistry, PgAgents};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{JobsRepository, PgJobs};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use uuid::Uuid;

const AGENT: &str = "agent-claude";
const PACKET: &str = "cccccccc-0000-4000-8000-000000000001";

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "Nothing calls boss dispatch --next".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A run in flight: the `agent-run` packet `boss dispatch` files right
/// after its claim is admitted. Open, and naming its agent.
fn open_run(agent: &str) -> Job {
    Job {
        id: JobId::new(),
        kind: "agent-run".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "run".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "agent": agent }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A ready step carrying car 1's projection of an agent block — what a
/// station queue hands out and what a puller would claim.
fn build_step(slug: &str, sort_order: i32) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        job_id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        kind: "task".into(),
        title: slug.into(),
        spec_slug: Some(slug.into()),
        assignee_id: None,
        status: StepStatus::Ready,
        sort_order,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({
            "authority_role": "platform-admin",
            "agent_profile": "builder",
            "agent_model": "opus-5",
            "agent_budget_usd": 1.0,
            "agent_effort": "high",
        }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// The seeded row, re-declared with a concurrency cap (take mode: the
/// instance is the truth, and this test IS the operator's edit).
async fn cap_the_agent(db: &TestDb, max_concurrent_runs: Option<i32>) {
    PgAgents::new(db.pool.clone())
        .publish(
            &[AgentInput {
                id: AGENT.into(),
                display_name: "Claude (engineering)".into(),
                default_model: "opus-5".into(),
                aliases: vec!["claude@algedonic.dev".into()],
                role: None,
                department: None,
                hourly_budget_usd_micros: None,
                max_concurrent_runs,
            }],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the row takes the cap");
}

/// The app with `n` ready agent-blocked steps on one open packet.
async fn app_with_steps(db: &TestDb, n: usize) -> (axum::Router, Arc<PgJobs>, Vec<String>) {
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    jobs.create_job(&packet()).await.expect("packet lands");
    let mut ids = Vec::new();
    for i in 0..n {
        let step = build_step(&format!("build-{i}"), i as i32);
        jobs.add_step(&step).await.expect("step lands");
        ids.push(step.id.to_string());
    }
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let agents: Arc<dyn AgentsRegistry> = Arc::new(PgAgents::new(db.pool.clone()));
    let runs: Arc<dyn AgentRunLog> = Arc::new(PgAgentRuns::new(db.pool.clone()));
    let app = router(JobsApiState {
        agent_budget: Some(Arc::new(BudgetDoor { agents, runs })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    (app, jobs, ids)
}

async fn claim(
    app: &axum::Router,
    step_id: &str,
    as_user: &str,
    expect: StatusCode,
) -> serde_json::Value {
    let resp = TestRequest::post(format!("/api/jobs/{PACKET}/steps/{step_id}/claim"))
        .as_user(as_user, "platform-admin")
        .send(app)
        .await;
    resp.assert_status(expect);
    resp.assert_json()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_claim_at_the_cap_is_refused_with_the_count() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(1)).await;
    let (app, jobs, steps) = app_with_steps(&db, 2).await;

    // The first run: nothing in flight, admitted.
    let body = claim(&app, &steps[0], AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT);
    assert_eq!(body["status"], "active");

    // …and `boss dispatch` files its run, which is what the next claim
    // is judged against.
    jobs.create_job(&open_run(AGENT)).await.expect("run lands");

    // The second, which a puller firing again a minute later would
    // make: one run in flight, cap of one.
    let body = claim(&app, &steps[1], AGENT, StatusCode::CONFLICT).await;
    assert_eq!(body["error"], "claim past the agent's concurrent-run cap");
    assert_eq!(body["actor"], AGENT);
    assert_eq!(body["in_flight"], 1);
    assert_eq!(body["max_concurrent_runs"], 1);
    assert!(
        body["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("1 of 1 runs in flight"),
        "{body}"
    );

    // It never entered the race.
    let step = jobs
        .get_step(&StepId::from_uuid(Uuid::parse_str(&steps[1]).unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.status, StepStatus::Ready);
    assert_eq!(step.assignee_id, None);

    // A person doing the work by hand is admitted: the cap is the
    // agent's.
    let body = claim(&app, &steps[1], "emp-david", StatusCode::OK).await;
    assert_eq!(body["assignee_id"], "emp-david");
}

/// THE DEADLOCK (backlog c314921e). A `build` step whose run was
/// handed back stays Active until its car closes, and the car waits to
/// be PROVEN — which can take days, and in the measured case could
/// only happen via the dispatch this gate was refusing. The run is
/// closed, so the slot is free, so the claim is admitted.
#[tokio::test(flavor = "multi_thread")]
async fn a_claim_left_active_by_a_finished_run_holds_no_slot() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(1)).await;
    let (app, jobs, steps) = app_with_steps(&db, 2).await;

    // A run that ran and was reported: its packet is CLOSED.
    let mut done = open_run(AGENT);
    done.status = JobStatus::Closed;
    done.closed_on = NaiveDate::from_ymd_opt(2026, 9, 20);
    jobs.create_job(&done).await.expect("run lands");

    // Its step is still Active — the car has landed and is not proven.
    let body = claim(&app, &steps[0], AGENT, StatusCode::OK).await;
    assert_eq!(body["status"], "active");

    // Under the old bound this was 1 of 1 and the queue stopped here.
    let body = claim(&app, &steps[1], AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT);
    assert_eq!(body["status"], "active");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undeclared_cap_bounds_nothing() {
    // The seeded default — `max_concurrent_runs` NULL — admits every
    // claim, the boot-guard rule every cap reader here follows.
    let db = TestDb::new().await;
    let (app, jobs, steps) = app_with_steps(&db, 3).await;
    for step_id in &steps {
        jobs.create_job(&open_run(AGENT)).await.expect("run lands");
        let body = claim(&app, step_id, AGENT, StatusCode::OK).await;
        assert_eq!(body["assignee_id"], AGENT);
    }
}
