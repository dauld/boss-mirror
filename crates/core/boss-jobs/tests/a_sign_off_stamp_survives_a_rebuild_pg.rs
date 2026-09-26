//! A SIGN-OFF STAMP SURVIVES A PROJECTION REBUILD (backlog f146a13a).
//!
//! The sign-off door recorded only the `jobs.step.signed_off` marker,
//! and the rebuild skips markers, so a live stamp no later edit or
//! completion carried was dropped by a replay: the log did not
//! reproduce the step (determinism, the correctness protocol). Triage
//! reproduced it with the first test below, in the shape the door
//! writes: `append_sign_off` with the door's own marker payload, the
//! outbox relayed into `audit_log` as production does, then the rebuild.
//!
//! The append now records the step's full row beside the marker, so
//! the log carries the stamp. The markers already written carry no
//! `stamped_at` (the door never wrote one, from the initial commit on),
//! so a stamp only a marker recorded cannot be rebuilt exactly — the
//! rebuild counts it in `sign_offs_unreproduced` and invents nothing.

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, SignOffStamp, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::EventStamp;
use boss_jobs::port::JobsRepository;
use boss_jobs::{PgJobs, events, rebuild_jobs_and_steps};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const ROLE: &str = "platform-admin";

fn job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "ops-request".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "forge"),
        title: "An approval".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn approve_step(job: &Job, decision: &str) -> Step {
    let mut s = Step::new(job.id, "sign-off", "Approve the plan", 0)
        .with_sign_offs_required(vec![ROLE.into()]);
    s.status = StepStatus::Ready;
    s.metadata = json!({"authority_role": ROLE, "decision": decision});
    s
}

fn es() -> EventStamp {
    EventStamp::new("jobs", ActorId::Human("emp-david".into()))
}

fn stamp_on(step: &Step, who: &str, at: DateTime<Utc>) -> SignOffStamp {
    SignOffStamp {
        authority_id: who.into(),
        role: ROLE.into(),
        stamped_at: at,
        shape_hash: step.shape_hash(),
        assurance: boss_core::job::Assurance::Presence,
        presence_nonce: Some(format!("nonce-{who}-{}", at.timestamp_micros())),
        voided_at: None,
        voided_by_event: None,
    }
}

/// The marker payload exactly as the sign-off door
/// (`http/steps.rs` `post_step_sign_off`) builds it — no `stamped_at`.
fn door_marker(step: &Step, st: &SignOffStamp) -> Value {
    json!({
        "job_id": step.job_id.to_string(),
        "step_id": step.id.to_string(),
        "role": st.role,
        "authority_id": st.authority_id,
        "shape_hash": st.shape_hash,
        "assurance": st.assurance,
        "presence_nonce": st.presence_nonce,
    })
}

/// Sign through the adapter the way the door does: its marker, its
/// event stamp.
async fn sign(repo: &PgJobs, step: &Step, st: &SignOffStamp) {
    let ev = es();
    let marker = ev.event(events::STEP_SIGNED_OFF, door_marker(step, st));
    repo.append_sign_off(&step.id, st, &ev, &[marker])
        .await
        .unwrap();
}

async fn open(repo: &PgJobs, j: &Job, step: &Step) {
    let s = es();
    repo.create_job_at(
        j,
        Utc::now(),
        &[s.event(events::JOB_CREATED, serde_json::to_value(j).unwrap())],
    )
    .await
    .unwrap();
    repo.add_step_at(
        step,
        Utc::now(),
        &[s.event(events::STEP_CREATED, events::step_state_payload(step))],
    )
    .await
    .unwrap();
}

/// Relay the outbox into audit_log, as production does.
async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("relay drain");
}

