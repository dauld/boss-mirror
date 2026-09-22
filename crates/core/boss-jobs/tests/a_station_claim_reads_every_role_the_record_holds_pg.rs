//! The claim door judges an agent over EVERY role the record holds for
//! it — the request's platform role AND its agents row's own (backlog
//! 4b103f0f).
//!
//! TWO VOCABULARIES, ONE DECISION. A step's role selector is a Class
//! code: `audience = { role = X }` resolves to every HOLDER of X,
//! agents included (migration 20260918022311), and the roster the
//! dispatcher matches against reads `agents.role` for exactly that.
//! `station_projection::agent_stations` copies that same code into the
//! projected station's `capability.roles`. The request's `user.role`
//! is a PLATFORM role, and every named CLI caller carries
//! `platform-admin` (`boss-cli` `identity::header`).
//!
//! So until this car the two halves of one routing decision read two
//! different vocabularies: the roster nominated `agent-claude`
//! (`role = engineering-agent`) for an `engineering-agent` step, and
//! the claim door then refused it — the only role it compared was the
//! one the request asserted about itself. The live row's `role` was
//! read by nothing at the claim, which is the half of 4b103f0f that
//! the concurrency bound (57c108c2) left open.
//!
//! Contracts:
//! 1. A station whose capability names the role the agents ROW holds
//!    admits that agent, although the request's platform role is not
//!    in the list.
//! 2. A station admitting neither spelling still refuses, and the
//!    refusal names every role the door compared.
//! 3. A person — no agents row, so one spelling — is judged exactly as
//!    before.

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
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_projection::agent_stations;
use boss_jobs::{JobsRepository, PgJobs, StationRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

const AGENT: &str = "agent-claude";
const MODEL: &str = "opus-5";
const PACKET: &str = "cccccccc-0000-4000-8000-000000000001";

fn packet() -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "A station claim reads the agents own role".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A ready `task` step addressed to `role`, carrying the projection of
/// an agent block on [`MODEL`] — the two keys an `a.<role>.<model>`
/// station's predicate matches on.
fn step_for(role: &str) -> Step {
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
            "authority_role": role,
            "agent_profile": "builder",
            "agent_model": MODEL,
            "agent_effort": "high",
        }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// The live row as the real tenant's `agents.toml` declares it: a
/// Class code for the role, a Class code for the department (backlog
/// ab192a9f). `role: None` is the "holds no role" case.
async fn declare_agent(db: &TestDb, role: Option<&str>) {
    PgAgents::new(db.pool.clone())
        .publish(
            &[AgentInput {
                id: AGENT.into(),
                display_name: "Claude (engineering)".into(),
                default_model: MODEL.into(),
                aliases: vec!["claude@algedonic.dev".into()],
                role: role.map(str::to_string),
                department: Some("engineering".into()),
                hourly_budget_usd_micros: None,
                max_concurrent_runs: None,
            }],
            PublishMode::Take,
            &EventStamp::new("jobs", ActorId::Automation("test".into())),
        )
        .await
        .expect("the row takes its role");
}

/// The `a.<role>.<model-slug>` station the protocol set PROJECTS for a
/// step addressed to `role` — published from the projection's own
/// output, so what is tested is the row the API serves rather than a
/// copy of it.
async fn project_station(db: &TestDb, role: &str) -> String {
    let mut wf = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| !w.steps.is_empty())
        .expect("a platform kind with steps");
    wf.status = WorkflowStatus::Active;
    wf.steps.truncate(1);
    wf.steps[0].kind = "task".into();
    wf.steps[0].authority_role = Some(role.to_string());
    wf.steps[0].audience = None;
    wf.steps[0].agent = Some(boss_jobs::agent_spec::AgentSpec {
        profile: "builder".into(),
        model: MODEL.into(),
        budget_usd: 1.0,
        effort: boss_jobs::agent_spec::Effort::Low,
    });
    let now = Utc::now();
    let mut projected = agent_stations(std::slice::from_ref(&wf), &[], now);
    let station = projected.pop().expect("one (role, model) station");
    boss_jobs::PgStations::new(db.pool.clone())
        .publish_declared(station.clone(), &ActorId::Automation("test".into()), now)
        .await
        .expect("the projected row publishes");
    station.name
}

