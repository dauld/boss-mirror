//! The claim door reserves an agent-blocked step's budget against the
//! agent's hour (design c87fb59b car 3, backlog cb78818d).
//!
//! `agent_runs` judges a run against `agents.hourly_budget_usd_micros`
//! at FINISH — a fact, not a gate. This is the gate: at the claim,
//! before the CAS, the actor's priced spend in the last hour plus the
//! step's `agent_budget_usd` must fit under the cap, else 409 carrying
//! the three numbers and the step stays in its queue.
//!
//! Against the Pg adapters, through the claim endpoint the pod's
//! `boss dispatch` calls, on the rows the schema ships: `agent-claude`
//! (migration 20260915212644, caps NULL), the seeded rate card, and one
//! run this test records so the hour holds a priced spend.
//!
//! Since backlog e6b2066f the money half is a READING, not a gate: the
//! file keeps its name so its history reads straight, and contract 1
//! says what the door does now.
//!
//! Contracts:
//! 1. A cap the reservation would exceed ADMITS the claim and commits
//!    `agents.claim.over_budget` beside it, naming spent, budget and
//!    cap.
//! 2. A reservation that fits is admitted: the step goes `active` under
//!    the agent, exactly as a claim did before this car.
//! 3. A person claiming the same step is admitted — the budget is the
//!    agent's, and a person doing the work by hand is allowed.
//! 4. A row with NO cap (the seeded default) reserves nothing.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::{AgentRunLog, NewAgentRun, PgAgentRuns, RunOutcome, TokenUsage};
use boss_jobs::agents::{AgentInput, AgentsRegistry, PgAgents};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{JobsRepository, PgJobs, StationRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

const AGENT: &str = "agent-claude";
const PACKET: &str = "bbbbbbbb-0000-4000-8000-000000000001";

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "Agent controls car 3".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A ready `build` step carrying car 1's projection of its agent block
/// — the four keys materialisation writes — under `budget_usd`, on
/// `model`.
fn build_step(budget_usd: f64, model: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        job_id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        kind: "task".into(),
        title: "build".into(),
        spec_slug: Some("build".into()),
        assignee_id: None,
        status: StepStatus::Ready,
        sort_order: 0,
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
            "agent_model": model,
            "agent_budget_usd": budget_usd,
            "agent_effort": "high",
        }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// The seeded row, re-declared with a cap (take mode: the instance is
/// the truth, and this test IS the operator's edit).
async fn cap_the_agent(db: &TestDb, hourly_budget_usd_micros: Option<i64>) {
    PgAgents::new(db.pool.clone())
        .publish(
            &[AgentInput {
                id: AGENT.into(),
                display_name: "Claude (engineering)".into(),
                default_model: "opus-5".into(),
                aliases: vec!["claude@algedonic.dev".into()],
                role: None,
                department: None,
                hourly_budget_usd_micros,
                max_concurrent_runs: None,
            }],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the row takes the cap");
}

/// One priced run in the hour: 128,684 in + 14,298 out on the seeded
/// opus-5 row = 1,000,870 usd_micros (the same numbers agent_runs_pg
/// prices), finished a minute ago.
async fn spend_a_dollar(db: &TestDb) {
    let finished_at = Utc::now() - chrono::Duration::minutes(1);
    PgAgentRuns::new(db.pool.clone())
        .record_run(
            &NewAgentRun {
                run_id: "run-an-hour-ago".into(),
                actor_id: ActorId::RegisteredAgent(AGENT.into()),
                model: Some("opus-5".into()),
                started_at: finished_at - chrono::Duration::minutes(10),
                finished_at,
                outcome: RunOutcome::Success,
                error: None,
                tokens: TokenUsage::Split {
                    input: 128_684,
                    output: 14_298,
                },
                tool_calls: 3,
                job_id: None,
                branch: None,
                detail: serde_json::Value::Null,
            },
            &ActorId::Automation("test".into()),
        )
        .await
        .expect("the run records");
}

async fn app_with_step(db: &TestDb, budget_usd: f64) -> (axum::Router, Arc<PgJobs>, String) {
    app_with_step_on(db, budget_usd, "opus-5", false).await
}

/// The app, with the step's block on `model` and, when `with_stations`,
/// the Pg station registry wired so a claim may name its station.
async fn app_with_step_on(
    db: &TestDb,
    budget_usd: f64,
    model: &str,
    with_stations: bool,
) -> (axum::Router, Arc<PgJobs>, String) {
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    jobs.create_job(&packet()).await.expect("packet lands");
    let step = build_step(budget_usd, model);
    jobs.add_step(&step).await.expect("step lands");
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
        stations: with_stations.then(|| {
            Arc::new(boss_jobs::PgStations::new(db.pool.clone())) as Arc<dyn StationRegistry>
        }),
        agent_budget: Some(Arc::new(BudgetDoor { agents, runs })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    (app, jobs, step.id.to_string())
}

/// The claim, as `boss dispatch` makes it, answered with the status
/// expected and the JSON body.
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

/// Backlog e6b2066f: the hour the reservation cannot hold ADMITS the
/// claim and puts the reading on the log beside it — a cost signal,
/// never a refusal (David 2026-09-23: budgets must not limit building).
#[tokio::test(flavor = "multi_thread")]
async fn a_claim_the_hour_cannot_hold_is_admitted_and_the_reading_is_on_the_log() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(3_000_000)).await;
    spend_a_dollar(&db).await;
    let (app, _jobs, step_id) = app_with_step(&db, 5.0).await;

    let body = claim(&app, &step_id, AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT, "{body}");
    assert_eq!(body["status"], "active", "{body}");

    let reading: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'agents.claim.over_budget'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("the reading is committed with the claim");
    assert_eq!(reading["actor"], AGENT);
    assert_eq!(reading["step_id"], step_id.as_str());
    assert_eq!(reading["spent_usd_micros"], 1_000_870);
    assert_eq!(reading["budget_usd_micros"], 5_000_000);
    assert_eq!(reading["hourly_budget_usd_micros"], 3_000_000);
    assert!(
        reading["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("over the cap"),
        "{reading}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_that_fits_is_admitted_and_an_uncapped_row_reserves_nothing() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(3_000_000)).await;
    spend_a_dollar(&db).await;
    // 1,000,870 spent + 1,500,000 asked = 2,500,870 <= 3,000,000.
    let (app, _, step_id) = app_with_step(&db, 1.5).await;
    let body = claim(&app, &step_id, AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT);
    assert_eq!(body["status"], "active");

    // The seeded default — caps NULL — admits a reservation of any size.
    let uncapped = TestDb::new().await;
    spend_a_dollar(&uncapped).await;
    let (router, _, step_id) = app_with_step(&uncapped, 500.0).await;
    let body = claim(&router, &step_id, AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT);
}

/// THE MODEL HALF OF A STATION'S CAPABILITY. A `(role, model)` station
/// projected by `station_projection::agent_stations` admits an agent
/// that runs one of its models and refuses one that does not, naming
/// both lists; a person (no model) passes it and is gated by the roles
/// alone. The row is published through the registry from the
/// projection's own output, so what is tested is the shape the API
/// serves, not a copy of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_role_model_station_admits_the_agent_that_runs_the_model() {
    use boss_jobs::registry::WorkflowStatus;
    use boss_jobs::station_projection::agent_stations;

    let db = TestDb::new().await;
    let now = Utc::now();
    let actor = ActorId::Automation("test".into());
    // A protocol whose one step is a platform-admin haiku-4-5 build —
    // the seeded agent runs opus-5[1m], so it does not serve it.
    let mut wf = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| !w.steps.is_empty())
        .expect("a platform kind with steps");
    wf.status = WorkflowStatus::Active;
    wf.steps.truncate(1);
    wf.steps[0].kind = "task".into();
    wf.steps[0].authority_role = Some("platform-admin".into());
    wf.steps[0].audience = None;
    wf.steps[0].agent = Some(boss_jobs::agent_spec::AgentSpec {
        profile: "builder".into(),
        model: "haiku-4-5".into(),
        budget_usd: 1.0,
        effort: boss_jobs::agent_spec::Effort::Low,
    });
    let mut projected = agent_stations(std::slice::from_ref(&wf), &[], now);
    let station = projected.pop().expect("one (role, model) station");
    assert_eq!(station.name, "a.platform-admin.haiku-4-5");
    boss_jobs::PgStations::new(db.pool.clone())
        .publish_declared(station.clone(), &actor, now)
        .await
        .expect("the projected row publishes");

    let (app, _, step_id) = app_with_step_on(&db, 1.0, "haiku-4-5", true).await;
    let claim_from_station = |as_user: &'static str| {
        TestRequest::post(format!(
            "/api/jobs/{PACKET}/steps/{step_id}/claim?station={}",
            station.name
        ))
        .as_user(as_user, "platform-admin")
        .send(&app)
    };

    let resp = claim_from_station(AGENT).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(body["error"], "model not admitted by station capability");
    assert_eq!(
        body["models"],
        serde_json::json!(["opus-5[1m]"]),
        "the seeded default"
    );
    assert_eq!(body["allowed_models"], serde_json::json!(["haiku-4-5"]));

    // A person of the role runs no model: the roles gate decides, and
    // platform-admin is admitted.
    let resp = claim_from_station("emp-david").await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(body["assignee_id"], "emp-david");
}