async fn stamps(pool: &PgPool, step: &Step) -> Vec<SignOffStamp> {
    let (v,): (Value,) = sqlx::query_as("SELECT sign_offs FROM steps WHERE id = $1")
        .bind(*step.id.inner().as_uuid())
        .fetch_one(pool)
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn log(pool: &PgPool, kind: &str, at: DateTime<Utc>, payload: Value) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         VALUES ($1, $2, 'jobs', $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(at)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
}

fn t(s: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-26T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::seconds(s)
}

/// THE TRIAGE'S REPRODUCTION (run 40656725), kept in its shape: a live
/// stamp that no edit or completion followed survives a replay, because
/// the append recorded the row it wrote, before the marker.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_sign_off_stamp_survives_a_rebuild() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000f146");
    let step = approve_step(&j, "approved");
    open(&repo, &j, &step).await;
    sign(&repo, &step, &stamp_on(&step, "emp-david", Utc::now())).await;
    drain_outbox(&db.pool).await;

    let kinds: Vec<(String,)> =
        sqlx::query_as("SELECT kind FROM audit_log WHERE kind LIKE 'jobs.%' ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let kinds: Vec<&str> = kinds.iter().map(|(k,)| k.as_str()).collect();
    assert_eq!(
        kinds,
        [
            events::JOB_CREATED,
            events::STEP_CREATED,
            events::STEP_UPDATED,
            events::STEP_SIGNED_OFF
        ],
        "the append records the row it wrote, before the door's marker"
    );

    let live = stamps(&db.pool, &step).await;
    assert_eq!(live.len(), 1, "control: the live row holds the stamp");

    let report = rebuild_jobs_and_steps(&db.pool).await.expect("rebuild");
    let rebuilt = stamps(&db.pool, &step).await;
    assert_eq!(
        rebuilt, live,
        "DETERMINISM: the rebuilt row must hold the live stamp"
    );
    assert_eq!(report.sign_offs_unreproduced, 0, "{report:?}");
}

/// IDEMPOTENT: a log holding BOTH the full row carrying a stamp and the
/// marker for it yields ONE stamp, in either order the log has held
/// them (the append's order now; a later edit's before this car), and a
/// second replay yields the same row as the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_replay_holding_the_marker_and_the_full_row_yields_one_stamp() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    let j = job("00000000-0000-0000-0000-00000000f147");
    let now_order = approve_step(&j, "approved");
    let mut legacy_order = Step::new(j.id, "sign-off", "Approve the other plan", 1)
        .with_sign_offs_required(vec![ROLE.into()]);
    legacy_order.status = StepStatus::Ready;
    legacy_order.metadata = json!({"authority_role": ROLE, "decision": "approved"});

    log(
        pool,
        "jobs.job.created",
        t(0),
        serde_json::to_value(&j).unwrap(),
    )
    .await;
    for step in [&now_order, &legacy_order] {
        log(
            pool,
            "jobs.step.created",
            t(1),
            events::step_state_payload(step),
        )
        .await;
    }
    // The append's order: the row it wrote, then the marker.
    let s1 = stamp_on(&now_order, "emp-david", t(2));
    let mut signed = now_order.clone();
    signed.sign_offs = vec![s1.clone()];
    log(
        pool,
        "jobs.step.updated",
        t(2),
        events::step_state_payload(&signed),
    )
    .await;
    log(
        pool,
        "jobs.step.signed_off",
        t(2),
        door_marker(&now_order, &s1),
    )
    .await;
    // The legacy order: the marker alone, then an edit carrying it.
    let s2 = stamp_on(&legacy_order, "emp-david", t(3));
    log(
        pool,
        "jobs.step.signed_off",
        t(3),
        door_marker(&legacy_order, &s2),
    )
    .await;
    let mut edited = legacy_order.clone();
    edited.assignee_id = Some("emp-david".into());
    edited.sign_offs = vec![s2.clone()];
    log(
        pool,
        "jobs.step.updated",
        t(4),
        events::step_state_payload(&edited),
    )
    .await;

    let first = rebuild_jobs_and_steps(pool).await.expect("rebuild");
    assert_eq!(stamps(pool, &now_order).await, vec![s1.clone()]);
    assert_eq!(stamps(pool, &legacy_order).await, vec![s2.clone()]);
    assert_eq!(first.sign_offs_unreproduced, 0, "{first:?}");

    let second = rebuild_jobs_and_steps(pool).await.expect("rebuild again");
    assert_eq!(first, second, "a replay is idempotent");
    assert_eq!(stamps(pool, &now_order).await, vec![s1]);
    assert_eq!(stamps(pool, &legacy_order).await, vec![s2]);
}

/// A VOID IS PERMANENT across a rebuild: a stamp an edit killed stays
/// dead, and the stamp signed on the moved shape afterwards — which no
/// later event carried — stays alive. Driven through the adapters, so
/// the log is the one production writes.
#[tokio::test(flavor = "multi_thread")]
async fn a_voided_stamp_stays_voided_across_a_rebuild() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000f148");
    let step = approve_step(&j, "approved");
    open(&repo, &j, &step).await;
    sign(&repo, &step, &stamp_on(&step, "emp-david", Utc::now())).await;

    // Request changes: the shape moves, the stamp dies in the merge.
    let patch = json!({"decision": "changes-requested"});
    let moved = repo
        .merge_step_metadata_at(&step.id, patch.as_object().unwrap(), &es())
        .await
        .unwrap();
    // A fresh signature on the moved shape, and nothing after it.
    sign(&repo, &moved, &stamp_on(&moved, "emp-other", Utc::now())).await;
    drain_outbox(&db.pool).await;

    let live = stamps(&db.pool, &step).await;
    assert_eq!(live.len(), 2, "control: {live:?}");
    assert!(live[0].voided_at.is_some(), "control: the first stamp died");
    assert!(live[1].voided_at.is_none(), "control: the second lives");

    let report = rebuild_jobs_and_steps(&db.pool).await.expect("rebuild");
    assert_eq!(
        stamps(&db.pool, &step).await,
        live,
        "the rebuild reproduces the dead stamp dead and the live one alive"
    );
    assert_eq!(report.sign_offs_unreproduced, 0, "{report:?}");
}

/// THE LOG ALREADY WRITTEN. A marker from before this car, which no
/// state event followed, recorded no `stamped_at` — so the stamp cannot
/// be rebuilt exactly. The rebuild does not invent one: it leaves the
/// row without it and COUNTS it, so a replay that differs from the live
/// row says so instead of passing in silence.
#[tokio::test(flavor = "multi_thread")]
async fn a_marker_no_state_event_carried_is_counted_not_invented() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    let j = job("00000000-0000-0000-0000-00000000f149");
    let step = approve_step(&j, "approved");
    log(
        pool,
        "jobs.job.created",
        t(0),
        serde_json::to_value(&j).unwrap(),
    )
    .await;
    log(
        pool,
        "jobs.step.created",
        t(1),
        events::step_state_payload(&step),
    )
    .await;
    let s1 = stamp_on(&step, "emp-david", t(2));
    log(pool, "jobs.step.signed_off", t(2), door_marker(&step, &s1)).await;

    let report = rebuild_jobs_and_steps(pool).await.expect("rebuild");
    assert!(
        stamps(pool, &step).await.is_empty(),
        "no stamp is invented from a marker without its instant"
    );
    assert_eq!(report.sign_offs_unreproduced, 1, "{report:?}");
}
