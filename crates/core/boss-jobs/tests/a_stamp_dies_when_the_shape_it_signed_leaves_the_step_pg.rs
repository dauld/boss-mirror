//! A STAMP DIES WHEN THE SHAPE IT SIGNED LEAVES THE STEP — the Pg half
//! (backlog c085256d, design 87329a13, option C decided 2026-09-25).
//!
//! The HTTP half (`a_stamp_dies_when_the_shape_it_signed_leaves_the_step`)
//! pins the judgement at both edit doors over both withdrawals. This
//! file pins what the record does with it:
//!
//! - the merge door voids INSIDE its own transaction, beside its
//!   STEP_UPDATED, and records the invalidation event listing the stamps
//!   it voided — it used to judge against a read taken before the write
//!   and record a marker best-effort in a transaction of its own;
//! - a whole-row write carrying a stale copy of the stamps cannot lift
//!   a void;
//! - a rebuild reproduces every void from the log (determinism);
//! - the rows written before this car are re-judged from the log by the
//!   backfill migration, and the backfill agrees with the rebuild over
//!   the same legacy log, stamp for stamp.

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
const MIGRATION: &str = "infra/postgres/schema/20260925145943-a-stamp-dies-when-the-shape-it-signed-leaves-the-step.sql";

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
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
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

fn es() -> EventStamp {
    EventStamp::new("jobs", ActorId::Human("emp-1".into()))
}

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

async fn wipe_projection(pool: &PgPool) {
    sqlx::query("DELETE FROM steps")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs").execute(pool).await.unwrap();
}

/// The merge door, the one an approval surface saves its decision
/// through: the void lands in its own transaction, the event lists what
/// it voided, a stale whole-row write lifts nothing, and a rebuild
/// reproduces the stamps exactly.
#[tokio::test(flavor = "multi_thread")]
async fn the_merge_door_voids_in_its_own_write_and_a_rebuild_reproduces_it() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000c851");
    let s = es();
    repo.create_job_at(
        &j,
        Utc::now(),
        &[s.event(events::JOB_CREATED, serde_json::to_value(&j).unwrap())],
    )
    .await
    .unwrap();
    let step = approve_step(&j, "approved");
    repo.add_step_at(
        &step,
        Utc::now(),
        &[s.event(events::STEP_CREATED, events::step_state_payload(&step))],
    )
    .await
    .unwrap();
    let s1 = stamp_on(&step, "emp-david", Utc::now());
    repo.append_sign_off(&step.id, &s1, Utc::now(), &[])
        .await
        .unwrap();
    // A copy of the step read while S1 was alive — what a racing
    // whole-row writer would hold.
    let stale = repo.get_step(&step.id).await.unwrap().unwrap();

    // Request changes: the decision moves, no stamp is written.
    let patch = json!({"decision": "changes-requested"});
    let merged = repo
        .merge_step_metadata_at(&step.id, patch.as_object().unwrap(), &es())
        .await
        .unwrap();
    assert!(merged.sign_offs[0].voided_at.is_some(), "{merged:?}");
    let row = stamps(&db.pool, &step).await;
    let void_id = row[0].voided_by_event.expect("the row names its void");
    let outbox: Vec<(Uuid, Value)> = sqlx::query_as(
        "SELECT event_id, payload FROM event_outbox \
         WHERE kind = 'jobs.step.stamps_invalidated'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        outbox.len(),
        1,
        "one invalidation, in the merge's own write"
    );
    assert_eq!(
        outbox[0].0, void_id,
        "the stamp names the event that lists it"
    );
    let listed: Vec<SignOffStamp> = serde_json::from_value(outbox[0].1["voided"].clone()).unwrap();
    assert!(listed.len() == 1 && listed[0].same_stamp(&s1), "{listed:?}");

    // X back through the merge door: nothing alive to void, and the
    // dead stamp stays dead.
    let patch = json!({"decision": "approved"});
    let back = repo
        .merge_step_metadata_at(&step.id, patch.as_object().unwrap(), &es())
        .await
        .unwrap();
    assert_eq!(back.shape_hash(), s1.shape_hash, "back on the signed shape");
    assert!(!back.sign_offs_satisfied(), "the dead stamp does not count");

    // A whole-row write of the copy read while S1 was alive lifts nothing.
    repo.update_step_at(&stale, Utc::now(), &[]).await.unwrap();
    let row = stamps(&db.pool, &step).await;
    assert_eq!(row[0].voided_by_event, Some(void_id), "{row:?}");

    // The rebuild reproduces the row's stamps from the log.
    let st = repo.get_step(&step.id).await.unwrap().unwrap();
    let written = es().event(events::STEP_UPDATED, events::step_state_payload(&st));
    repo.record_events(&[written]).await.unwrap();
    drain_outbox(&db.pool).await;
    let before = stamps(&db.pool, &step).await;
    wipe_projection(&db.pool).await;
    rebuild_jobs_and_steps(&db.pool).await.expect("rebuild");
    assert_eq!(stamps(&db.pool, &step).await, before);
}

