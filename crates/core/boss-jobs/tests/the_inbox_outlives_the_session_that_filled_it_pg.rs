//! THE DURABLE INBOX (design 8382bbb2, backlog 923b6571).
//!
//! Dispatch was synchronous from a session: `boss dispatch` printed a
//! prompt and the session held the thread, so on 2026-09-19 fifteen
//! `agent-run` packets were orphaned when the dispatching session died
//! at ~15:39Z. The decided shape is a STATION plus the existing
//! `agent-run` kind — not a revived coordinator — because a station is
//! already this system's word for a queue that holds work until there
//! is capability and bandwidth, and because one budget gate
//! (`agent_budget`, reserving pre-CAS against the actor's hour) must
//! not become two implementations of the same rule.
//!
//! The queue was already there. `station_projection::agent_stations`
//! projects one `a.<role>.<model-slug>` station per `(role, model)` the
//! active protocols declare, and `GET /api/stations/{name}/queue`
//! answers for it — measured on the live instance 2026-09-19,
//! `a.platform-admin.opus-5-1m` held 24 packets. What was missing is
//! the door out of it: the claim path resolved the station it was
//! handed with `get_active` ALONE, so a claim naming a DERIVED station
//! answered 404 and no agent could ever claim from its own inbox. This
//! test is that door, and the property the inbox exists for:
//!
//! 1. A different process, sharing nothing with the one that filed the
//!    work but the database, finds it at the station and claims it —
//!    the record alone carries enough to pick the work up.
//! 2. That claim goes through the ONE budget door: over the actor's
//!    hourly cap it is refused with the numbers, and the refused work
//!    is still IN the queue afterwards. Stations hold; they do not drop.

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
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus};
use boss_jobs::{JobsRepository, PgJobs, PgStations, StationRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{NaiveDate, Utc};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// The seeded agent (migration 20260915212644): `opus-5[1m]`, no cap.
const AGENT: &str = "agent-claude";
const MODEL: &str = "opus-5[1m]";
const ROLE: &str = "platform-admin";
/// What `agent_stations` names the `(platform-admin, opus-5[1m])`
/// queue — the inbox this test claims from. Spelled out rather than
/// derived, because a rename that silently moved every agent's inbox
/// should red here.
const INBOX: &str = "a.platform-admin.opus-5-1m";
const PACKET: &str = "bbbbbbbb-1111-4000-8000-000000000001";

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(PACKET).expect("a fixed uuid")),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "The durable inbox as a station".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 19).expect("a fixed date"),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A ready `build` step carrying the agent block's projection — the
/// four keys materialisation writes, plus the `authority_role` the
/// station predicate matches on. Unassigned: this is work WAITING,
/// which is the only kind an inbox holds.
fn build_step(budget_usd: f64) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        job_id: JobId::from_uuid(Uuid::parse_str(PACKET).expect("a fixed uuid")),
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
            "authority_role": ROLE,
            "agent_profile": "builder",
            "agent_model": MODEL,
            "agent_budget_usd": budget_usd,
            "agent_effort": "high",
        }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// The protocol that DECLARES the inbox: one active Workflow whose
/// step carries an `agent` block under `platform-admin` on `opus-5[1m]`.
/// Nothing authors the station — it is projected from this row, which
/// is the whole reason the claim door has to resolve it the way the
/// queue read does.
async fn declare_the_protocol(db: &TestDb) {
    let mut wf = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| !w.steps.is_empty())
        .expect("a platform kind with steps");
    // The whole protocol, with a block added to its first step: a
    // truncated one is unviable (no terminal) and the publish lint
    // refuses it, which is the registry doing its job.
    wf.status = WorkflowStatus::Draft;
    wf.steps[0].authority_role = Some(ROLE.into());
    wf.steps[0].audience = None;
    wf.steps[0].agent = Some(boss_jobs::agent_spec::AgentSpec {
        profile: "builder".into(),
        model: MODEL.into(),
        budget_usd: 5.0,
        effort: boss_jobs::agent_spec::Effort::High,
    });
    let kind = wf.kind.clone();
    let actor = ActorId::Automation("test".into());
    let now = Utc::now();
    let registry = PgWorkflows::new(db.pool.clone());
    registry
        .create_draft(wf, &actor, now)
        .await
        .expect("the protocol drafts");
    registry
        .publish(&kind, &actor, now)
        .await
        .expect("the protocol publishes");
}