/// The jobs API with the Pg station registry and the agents door wired
/// — the same shape `boss dispatch` claims through.
async fn app_with(db: &TestDb, step: Step) -> (axum::Router, String) {
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    jobs.create_job(&packet()).await.expect("packet lands");
    let id = step.id.to_string();
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
        stations: Some(
            Arc::new(boss_jobs::PgStations::new(db.pool.clone())) as Arc<dyn StationRegistry>
        ),
        agent_budget: Some(Arc::new(BudgetDoor { agents, runs })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    (app, id)
}

async fn claim_at(
    app: &axum::Router,
    step_id: &str,
    station: &str,
    as_user: &str,
    expect: StatusCode,
) -> serde_json::Value {
    let resp = TestRequest::post(format!(
        "/api/jobs/{PACKET}/steps/{step_id}/claim?station={station}"
    ))
    .as_user(as_user, "platform-admin")
    .send(app)
    .await;
    resp.assert_status(expect);
    resp.assert_json()
}

/// Contract 1. The station speaks the ORG vocabulary — the one the
/// roster routed on — and the claimant's platform role is not in it.
/// The agents row is what reconciles them.
#[tokio::test(flavor = "multi_thread")]
async fn a_station_in_the_org_vocabulary_admits_the_agent_that_holds_the_role() {
    let db = TestDb::new().await;
    declare_agent(&db, Some("engineering-agent")).await;
    let station = project_station(&db, "engineering-agent").await;
    assert_eq!(station, "a.engineering-agent.opus-5");
    let (app, step_id) = app_with(&db, step_for("engineering-agent")).await;

    let body = claim_at(&app, &step_id, &station, AGENT, StatusCode::OK).await;
    assert_eq!(body["assignee_id"], AGENT);
    assert_eq!(body["status"], "active");
}

/// Contract 2. Neither spelling is admitted, so the claim is refused —
/// and the body names every role compared, not only the asserted one.
/// A verdict an operator has to go re-derive is not a verdict.
#[tokio::test(flavor = "multi_thread")]
async fn a_station_admitting_neither_spelling_refuses_and_names_both() {
    let db = TestDb::new().await;
    declare_agent(&db, Some("engineering-agent")).await;
    let station = project_station(&db, "bookkeeper").await;
    let (app, step_id) = app_with(&db, step_for("bookkeeper")).await;

    let body = claim_at(&app, &step_id, &station, AGENT, StatusCode::FORBIDDEN).await;
    assert_eq!(body["error"], "role not admitted by station capability");
    assert_eq!(body["role"], "platform-admin");
    assert_eq!(
        body["actor_roles"],
        serde_json::json!(["platform-admin", "engineering-agent"]),
        "every spelling the door compared"
    );
    assert_eq!(body["allowed_roles"], serde_json::json!(["bookkeeper"]));
}

/// Contract 3. A row that holds NO role adds no spelling, and a person
/// has no row at all: both are judged on the request's platform role
/// exactly as before this car.
#[tokio::test(flavor = "multi_thread")]
async fn a_roleless_row_and_a_person_are_judged_on_the_request_alone() {
    let db = TestDb::new().await;
    declare_agent(&db, None).await;
    let station = project_station(&db, "engineering-agent").await;
    let (app, step_id) = app_with(&db, step_for("engineering-agent")).await;

    let body = claim_at(&app, &step_id, &station, AGENT, StatusCode::FORBIDDEN).await;
    assert_eq!(
        body["actor_roles"],
        serde_json::json!(["platform-admin"]),
        "a row holding no role contributes no second spelling"
    );

    // A person: no agents row, so the platform role is the only one
    // there has ever been — and this station does not admit it.
    let body = claim_at(&app, &step_id, &station, "emp-david", StatusCode::FORBIDDEN).await;
    assert_eq!(body["actor_roles"], serde_json::json!(["platform-admin"]));
}