/// One legacy `audit_log` row, inserted in order so its id follows the
/// ones before it.
async fn log(pool: &PgPool, kind: &str, at: DateTime<Utc>, payload: Value) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         VALUES ($1, $2, 'jobs', $3, $4)",
    )
    .bind(id)
    .bind(kind)
    .bind(at)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn signed_off(step: &Step, st: &SignOffStamp) -> Value {
    json!({
        "job_id": step.job_id.to_string(), "step_id": step.id.to_string(),
        "role": st.role, "authority_id": st.authority_id, "shape_hash": st.shape_hash,
        "assurance": "presence", "presence_nonce": st.presence_nonce,
    })
}

/// The shape every invalidation had before this car: the step, and no
/// list of what died.
fn legacy_invalidation(step: &Step) -> Value {
    json!({
        "job_id": step.job_id.to_string(), "step_id": step.id.to_string(),
        "stale_roles": [ROLE], "required_roles": [ROLE],
    })
}

/// THE ROWS ALREADY WRITTEN, re-judged from the log. A legacy A-B-A as
/// the old code logged it: S1 on X, rejected (S2 on Y), X restored, and
/// then a fresh S3 on X by someone else — each edit a STEP_UPDATED plus
/// an invalidation carrying no list. S1 died at the first edit, S2 at the
/// second, S3 lives. The rebuild and the backfill migration must both
/// say exactly that, and say it identically.
#[tokio::test(flavor = "multi_thread")]
async fn rows_written_before_the_rule_are_re_judged_from_the_log_as_the_rebuild_judges_them() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    let j = job("00000000-0000-0000-0000-00000000c852");
    let t = |s: i64| {
        DateTime::parse_from_rfc3339("2026-09-20T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(s)
    };
    let x = approve_step(&j, "approved");
    let mut y = x.clone();
    y.metadata["decision"] = json!("rejected");

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
        events::step_state_payload(&x),
    )
    .await;
    let s1 = stamp_on(&x, "emp-david", t(2));
    log(pool, "jobs.step.signed_off", t(2), signed_off(&x, &s1)).await;
    let mut at_y = y.clone();
    at_y.sign_offs = vec![s1.clone()];
    log(
        pool,
        "jobs.step.updated",
        t(3),
        events::step_state_payload(&at_y),
    )
    .await;
    let e1 = log(
        pool,
        "jobs.step.stamps_invalidated",
        t(3),
        legacy_invalidation(&x),
    )
    .await;
    let s2 = stamp_on(&y, "emp-david", t(4));
    log(pool, "jobs.step.signed_off", t(4), signed_off(&y, &s2)).await;
    let mut back = x.clone();
    back.sign_offs = vec![s1.clone(), s2.clone()];
    log(
        pool,
        "jobs.step.updated",
        t(5),
        events::step_state_payload(&back),
    )
    .await;
    let e2 = log(
        pool,
        "jobs.step.stamps_invalidated",
        t(5),
        legacy_invalidation(&x),
    )
    .await;
    let s3 = stamp_on(&x, "emp-other", t(6));
    log(pool, "jobs.step.signed_off", t(6), signed_off(&x, &s3)).await;
    let mut last = x.clone();
    last.sign_offs = vec![s1.clone(), s2.clone(), s3.clone()];
    log(
        pool,
        "jobs.step.updated",
        t(7),
        events::step_state_payload(&last),
    )
    .await;

    // The rebuild's reading.
    let report = rebuild_jobs_and_steps(pool).await.expect("rebuild");
    assert_eq!(report.stamps_voided, 2, "{report:?}");
    let rebuilt = stamps(pool, &x).await;
    assert_eq!(
        rebuilt[0].voided_by_event,
        Some(e1),
        "S1 died at the first edit"
    );
    assert_eq!(rebuilt[0].voided_at, Some(t(3)));
    assert_eq!(
        rebuilt[1].voided_by_event,
        Some(e2),
        "S2 died at the second"
    );
    assert_eq!(rebuilt[1].voided_at, Some(t(5)));
    assert!(
        rebuilt[2].voided_at.is_none(),
        "S3 was signed after both: alive"
    );
    let mut judged = last.clone();
    judged.sign_offs = rebuilt.clone();
    assert!(judged.sign_offs_satisfied(), "the fresh S3 still counts");
    judged.sign_offs.pop();
    assert!(!judged.sign_offs_satisfied(), "the revived S1 does not");

    // The backfill's reading, over the row as the old code left it.
    sqlx::query("UPDATE steps SET sign_offs = $2 WHERE id = $1")
        .bind(*x.id.inner().as_uuid())
        .bind(serde_json::to_value(&last.sign_offs).unwrap())
        .execute(pool)
        .await
        .unwrap();
    let sql = std::fs::read_to_string(boss_testing::repo_root().join(MIGRATION))
        .expect("the migration is in the tree");
    sqlx::raw_sql(&sql)
        .execute(pool)
        .await
        .expect("the backfill runs");
    assert_eq!(
        stamps(pool, &x).await,
        rebuilt,
        "the backfill and the rebuild judge the same log the same way"
    );
    // And a second run changes nothing.
    sqlx::raw_sql(&sql).execute(pool).await.expect("a re-run");
    assert_eq!(stamps(pool, &x).await, rebuilt);
}