/// The seeded agent row, re-declared with a cap (take mode: the
/// instance is the truth, and this test IS the operator's edit).
async fn cap_the_agent(db: &TestDb, hourly_budget_usd_micros: i64) {
    PgAgents::new(db.pool.clone())
        .publish(
            &[AgentInput {
                id: AGENT.into(),
                display_name: "Claude (engineering)".into(),
                default_model: MODEL.into(),
                aliases: vec!["claude@algedonic.dev".into()],
                role: Some(ROLE.into()),
                department: None,
                hourly_budget_usd_micros: Some(hourly_budget_usd_micros),
                max_concurrent_runs: None,
            }],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the row takes the cap");
}

/// One priced run in the hour: 128,684 in + 14,298 out on the seeded
/// rate card = 1,000,870 usd_micros, finished a minute ago.
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

/// A whole jobs API over its OWN connection pool. Two of these share
/// nothing but the database — which is the point: the second one is
/// what a restarted service, a later session or a runner on another
/// host is.
async fn an_api(url: &str) -> axum::Router {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .expect("its own pool");
    let jobs = Arc::new(PgJobs::new(pool.clone()));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(ROLE, Action::Update, Resource::step(), Scope::All)
            .allow(ROLE, Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let agents: Arc<dyn AgentsRegistry> = Arc::new(PgAgents::new(pool.clone()));
    let runs: Arc<dyn AgentRunLog> = Arc::new(PgAgentRuns::new(pool.clone()));
    router(JobsApiState {
        kind_registry: Some(Arc::new(PgWorkflows::new(pool.clone())) as Arc<dyn WorkflowRegistry>),
        stations: Some(Arc::new(PgStations::new(pool.clone())) as Arc<dyn StationRegistry>),
        agent_budget: Some(Arc::new(BudgetDoor { agents, runs })),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

/// The work, filed by whatever is about to die.
async fn file_the_work(db: &TestDb, budget_usd: f64) -> StepId {
    let jobs = PgJobs::new(db.pool.clone());
    jobs.create_job(&packet()).await.expect("packet lands");
    let step = build_step(budget_usd);
    jobs.add_step(&step).await.expect("step lands");
    step.id
}

/// `GET /api/stations/{INBOX}/queue`, as the agent.
async fn inbox(app: &axum::Router) -> serde_json::Value {
    let resp = TestRequest::get(format!("/api/stations/{INBOX}/queue"))
        .as_user(AGENT, ROLE)
        .send(app)
        .await;
    resp.assert_status(StatusCode::OK);
    resp.assert_json()
}

/// THE PROPERTY THE INBOX EXISTS FOR. Nothing is handed between the
/// process that filed the work and the one that takes it: the first is
/// gone before the second is built, and the only thing they share is
/// the record.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_process_claims_work_the_first_one_only_left_in_the_record() {
    let db = TestDb::new().await;
    declare_the_protocol(&db).await;
    cap_the_agent(&db, 10_000_000).await;
    let step_id = file_the_work(&db, 5.0).await;

    // The session that filed it dies — its router, its pool, its
    // registries.
    drop(an_api(&db.url()).await);

    // A different process. It was told nothing; it asks the record.
    let app = an_api(&db.url()).await;
    let queue = inbox(&app).await;
    assert_eq!(
        queue["total"], 1,
        "the inbox holds the waiting work: {queue}"
    );
    assert_eq!(queue["data"][0]["id"], PACKET);
    assert_eq!(queue["station"], INBOX);

    // And the queue is a door, not a display: the claim names the
    // station the queue named, and the packet it named.
    let resp = TestRequest::post(format!(
        "/api/jobs/{PACKET}/steps/{step_id}/claim?station={INBOX}"
    ))
    .as_user(AGENT, ROLE)
    .send(&app)
    .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(body["assignee_id"], AGENT);
    assert_eq!(body["status"], "active");
}

/// THE ONE BUDGET DOOR. The inbox claims through `agent_budget` like
/// every other claim — there is no second rule for queued work — and a
/// refusal leaves the work IN the queue, because stations hold rather
/// than drop.
#[tokio::test(flavor = "multi_thread")]
async fn the_inbox_claims_through_the_one_budget_door_and_a_refusal_leaves_the_work_queued() {
    let db = TestDb::new().await;
    declare_the_protocol(&db).await;
    cap_the_agent(&db, 3_000_000).await;
    spend_a_dollar(&db).await;
    let step_id = file_the_work(&db, 5.0).await;

    let app = an_api(&db.url()).await;
    let resp = TestRequest::post(format!(
        "/api/jobs/{PACKET}/steps/{step_id}/claim?station={INBOX}"
    ))
    .as_user(AGENT, ROLE)
    .send(&app)
    .await;
    resp.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(body["spent_usd_micros"], 1_000_870);
    assert_eq!(body["budget_usd_micros"], 5_000_000);
    assert_eq!(body["hourly_budget_usd_micros"], 3_000_000);

    // Still queued, still ready, still nobody's: the hour will open.
    let queue = inbox(&app).await;
    assert_eq!(
        queue["total"], 1,
        "a refused claim does not drop it: {queue}"
    );
    let step = PgJobs::new(db.pool.clone())
        .get_step(&step_id)
        .await
        .expect("the step reads")
        .expect("the step is there");
    assert_eq!(step.status, StepStatus::Ready);
    assert_eq!(step.assignee_id, None);
}
