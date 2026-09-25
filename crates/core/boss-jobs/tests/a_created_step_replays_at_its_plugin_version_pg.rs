//! A step's STEP_CREATED carries the plugin version its row was stored
//! at, so a replay rebuilds the row it recorded (backlog aba364fe —
//! core correctness: determinism).
//!
//! Every write path built STEP_CREATED from the caller's Step, whose
//! `step_plugin_version` is 0, and the Pg step INSERT then stamped the
//! row with the version of the plugin active for its kind. The
//! rebuilder replays the payload, so a step created and never updated
//! rebuilt at 0 while the live row said 1 or 3 — the log and the
//! projection disagreed. Measured on the live record 2026-09-25: all
//! 2,672 plugin-served STEP_CREATED events since the log began carried
//! 0, and 144 of those steps (130 `answer-question` at v1, 14
//! `sign-off` at v3, every one pending on an open packet) had no
//! STEP_UPDATED to carry the stored value, so a rebuild would move
//! them. One test per path that writes a step and its event: the
//! admission handler (materialised steps), the add-step handler, and
//! the re-pin's inserted rows.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{JobsRepository, PgJobs, PgStepPlugins, StepPluginRegistry, StepPluginSpec};
use boss_policy_client::User;
use boss_policy_client::{AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The workflow the admission test files: a platform protocol with a
/// `task` step, which no migration serves with a plugin — so the
/// version this test publishes is the only one that could be stamped.
const WORKFLOW: &str = "backlog-item";
const SERVED: &str = "task";

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct StepRow {
    id: Uuid,
    kind: String,
    status: String,
    step_plugin_version: i32,
    metadata: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn step_rows(pool: &PgPool) -> Vec<StepRow> {
    sqlx::query_as::<_, StepRow>(
        "SELECT id, kind, status, step_plugin_version, metadata, created_at, updated_at \
         FROM steps ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `(step id, step_plugin_version)` of every STEP_CREATED on the outbox.
async fn created_versions(pool: &PgPool) -> Vec<(String, i64)> {
    let mut rows: Vec<(String, i64)> = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.step.created'",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(p,)| {
        (
            p["id"].as_str().unwrap_or_default().to_string(),
            p["step_plugin_version"].as_i64().unwrap_or(-1),
        )
    })
    .collect();
    rows.sort();
    rows
}

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

/// Publish two versions of a plugin for `kind`, so the stamp cannot
/// pass as a default 1. Returns the active version.
async fn serve(pool: &PgPool, kind: &str) -> i32 {
    let registry = PgStepPlugins::new(pool.clone());
    let mut version = 0;
    for _ in 0..2 {
        let spec = StepPluginSpec::draft(kind, "Served", "qa", "served.js", serde_json::json!({}));
        registry
            .create_draft(spec, &author(), Utc::now())
            .await
            .expect("draft");
        version = registry
            .publish(kind, &author(), Utc::now())
            .await
            .expect("publish")
            .version;
    }
    assert!(version >= 2, "two publishes, got v{version}");
    version
}

/// The platform protocol, published, with its filer fields turned to
/// executor fields — this test is about the stamp, not admission's
/// filer gate.
async fn publish_workflow(pool: &PgPool) {
    let mut wf = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == WORKFLOW)
        .expect("a platform workflow");
    assert!(
        wf.steps.iter().any(|s| s.kind == SERVED),
        "{WORKFLOW} has a {SERVED} step"
    );
    wf.status = WorkflowStatus::Draft;
    for step in &mut wf.steps {
        for field in &mut step.fields {
            field.filled_by = boss_core::job::FilledBy::Executor;
        }
    }
    let registry = PgWorkflows::new(pool.clone());
    registry
        .create_draft(wf, &author(), Utc::now())
        .await
        .expect("the protocol drafts");
    registry
        .publish(WORKFLOW, &author(), Utc::now())
        .await
        .expect("the protocol publishes");
}

/// The jobs router over `pool`. `with_workflows` plumbs the Workflow
/// registry, so admission materialises the protocol's steps; without
/// it any kind is admitted bare, and steps arrive only through the
/// add-step door.
fn build_app(pool: PgPool, with_workflows: bool) -> Router {
    let jobs = Arc::new(PgJobs::new(pool.clone()));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let kind_registry: Option<Arc<dyn WorkflowRegistry>> =
        with_workflows.then(|| Arc::new(PgWorkflows::new(pool)) as Arc<dyn WorkflowRegistry>);
    router(JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        kind_registry,
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

fn ceo() -> String {
    serde_json::to_string(&User {
        id: "emp-cto".into(),
        role: "ceo".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .unwrap()
}

async fn post(app: &Router, uri: &str, body: &serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("x-boss-user", ceo())
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        status.is_success(),
        "POST {uri} -> {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
}

fn job(id: &str, kind: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "a step replays at its plugin version".into(),
        owner_id: "emp-cto".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn step(id: &str, job_id: JobId, kind: &str, slug: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id,
        kind: kind.into(),
        title: slug.into(),
        spec_slug: Some(slug.into()),
        assignee_id: None,
        status: StepStatus::Pending,
        sort_order: 7,
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

/// Drain the outbox into audit_log, drop both projections, rebuild from
/// the log alone, and hand back the step rows the replay produced.
async fn replay(pool: &PgPool) -> Vec<StepRow> {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("relay drain");
    sqlx::query("DELETE FROM steps")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs").execute(pool).await.unwrap();
    boss_jobs::rebuild_jobs_and_steps(pool)
        .await
        .expect("rebuild succeeds");
    step_rows(pool).await
}

/// The event and the row agree for every step, the served ones are
/// stamped (so the agreement is not two zeros), and the replay
/// reproduces the rows.
async fn assert_the_log_is_the_rows(pool: &PgPool, served: i32) {
    let before = step_rows(pool).await;
    let stamped: Vec<&StepRow> = before.iter().filter(|r| r.kind == SERVED).collect();
    assert!(
        !stamped.is_empty(),
        "a {SERVED} step was written: {before:?}"
    );
    for row in &stamped {
        assert_eq!(
            row.step_plugin_version, served,
            "the row is stamped with the active version"
        );
    }
    let stored: Vec<(String, i64)> = before
        .iter()
        .map(|r| (r.id.to_string(), i64::from(r.step_plugin_version)))
        .collect();
    assert_eq!(
        created_versions(pool).await,
        stored,
        "each STEP_CREATED carries the version its row was stored at"
    );
    assert_eq!(
        replay(pool).await,
        before,
        "the rebuild reproduces the step rows"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_materialised_step_replays_at_the_version_it_was_stored_at() {
    let db = TestDb::new().await;
    publish_workflow(&db.pool).await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone(), true);

    let packet = job("aba364fe-0000-4000-8000-000000000001", WORKFLOW);
    post(&app, "/api/jobs", &serde_json::to_value(&packet).unwrap()).await;

    assert_the_log_is_the_rows(&db.pool, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_step_replays_at_the_version_it_was_stored_at() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone(), false);

    // No Workflow registry, so admission materialises nothing and the
    // one step arrives through the add-step door.
    let packet = job("aba364fe-0000-4000-8000-000000000002", "ad-hoc");
    post(&app, "/api/jobs", &serde_json::to_value(&packet).unwrap()).await;
    let added = step(
        "aba364fe-0000-4000-8000-0000000000a1",
        packet.id,
        SERVED,
        "added",
    );
    post(
        &app,
        &format!("/api/jobs/{}/steps", packet.id),
        &serde_json::to_value(&added).unwrap(),
    )
    .await;

    assert_the_log_is_the_rows(&db.pool, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repinned_packets_inserted_step_is_recorded_at_the_version_it_was_stored_at() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let repo = PgJobs::new(db.pool.clone());

    let packet = job("aba364fe-0000-4000-8000-000000000003", "ad-hoc");
    repo.create_job(&packet).await.unwrap();
    let inserted = step(
        "aba364fe-0000-4000-8000-0000000000b1",
        packet.id,
        SERVED,
        "inserted",
    );
    let plan = boss_jobs::repin::RepinPlan {
        reprojected: vec![],
        inserted: vec![inserted],
    };
    let stamp = EventStamp::new("jobs", author());
    let record = boss_jobs::repin::record(&plan, 1, 2, "emp-cto", stamp.timestamp);
    repo.repin_workflow_version_at(&packet.id, 2, &plan, &record, &stamp)
        .await
        .expect("the re-pin writes");

    let rows = step_rows(&db.pool).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].step_plugin_version, served);
    assert_eq!(
        created_versions(&db.pool).await,
        vec![(rows[0].id.to_string(), i64::from(served))],
        "the inserted row's STEP_CREATED carries the version it was stored at"
    );
}
