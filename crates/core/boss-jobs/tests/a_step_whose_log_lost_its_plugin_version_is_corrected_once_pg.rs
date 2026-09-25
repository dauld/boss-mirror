//! The one-time repair door for steps whose STEP_CREATED said plugin
//! version 0 while the row was stamped (backlog 5a670a71 — core
//! correctness: determinism). Contract: `boss_jobs::plugin_version_repair`.
//!
//! Car 0f48e5f9 (backlog aba364fe) made every NEW STEP_CREATED carry
//! the stamped version; it could not reach the steps already written.
//! Measured 2026-09-25: 144 pending steps on open packets held only a
//! version-0 STEP_CREATED under a row at 1 or 3, so a rebuild moved
//! them. Each divergence here is made the way the old write path made
//! it — through the port, with a STEP_CREATED built from the caller's
//! version-0 Step while the INSERT stamps the row — and the door is
//! driven through the HTTP surface the CLI verb speaks to.
//!
//! What the tests hold: the dry run lists exactly the divergent step
//! and writes nothing; the write appends ONE STEP_UPDATED for it and
//! nothing else; a second write appends nothing (before the relay has
//! drained, so the outbox counts as the log's tail); a rebuild from the
//! log then reproduces every step row to the column; anything but that
//! exact divergence is refused, listed, and left alone; only a pending
//! step on an open packet is ever corrected, and each listed step
//! names its packet's status and partition; the dry run takes no row
//! lock; the scan skips a payload the rebuild skips (the three added
//! for the adversarial review of car 55ae8de4); and a caller without
//! the authority is refused before anything is read.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::plugin_version_repair::{Outcome, RepairReport};
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

/// A kind no migration serves with a plugin, so the version this test
/// publishes is the only one that could be stamped.
const SERVED: &str = "task";
const DOOR: &str = "/api/jobs/repairs/step-plugin-version";

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct StepRow {
    id: Uuid,
    job_id: Uuid,
    kind: String,
    title: String,
    status: String,
    sort_order: i32,
    step_plugin_version: i32,
    metadata: serde_json::Value,
    fields: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    became_ready_at: Option<DateTime<Utc>>,
}