/// AN UNREADABLE LIST VOIDS, NOT NOTHING (backlog 4174c4a9, the review's
/// rebuild nit). An invalidation whose `voided` does not parse used to
/// void nothing in the replay, so the rebuilt row kept alive a stamp the
/// live edit had killed. Every live void kills every stamp alive on the
/// row at that write, so the replay reads it as the legacy event.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalidation_whose_list_does_not_parse_voids_every_live_stamp_in_the_rebuild() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    let j = job("00000000-0000-0000-0000-00000000c855");
    let t = |s: i64| {
        DateTime::parse_from_rfc3339("2026-09-25T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(s)
    };
    let x = approve_step(&j, "approved");
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
        events::step_state_payload(&x),
    )
    .await;
    let s1 = stamp_on(&x, "emp-david", t(2));
    let mut signed = x.clone();
    signed.sign_offs = vec![s1];
    log(
        pool,
        "jobs.step.updated",
        t(2),
        events::step_state_payload(&signed),
    )
    .await;
    let mut bad = legacy_invalidation(&x);
    bad["voided"] = json!("not a list of stamps");
    let e1 = log(pool, "jobs.step.stamps_invalidated", t(3), bad).await;

    let report = rebuild_jobs_and_steps(pool).await.expect("rebuild");
    assert_eq!(report.stamps_voided, 1, "{report:?}");
    let rebuilt = stamps(pool, &x).await;
    assert_eq!(rebuilt[0].voided_by_event, Some(e1), "{rebuilt:?}");
    assert_eq!(rebuilt[0].voided_at, Some(t(3)));
}

/// The run edge a dispatch writes and a claim by a new holder drops. It
/// is metadata, so it is inside the shape a stamp signs.
const RUN: &str = "0d9fc9c6-32e4-451f-9f7d-ecfe25b25a38";

async fn outbox(pool: &PgPool, kind: &str) -> Vec<(Uuid, Value)> {
    sqlx::query_as("SELECT event_id, payload FROM event_outbox WHERE kind = $1")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// THE REVIEW'S RACE, at the Pg append (backlog 4174c4a9). The sign-off
/// door builds its stamp over the step it read; a write that lands before
/// the append moves the row. The append judges the stamp against the row
/// under the lock it writes through, refuses, and writes nothing — not
/// the stamp and not the caller's signed-off marker.
#[tokio::test(flavor = "multi_thread")]
async fn a_stamp_on_a_shape_the_row_has_left_is_refused_under_the_lock() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000c853");
    repo.create_job(&j).await.unwrap();
    let step = approve_step(&j, "approved");
    repo.add_step(&step).await.unwrap();

    let read = repo.get_step(&step.id).await.unwrap().unwrap();
    let s1 = stamp_on(&read, "emp-david", Utc::now());
    let edge = json!({"agent_run": RUN});
    repo.merge_step_metadata_at(&step.id, edge.as_object().unwrap(), &es())
        .await
        .unwrap();

    let marker = es().event(
        events::STEP_SIGNED_OFF,
        json!({"step_id": step.id.to_string()}),
    );
    let refused = repo
        .append_sign_off(&step.id, &s1, Utc::now(), &[marker])
        .await;
    match refused {
        Err(boss_jobs::port::JobsError::StampOffShape {
            signed, current, ..
        }) => {
            assert_eq!(signed, s1.shape_hash);
            assert_ne!(current, s1.shape_hash);
        }
        other => panic!("the append must refuse a stamp off the row's shape, got {other:?}"),
    }
    assert!(stamps(&db.pool, &step).await.is_empty(), "no stamp landed");
    assert!(
        outbox(&db.pool, events::STEP_SIGNED_OFF).await.is_empty(),
        "and no marker recorded for it"
    );

    // A stamp on the row as it stands lands.
    let now = repo.get_step(&step.id).await.unwrap().unwrap();
    let s2 = stamp_on(&now, "emp-david", Utc::now());
    repo.append_sign_off(&step.id, &s2, Utc::now(), &[])
        .await
        .unwrap();
    assert_eq!(stamps(&db.pool, &step).await, vec![s2]);
}

