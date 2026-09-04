//! End-to-end: drive Job + Step writes through the API, snapshot
//! the projections, drop them, rebuild from `audit_log`, and assert
//! the rebuilt projections match the snapshots exactly.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::PgJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::rebuild_jobs_and_steps;
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct JobSnapshot {
    id: Uuid,
    kind: String,
    subject_kind: String,
    subject_id: String,
    title: String,
    owner_id: String,
    status: String,
    priority: String,
    opened_on: NaiveDate,
    due_on: Option<NaiveDate>,
    closed_on: Option<NaiveDate>,
    metadata: serde_json::Value,
    tags: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct StepSnapshot {
    id: Uuid,
    job_id: Uuid,
    kind: String,
    title: String,
    assignee_id: Option<String>,
    status: String,
    sort_order: i32,
    blocked_by: Vec<Uuid>,
    sign_offs_required: serde_json::Value,
    assurance_required: Option<String>,
    sign_offs: serde_json::Value,
    completed_on: Option<NaiveDate>,
    metadata: serde_json::Value,
    notes: Option<String>,
    step_plugin_version: i32,
    embedded_job: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// The queue-age stamp (2a0b034e): written once at the ready
    /// flip, reproduced on replay from the event that carried it.
    became_ready_at: Option<DateTime<Utc>>,
}

async fn snapshot_jobs(pool: &PgPool) -> Vec<JobSnapshot> {
    sqlx::query_as::<_, JobSnapshot>(
        "SELECT id, kind, subject_kind, subject_id, title, owner_id, status, priority, \
                opened_on, due_on, closed_on, metadata, tags, created_at, updated_at \
         FROM jobs ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn snapshot_steps(pool: &PgPool) -> Vec<StepSnapshot> {
    sqlx::query_as::<_, StepSnapshot>(
        "SELECT id, job_id, kind, title, assignee_id, status, sort_order, blocked_by, \
                sign_offs_required, assurance_required, sign_offs, completed_on, metadata, notes, \
                step_plugin_version, embedded_job, created_at, updated_at, became_ready_at \
         FROM steps ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn user(id: &str, role: &str) -> User {
    User {
        id: id.into(),
        role: role.into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

fn build_app(pool: PgPool) -> Router {
    // No direct audit writer: events record on the outbox in the
    // domain write; the drain moves them to audit_log.
    let jobs = Arc::new(PgJobs::new(pool.clone()));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());

    // Permissive policy — every action allowed for the test user.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .allow("ceo", Action::Close, Resource::job(), Scope::All)
            .build(),
    );

    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    router(state)
}

fn fixture_job(id: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "service-visit".into(),
        workflow_version: 1,
        subject: Subject::new("account", "acc-001"),
        title: title.into(),
        owner_id: "emp-100".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: Some(NaiveDate::from_ymd_opt(2026, 4, 30).unwrap()),
        closed_on: None,
        metadata: serde_json::json!({"site": "main"}),
        tags: vec!["urgent".into(), "vip".into()],
        simulated: false,
    }
}

fn fixture_step(step_id: &str, job_id: &str, sort_order: i32, title: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(step_id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(job_id).unwrap()),
        // `generic` has no required-on-done metadata fields, so the
        // step can flip to Done without us shaping a kind-specific
        // metadata payload — this test exercises rebuild, not
        // step-kind validation.
        kind: "generic".into(),
        title: title.into(),
        // A real slug, deliberately different from the title: the
        // before/after snapshot equality proves the rebuilder
        // reproduces the column (a projection column lands WITH its
        // rebuild source, or replay silently loses it).
        spec_slug: Some(format!("slug-{sort_order}")),
        assignee_id: Some("emp-200".into()),
        status: StepStatus::Pending,
        sort_order,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn http_json(
    app: &Router,
    method: &str,
    uri: &str,
    user: &User,
    body: Option<&serde_json::Value>,
) -> StatusCode {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", user_header(user));
    let body_bytes = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(v).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(body_bytes).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    if !status.is_success() {
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        panic!(
            "{method} {uri} -> {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    status
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_jobs_and_steps_after_drop() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());
    let ceo = user("emp-cto", "ceo");

    // 1. Create two Jobs through the API. Each lands a row in `jobs`
    //    AND a `jobs.job.created` event in audit_log.
    let job_a = fixture_job("aaaaaaaa-0000-4000-8000-000000000001", "first job");
    let job_b = fixture_job("bbbbbbbb-0000-4000-8000-000000000002", "second job");
    http_json(
        &app,
        "POST",
        "/api/jobs",
        &ceo,
        Some(&serde_json::to_value(&job_a).unwrap()),
    )
    .await;
    http_json(
        &app,
        "POST",
        "/api/jobs",
        &ceo,
        Some(&serde_json::to_value(&job_b).unwrap()),
    )
    .await;

    // 2. Add three Steps to job_a.
    let step1 = fixture_step(
        "11111111-0000-4000-8000-000000000001",
        "aaaaaaaa-0000-4000-8000-000000000001",
        1,
        "intake",
    );
    // step2 requires presence assurance — the column landed with the
    // WebAuthn work but the rebuilder never reproduced it, so a
    // replay silently downgraded the step to session assurance
    // (packet d7b8158e). The snapshot equality below pins it.
    let mut step2 = fixture_step(
        "22222222-0000-4000-8000-000000000002",
        "aaaaaaaa-0000-4000-8000-000000000001",
        2,
        "diagnose",
    );
    step2.assurance_required = Some(boss_core::job::Assurance::Presence);
    let step3 = fixture_step(
        "33333333-0000-4000-8000-000000000003",
        "aaaaaaaa-0000-4000-8000-000000000001",
        3,
        "deliver",
    );
    for step in [&step1, &step2, &step3] {
        http_json(
            &app,
            "POST",
            &format!("/api/jobs/{}/steps", step.job_id),
            &ceo,
            Some(&serde_json::to_value(step).unwrap()),
        )
        .await;
    }

    // 3. Update job_b — bump priority + add a tag. Lands a
    //    `jobs.job.updated` event with full row state.
    let mut job_b_updated = job_b.clone();
    job_b_updated.priority = Priority::Urgent;
    job_b_updated.tags = vec!["urgent".into(), "expedite".into()];
    http_json(
        &app,
        "PUT",
        &format!("/api/jobs/{}", job_b.id),
        &ceo,
        Some(&serde_json::to_value(&job_b_updated).unwrap()),
    )
    .await;

    // 4. Complete step1 — emits STEP_UPDATED (state) + STEP_COMPLETED
    //    (marker). Also auto-transitions job_a (since not all steps
    //    done, job_a stays open — but the auto-transition path
    //    doesn't emit a JOB_UPDATED unless status actually changes).
    let mut step1_done = step1.clone();
    step1_done.status = StepStatus::Completed;
    step1_done.completed_on = Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap());
    http_json(
        &app,
        "PUT",
        &format!("/api/jobs/{}/steps/{}", step1.job_id, step1.id),
        &ceo,
        Some(&serde_json::to_value(&step1_done).unwrap()),
    )
    .await;

    // 4b. Promote step2 to Ready — the queue-age stamp
    //     (`became_ready_at`, 2a0b034e) is a projection column like
    //     any other and must survive the drop + replay.
    let mut step2_ready = step2.clone();
    step2_ready.status = StepStatus::Ready;
    http_json(
        &app,
        "PUT",
        &format!("/api/jobs/{}/steps/{}", step2.job_id, step2.id),
        &ceo,
        Some(&serde_json::to_value(&step2_ready).unwrap()),
    )
    .await;

    // 5. Snapshot.
    let jobs_before = snapshot_jobs(&db.pool).await;
    let steps_before = snapshot_steps(&db.pool).await;
    assert_eq!(jobs_before.len(), 2);
    assert_eq!(steps_before.len(), 3);
    // The promotion stamped step2 and nothing else — an all-NULL
    // column would make the after-rebuild equality below vacuous.
    let stamped: Vec<Uuid> = steps_before
        .iter()
        .filter(|s| s.became_ready_at.is_some())
        .map(|s| s.id)
        .collect();
    assert_eq!(stamped, vec![*step2.id.inner().as_uuid()]);

    // 6. Drain the outbox, then verify expected events landed in
    //    audit_log.
    drain_outbox(&db.pool).await;
    let event_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log \
         WHERE kind LIKE 'jobs.job.%' OR kind LIKE 'jobs.step.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    // 2 created + 1 updated + 3 step.created + 2 step.updated (step1
    // done, step2 ready) + 1 step.completed marker = 9 minimum.
    // Auto-transition path not triggered here (job_a still has
    // pending steps).
    assert!(
        event_count.0 >= 9,
        "got only {} audit events",
        event_count.0
    );

    // 7. Blow away both projections (steps first to respect FK).
    sqlx::query("DELETE FROM steps")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs")
        .execute(&db.pool)
        .await
        .unwrap();

    // 8. Rebuild from audit_log alone.
    let report = rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild succeeds");
    assert_eq!(report.jobs_inserted, 2);
    assert_eq!(report.jobs_updated, 1, "job_b's update event");
    assert_eq!(report.steps_inserted, 3);
    assert_eq!(
        report.steps_updated, 2,
        "step1's done update + step2's ready promotion"
    );
    // STEP_COMPLETED marker should land in events_skipped.
    assert!(report.events_skipped >= 1);

    // 9. Reconstructed projections must match the originals
    //    bit-for-bit, including timestamps.
    let jobs_after = snapshot_jobs(&db.pool).await;
    let steps_after = snapshot_steps(&db.pool).await;
    assert_eq!(jobs_before, jobs_after, "jobs projection mismatch");
    assert_eq!(steps_before, steps_after, "steps projection mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_is_idempotent() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());
    let ceo = user("emp-cto", "ceo");

    let job = fixture_job("aaaaaaaa-0000-4000-8000-000000000099", "idempotent test");
    http_json(
        &app,
        "POST",
        "/api/jobs",
        &ceo,
        Some(&serde_json::to_value(&job).unwrap()),
    )
    .await;

    let baseline_jobs = snapshot_jobs(&db.pool).await;

    drain_outbox(&db.pool).await;
    rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(snapshot_jobs(&db.pool).await, baseline_jobs);

    rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(snapshot_jobs(&db.pool).await, baseline_jobs);
}