async fn step_rows(pool: &PgPool) -> Vec<StepRow> {
    sqlx::query_as::<_, StepRow>(
        "SELECT id, job_id, kind, title, status, sort_order, step_plugin_version, metadata, \
                fields, created_at, updated_at, became_ready_at \
         FROM steps ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `(kind, step id, step_plugin_version)` of every step state event on
/// the outbox, in the order they were recorded.
async fn step_events(pool: &PgPool) -> Vec<(String, String, i64)> {
    sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'jobs.step.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(kind, p)| {
        (
            kind,
            p["id"].as_str().unwrap_or_default().to_string(),
            p["step_plugin_version"].as_i64().unwrap_or(-1),
        )
    })
    .collect()
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

/// The jobs router over `pool`. `admin` holds `publish` on
/// `step_plugin`, the door's authority; `viewer` holds only `read`.
fn build_app(pool: PgPool) -> Router {
    let jobs = Arc::new(PgJobs::new(pool));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("admin", Action::Create, Resource::job(), Scope::All)
            .allow("admin", Action::Read, Resource::job(), Scope::All)
            .allow("admin", Action::Update, Resource::job(), Scope::All)
            .allow("admin", Action::Update, Resource::step(), Scope::All)
            .allow(
                "admin",
                Action::Publish,
                Resource::step_plugin(),
                Scope::All,
            )
            .allow("viewer", Action::Read, Resource::step_plugin(), Scope::All)
            .build(),
    );
    router(JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

fn user(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-cto".into(),
        role: role.into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    role: &str,
    body: Option<&serde_json::Value>,
) -> (StatusCode, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-boss-user", user(role))
                .header("content-type", "application/json")
                .body(match body {
                    Some(b) => Body::from(serde_json::to_vec(b).unwrap()),
                    None => Body::empty(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// The door, as `admin`, read as the report it answers with.
async fn door(app: &Router, method: &str) -> RepairReport {
    let (status, bytes) = call(app, method, DOOR, "admin", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{method} {DOOR}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("the door answers a RepairReport")
}

fn job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "ad-hoc".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "a step whose log lost its plugin version".into(),
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

fn step(id: &str, job_id: JobId, slug: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id,
        kind: SERVED.into(),
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
        metadata: serde_json::json!({ "question": slug }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// Admit the packet through the handler, so its JOB_CREATED is in the
/// log and a rebuild has a job to hang the steps on.
async fn admit(app: &Router, packet: &Job) {
    let (status, bytes) = call(
        app,
        "POST",
        "/api/jobs",
        "admin",
        Some(&serde_json::to_value(packet).unwrap()),
    )
    .await;
    assert!(
        status.is_success(),
        "admit -> {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
}

/// Write `step` the way every path wrote one before car 0f48e5f9: the
/// row stamped by the INSERT, the recorded event built from the
/// caller's Step — `event_step`, which is `step` itself unless a test
/// wants the log to differ from the row in some other way too.
/// `event_kind` `None` records no event at all.
async fn write_the_old_way(
    repo: &PgJobs,
    step: &Step,
    event_kind: Option<&str>,
    event_step: &Step,
) {
    let stamp = EventStamp::new("jobs", author());
    let events: Vec<_> = event_kind
        .map(|kind| stamp.event(kind, boss_jobs::events::step_state_payload(event_step)))
        .into_iter()
        .collect();
    repo.add_step_at(step, stamp.timestamp, &events)
        .await
        .expect("the step writes");
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

#[tokio::test(flavor = "multi_thread")]
async fn the_divergent_step_is_corrected_once_and_the_rebuild_reproduces_every_row() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());

    let packet = job("5a670a71-0000-4000-8000-000000000001");
    admit(&app, &packet).await;
    // The divergence: the row stamped at `served`, its only event at 0.
    let lost = step("5a670a71-0000-4000-8000-0000000000a1", packet.id, "lost");
    write_the_old_way(&repo, &lost, Some(boss_jobs::events::STEP_CREATED), &lost).await;
    // A step written the fixed way, whose event already agrees.
    let kept = step("5a670a71-0000-4000-8000-0000000000a2", packet.id, "kept");
    let (status, bytes) = call(
        &app,
        "POST",
        &format!("/api/jobs/{}/steps", packet.id),
        "admin",
        Some(&serde_json::to_value(&kept).unwrap()),
    )
    .await;
    assert!(
        status.is_success(),
        "add-step -> {status}: {}",
        String::from_utf8_lossy(&bytes)
    );

    let before = step_rows(&db.pool).await;
    assert_eq!(before.len(), 2, "{before:?}");
    assert!(
        before.iter().all(|r| r.step_plugin_version == served),
        "both rows are stamped: {before:?}"
    );
    let events_before = step_events(&db.pool).await;
    assert_eq!(
        events_before
            .iter()
            .filter(|(_, id, v)| id == &lost.id.to_string() && *v == 0)
            .count(),
        1,
        "the divergence is in the log: {events_before:?}"
    );

    // The dry run names exactly the divergent step and writes nothing.
    let dry = door(&app, "GET").await;
    assert!(!dry.written, "{dry:?}");
    assert_eq!(dry.steps.len(), 1, "only the divergent step: {dry:?}");
    let listed = &dry.steps[0];
    assert_eq!(listed.step_id, lost.id.to_string());
    assert_eq!(listed.job_id, packet.id.to_string());
    assert_eq!(listed.outcome, Outcome::WouldCorrect, "{listed:?}");
    assert_eq!(listed.logged_version, Some(0));
    assert_eq!(listed.stored_version, served);
    assert_eq!(
        listed.logged_kind.as_deref(),
        Some(boss_jobs::events::STEP_CREATED)
    );
    assert_eq!(
        step_events(&db.pool).await,
        events_before,
        "a dry run writes nothing"
    );
    assert_eq!(
        step_rows(&db.pool).await,
        before,
        "a dry run touches no row"
    );

    // The write: ONE STEP_UPDATED, from the row, for that step alone.
    let run = door(&app, "POST").await;
    assert!(run.written, "{run:?}");
    assert_eq!(run.count(&Outcome::Corrected), 1, "{run:?}");
    assert_eq!(run.steps.len(), 1, "{run:?}");
    let after_write = step_events(&db.pool).await;
    let appended: Vec<_> = after_write[events_before.len()..].to_vec();
    assert_eq!(
        appended,
        vec![(
            boss_jobs::events::STEP_UPDATED.to_string(),
            lost.id.to_string(),
            i64::from(served)
        )],
        "exactly one correcting STEP_UPDATED, carrying the stored version"
    );
    let actor: String = sqlx::query_scalar(
        "SELECT payload->>'_actor' FROM event_outbox WHERE kind = 'jobs.step.updated' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(actor, "emp-cto", "the correction is signed as its caller");

    // A second write — the relay has NOT drained, so the correction is
    // still only on the outbox — appends nothing.
    let again = door(&app, "POST").await;
    assert!(again.steps.is_empty(), "idempotent: {again:?}");
    assert_eq!(
        step_events(&db.pool).await,
        after_write,
        "a second run writes nothing"
    );

    // The log now rebuilds the rows it recorded, to the column.
    let live = step_rows(&db.pool).await;
    assert_eq!(
        replay(&db.pool).await,
        live,
        "the rebuild reproduces every step row"
    );
    let settled = door(&app, "GET").await;
    assert!(
        settled.steps.is_empty(),
        "nothing left to find: {settled:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anything_but_that_exact_divergence_is_refused_and_left_alone() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());
    let packet = job("5a670a71-0000-4000-8000-000000000002");
    admit(&app, &packet).await;

    // The log differs in the title as well as the version.
    let drifted = step("5a670a71-0000-4000-8000-0000000000b1", packet.id, "drifted");
    let mut elsewhere = drifted.clone();
    elsewhere.title = "a title the row never held".into();
    write_the_old_way(
        &repo,
        &drifted,
        Some(boss_jobs::events::STEP_CREATED),
        &elsewhere,
    )
    .await;
    // The last state is a STEP_UPDATED at 0 — not the defect's event.
    let updated = step("5a670a71-0000-4000-8000-0000000000b2", packet.id, "updated");
    write_the_old_way(
        &repo,
        &updated,
        Some(boss_jobs::events::STEP_UPDATED),
        &updated,
    )
    .await;
    // The log holds no state event for the step at all.
    let silent = step("5a670a71-0000-4000-8000-0000000000b3", packet.id, "silent");
    write_the_old_way(&repo, &silent, None, &silent).await;

    let rows = step_rows(&db.pool).await;
    assert!(
        rows.iter().all(|r| r.step_plugin_version == served),
        "{rows:?}"
    );
    let events_before = step_events(&db.pool).await;

    for method in ["GET", "POST"] {
        let report = door(&app, method).await;
        assert_eq!(report.steps.len(), 3, "{method}: {report:?}");
        assert_eq!(
            report.count(&Outcome::Refused),
            3,
            "{method}: every one refused: {report:?}"
        );
        let reason = |id: StepId| {
            report
                .steps
                .iter()
                .find(|s| s.step_id == id.to_string())
                .and_then(|s| s.reason.clone())
                .unwrap_or_default()
        };
        assert!(
            reason(drifted.id).contains("title"),
            "{}",
            reason(drifted.id)
        );
        assert!(
            reason(updated.id).contains(boss_jobs::events::STEP_UPDATED),
            "{}",
            reason(updated.id)
        );
        assert!(
            reason(silent.id).contains("no state event"),
            "{}",
            reason(silent.id)
        );
    }
    assert_eq!(
        step_events(&db.pool).await,
        events_before,
        "a refusal writes nothing"
    );
    assert_eq!(step_rows(&db.pool).await, rows, "a refusal touches no row");
}

/// The adversarial review's scenario (car 55ae8de4): a READY step on a
/// CLOSED packet whose log lost its version. A STEP_UPDATED for it
/// re-fires the dispatcher's nomination — `handle_event` does not read
/// the packet's status — so work would be offered on a packet that has
/// ended. Only a PENDING step on an OPEN packet is corrected; every
/// other one is refused with the reason, and nothing is written. Each
/// listed step carries its packet's status and partition, so the dry
/// run the operator reads before the write says which packets it
/// touches.
#[tokio::test(flavor = "multi_thread")]
async fn only_a_pending_step_on_an_open_packet_is_corrected() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());

    let open = job("5a670a71-0000-4000-8000-000000000004");
    admit(&app, &open).await;
    let mut closed = job("5a670a71-0000-4000-8000-000000000005");
    admit(&app, &closed).await;

    // On the open packet: one pending step (the defect, corrected) and
    // one ready step (refused: the event would re-nominate it).
    let pending = step("5a670a71-0000-4000-8000-0000000000d1", open.id, "pending");
    write_the_old_way(
        &repo,
        &pending,
        Some(boss_jobs::events::STEP_CREATED),
        &pending,
    )
    .await;
    let mut ready = step("5a670a71-0000-4000-8000-0000000000d2", open.id, "ready");
    ready.status = StepStatus::Ready;
    write_the_old_way(&repo, &ready, Some(boss_jobs::events::STEP_CREATED), &ready).await;
    // On the packet that then closes: the reviewer's ready step, and a
    // pending one — both refused, because the packet has ended.
    let mut ended_ready = step(
        "5a670a71-0000-4000-8000-0000000000d3",
        closed.id,
        "ended-ready",
    );
    ended_ready.status = StepStatus::Ready;
    write_the_old_way(
        &repo,
        &ended_ready,
        Some(boss_jobs::events::STEP_CREATED),
        &ended_ready,
    )
    .await;
    let ended_pending = step(
        "5a670a71-0000-4000-8000-0000000000d4",
        closed.id,
        "ended-pending",
    );
    write_the_old_way(
        &repo,
        &ended_pending,
        Some(boss_jobs::events::STEP_CREATED),
        &ended_pending,
    )
    .await;
    closed.status = JobStatus::Closed;
    closed.closed_on = NaiveDate::from_ymd_opt(2026, 9, 25);
    repo.update_job(&closed).await.expect("the packet closes");

    let rows = step_rows(&db.pool).await;
    assert!(
        rows.iter().all(|r| r.step_plugin_version == served),
        "{rows:?}"
    );

    let dry = door(&app, "GET").await;
    assert_eq!(dry.steps.len(), 4, "{dry:?}");
    let listed = |report: &RepairReport, id: StepId| {
        report
            .steps
            .iter()
            .find(|s| s.step_id == id.to_string())
            .cloned()
            .unwrap_or_else(|| panic!("step {id} is listed: {report:?}"))
    };
    assert_eq!(listed(&dry, pending.id).outcome, Outcome::WouldCorrect);
    for (id, packet_status, why) in [
        (ready.id, "open", "ready"),
        (ended_ready.id, "closed", "closed"),
        (ended_pending.id, "closed", "closed"),
    ] {
        let s = listed(&dry, id);
        assert_eq!(s.outcome, Outcome::Refused, "{s:?}");
        let reason = s.reason.clone().unwrap_or_default();
        assert!(reason.contains(why), "{id}: {reason}");
        assert_eq!(s.packet_status, packet_status, "{s:?}");
        assert_eq!(s.partition, "real", "{s:?}");
    }
    assert_eq!(listed(&dry, pending.id).packet_status, "open");

    let events_before = step_events(&db.pool).await;
    let run = door(&app, "POST").await;
    assert_eq!(run.count(&Outcome::Corrected), 1, "{run:?}");
    assert_eq!(run.count(&Outcome::Refused), 3, "{run:?}");
    let appended: Vec<_> = step_events(&db.pool).await[events_before.len()..].to_vec();
    assert_eq!(
        appended,
        vec![(
            boss_jobs::events::STEP_UPDATED.to_string(),
            pending.id.to_string(),
            i64::from(served)
        )],
        "the pending step on the open packet alone is corrected"
    );
}

/// The dry run is a READ: it must not take the row locks the write
/// takes, so it neither waits on a step writer nor holds one up. A
/// transaction holding one step's row lock is left open while the dry
/// run runs; the dry run answers anyway.
#[tokio::test(flavor = "multi_thread")]
async fn the_dry_run_takes_no_row_lock() {
    let db = TestDb::new().await;
    serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());
    let packet = job("5a670a71-0000-4000-8000-000000000006");
    admit(&app, &packet).await;
    let lost = step("5a670a71-0000-4000-8000-0000000000e1", packet.id, "lost");
    write_the_old_way(&repo, &lost, Some(boss_jobs::events::STEP_CREATED), &lost).await;

    let mut writer = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM steps WHERE id = $1 FOR UPDATE")
        .bind(*lost.id.inner().as_uuid())
        .execute(&mut *writer)
        .await
        .unwrap();
    let dry = tokio::time::timeout(std::time::Duration::from_secs(10), door(&app, "GET"))
        .await
        .expect("the dry run answers while a writer holds the step's row lock");
    assert_eq!(dry.count(&Outcome::WouldCorrect), 1, "{dry:?}");
    writer.rollback().await.unwrap();
}

/// A state event that does not deserialize as a Step is skipped by the
/// rebuild, so the scan must skip it too: here the last state event is
/// a malformed STEP_UPDATED (no title) that happens to carry the row's
/// version, and the event the rebuild applies last is the version-0
/// STEP_CREATED. A scan that read the malformed payload's version would
/// see agreement and miss the step.
#[tokio::test(flavor = "multi_thread")]
async fn the_scan_skips_a_payload_the_rebuild_skips() {
    let db = TestDb::new().await;
    let served = serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());
    let packet = job("5a670a71-0000-4000-8000-000000000007");
    admit(&app, &packet).await;
    let lost = step("5a670a71-0000-4000-8000-0000000000f1", packet.id, "lost");
    write_the_old_way(&repo, &lost, Some(boss_jobs::events::STEP_CREATED), &lost).await;
    let malformed = EventStamp::new("jobs", author()).event(
        boss_jobs::events::STEP_UPDATED,
        serde_json::json!({ "id": lost.id.to_string(), "step_plugin_version": served }),
    );
    repo.record_events(&[malformed]).await.expect("recorded");

    let dry = door(&app, "GET").await;
    assert_eq!(dry.steps.len(), 1, "the step is found: {dry:?}");
    assert_eq!(dry.steps[0].outcome, Outcome::WouldCorrect, "{dry:?}");
    assert_eq!(dry.steps[0].logged_version, Some(0), "{dry:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_without_publish_on_step_plugins_is_refused_before_anything_is_written() {
    let db = TestDb::new().await;
    serve(&db.pool, SERVED).await;
    let app = build_app(db.pool.clone());
    let repo = PgJobs::new(db.pool.clone());
    let packet = job("5a670a71-0000-4000-8000-000000000003");
    admit(&app, &packet).await;
    let lost = step("5a670a71-0000-4000-8000-0000000000c1", packet.id, "lost");
    write_the_old_way(&repo, &lost, Some(boss_jobs::events::STEP_CREATED), &lost).await;
    let events_before = step_events(&db.pool).await;

    for method in ["GET", "POST"] {
        let (status, _) = call(&app, method, DOOR, "viewer", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} as a reader");
    }
    assert_eq!(step_events(&db.pool).await, events_before);
}