/// THE CLAIM MOVES A SHAPE TOO (backlog 4174c4a9). A claim by a new
/// holder drops the run edge; the stamps that signed the step with it
/// die in the claim's own transaction, the invalidation records beside
/// the claim's events, writing the edge back revives nothing, and a
/// rebuild reproduces the row's stamps from the log.
#[tokio::test(flavor = "multi_thread")]
async fn a_claim_that_drops_the_run_edge_voids_in_its_own_write_and_a_rebuild_reproduces_it() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-00000000c854");
    let s = es();
    repo.create_job_at(
        &j,
        Utc::now(),
        &[s.event(events::JOB_CREATED, serde_json::to_value(&j).unwrap())],
    )
    .await
    .unwrap();
    let mut step = approve_step(&j, "approved");
    step.metadata["agent_run"] = json!(RUN);
    repo.add_step_at(
        &step,
        Utc::now(),
        &[s.event(events::STEP_CREATED, events::step_state_payload(&step))],
    )
    .await
    .unwrap();
    let s1 = stamp_on(&step, "emp-david", Utc::now());
    repo.append_sign_off(&step.id, &s1, Utc::now(), &[])
        .await
        .unwrap();

    // The claim route's own event: the step as claimed, from its read.
    let mut claimed = repo.get_step(&step.id).await.unwrap().unwrap();
    claimed.assignee_id = Some("emp-other".into());
    claimed.status = StepStatus::Active;
    claimed.metadata = boss_jobs::agent_runs::without_edge(&claimed.metadata);
    let claim = es();
    let updated = claim.event(events::STEP_UPDATED, events::step_state_payload(&claimed));
    repo.claim_step_at(&step.id, "emp-other", &claim, &[updated])
        .await
        .unwrap();

    let row = stamps(&db.pool, &step).await;
    let void_id = row[0]
        .voided_by_event
        .expect("the claim that moved the shape voided the stamp on the row");
    assert_eq!(
        row[0].voided_at,
        Some(claim.timestamp),
        "dated by the claim"
    );
    let inv = outbox(&db.pool, events::STEP_STAMPS_INVALIDATED).await;
    assert_eq!(inv.len(), 1, "one invalidation, in the claim's own write");
    assert_eq!(inv[0].0, void_id, "the stamp names the event that lists it");
    let listed: Vec<SignOffStamp> = serde_json::from_value(inv[0].1["voided"].clone()).unwrap();
    assert!(listed.len() == 1 && listed[0].same_stamp(&s1), "{listed:?}");

    // A re-claim by the holder moves nothing and voids nothing.
    repo.claim_step_at(&step.id, "emp-other", &es(), &[])
        .await
        .unwrap();
    assert_eq!(
        outbox(&db.pool, events::STEP_STAMPS_INVALIDATED)
            .await
            .len(),
        1
    );

    // The edge written back: the signed shape again, and the stamp stays dead.
    let edge = json!({"agent_run": RUN});
    let back = repo
        .merge_step_metadata_at(&step.id, edge.as_object().unwrap(), &es())
        .await
        .unwrap();
    assert_eq!(back.shape_hash(), s1.shape_hash, "back on the signed shape");
    assert!(
        !back.sign_offs_satisfied(),
        "the stamp the claim moved does not count"
    );

    // The rebuild reproduces the row's stamps from the log.
    drain_outbox(&db.pool).await;
    let before = stamps(&db.pool, &step).await;
    wipe_projection(&db.pool).await;
    rebuild_jobs_and_steps(&db.pool).await.expect("rebuild");
    assert_eq!(stamps(&db.pool, &step).await, before);
}
