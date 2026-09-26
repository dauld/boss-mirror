//! The rebuilder reproduces `jobs.opened_at` from the log, and a
//! later event cannot move it (backlog 6c2eba00, design f2cdff23).
//!
//! `opened_at` is the instant a packet was admitted, promoted out of
//! the `metadata.opened_at` filer convention. Promoting it means
//! answering for the packets already in the system, and the only
//! back-fill this codebase permits is a projection of the log rather
//! than a guess (design question `backfill`):
//!
//!   1. A create event that carries the stamp replays to exactly that
//!      stamp — a rebuild must reproduce what the live write set, to
//!      sub-second resolution.
//!   2. A create event written BEFORE the field existed carries no
//!      stamp, and replays to the instant the log recorded it. That is
//!      the same derivation the migration's back-fill applies, so a
//!      full replay reproduces the columns the migration wrote instead
//!      of drifting from them.
//!   3. A later `jobs.job.updated` does not move it, the same way it
//!      cannot move a packet's partition. When the packet arrived is
//!      decided once.

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::partition::Partition;
use boss_core::port::EventBus;
use boss_core::publisher::EventStamp;
use boss_jobs::PgJobs;
use boss_jobs::port::JobsRepository;
use boss_jobs::rebuild_jobs_and_steps;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

fn stamped() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 20, 17, 40, 16)
        .single()
        .expect("valid instant")
}

fn logged_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 8, 15, 42)
        .single()
        .expect("valid instant")
}

fn job(id: &str, opened_at: Option<DateTime<Utc>>) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).expect("uuid")),
        kind: "keg-return".to_string(),
        workflow_version: 4,
        subject: Subject::new("account", "acct-1"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 20).expect("valid date"),
        opened_at,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: Partition::Real,
    }
}

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

async fn opened_at_of(pool: &PgPool, id: &str) -> Option<DateTime<Utc>> {
    let (at,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT opened_at FROM jobs WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("read the row back");
    at
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_recovers_every_admission_instant_from_the_create_event() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    // A packet admitted the way the create handler admits one: the row
    // and its JOB_CREATED event, carrying the server's stamp, in one
    // transaction.
    let promoted = "00000000-0000-0000-0000-00000000e001";
    let j = job(promoted, Some(stamped()));
    let stamp = EventStamp::new("jobs", ActorId::Human("emp-1".into()));
    let event = stamp.event(
        boss_jobs::events::JOB_CREATED,
        serde_json::to_value(&j).expect("serialize"),
    );
    repo.create_job_at(&j, Utc::now(), &[event])
        .await
        .expect("create");
    assert_eq!(
        opened_at_of(&db.pool, promoted).await,
        Some(stamped()),
        "the live write stamps the column"
    );

    // And a packet filed before the field existed: its create event
    // carries no `opened_at` at all, as every create event did until
    // this car. The log still knows when it recorded it.
    let legacy = "00000000-0000-0000-0000-00000000e002";
    let mut legacy_payload = serde_json::to_value(job(legacy, None)).expect("serialize");
    legacy_payload
        .as_object_mut()
        .expect("object")
        .remove("opened_at");
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload)
         VALUES (gen_random_uuid(), 'jobs.job.created', 'jobs', $1, $2)",
    )
    .bind(logged_at())
    .bind(legacy_payload)
    .execute(&db.pool)
    .await
    .expect("write the pre-promotion create event");
    drain_outbox(&db.pool).await;

    sqlx::query("DELETE FROM steps")
        .execute(&db.pool)
        .await
        .expect("clear steps");
    sqlx::query("DELETE FROM jobs")
        .execute(&db.pool)
        .await
        .expect("clear jobs");

    let report = rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild succeeds");
    assert_eq!(report.jobs_inserted, 2);

    assert_eq!(
        opened_at_of(&db.pool, promoted).await,
        Some(stamped()),
        "a replay reproduces the stamp the live write set, not the event's own instant"
    );
    assert_eq!(
        opened_at_of(&db.pool, legacy).await,
        Some(logged_at()),
        "a pre-promotion packet recovers the instant the log recorded its creation — \
         a projection of the log, never an invention"
    );

    // The adapter reads it back as the typed field, which is what
    // every duration surface will consume.
    let got = repo
        .get_job(&JobId::from_uuid(Uuid::parse_str(promoted).expect("uuid")))
        .await
        .expect("read")
        .expect("the packet");
    assert_eq!(got.opened_at, Some(stamped()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_event_does_not_move_when_the_packet_arrived() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    let id = "00000000-0000-0000-0000-00000000e003";
    let j = job(id, Some(stamped()));
    let stamp = EventStamp::new("jobs", ActorId::Human("emp-1".into()));
    repo.create_job_at(
        &j,
        Utc::now(),
        &[stamp.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&j).expect("serialize"),
        )],
    )
    .await
    .expect("create");

    // An update event claiming a different arrival instant — a client
    // that reconstructed the whole body, or an old payload replayed.
    let mut moved = j.clone();
    moved.title = "retitled".into();
    moved.opened_at = Some(stamped() - chrono::Duration::days(400));
    repo.update_job_at(
        &moved,
        moved.status,
        Utc::now(),
        &[stamp.event(
            boss_jobs::events::JOB_UPDATED,
            serde_json::to_value(&moved).expect("serialize"),
        )],
    )
    .await
    .expect("update");
    drain_outbox(&db.pool).await;

    assert_eq!(
        opened_at_of(&db.pool, id).await,
        Some(stamped()),
        "the live UPDATE leaves the column out, so no later write can move it"
    );

    sqlx::query("DELETE FROM steps")
        .execute(&db.pool)
        .await
        .expect("clear steps");
    sqlx::query("DELETE FROM jobs")
        .execute(&db.pool)
        .await
        .expect("clear jobs");
    rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        opened_at_of(&db.pool, id).await,
        Some(stamped()),
        "and the replay reads the CREATE event's stamp, never the update's"
    );
}

/// THE SAME DERIVATION, TWICE — so it gets an equality test.
///
/// The migration back-fills `opened_at` for every packet already in
/// the system, and the rebuilder fills it for every packet it replays.
/// Those are one rule living in two files (CLAUDE.md §9a), and if they
/// disagree a rebuild silently rewrites what the migration wrote. This
/// runs the migration's own statements — read off disk, not restated
/// here — over rows the rebuilder has already judged, and demands the
/// same answer from both.
#[tokio::test(flavor = "multi_thread")]
async fn the_migrations_back_fill_and_the_rebuilder_agree() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    // Two packets in the pre-migration state: rows with no stamp, and
    // create events in the log — one carrying the field (admitted
    // after the promotion), one predating it.
    let legacy = "00000000-0000-0000-0000-00000000e004";
    let promoted = "00000000-0000-0000-0000-00000000e005";
    for (id, payload_stamp) in [(legacy, None), (promoted, Some(stamped()))] {
        repo.create_job_at(&job(id, None), Utc::now(), &[])
            .await
            .expect("create the unstamped row");
        let mut payload = serde_json::to_value(job(id, payload_stamp)).expect("serialize");
        if payload_stamp.is_none() {
            payload.as_object_mut().expect("object").remove("opened_at");
        }
        sqlx::query(
            "INSERT INTO audit_log (event_id, kind, source, timestamp, payload)
             VALUES (gen_random_uuid(), 'jobs.job.created', 'jobs', $1, $2)",
        )
        .bind(logged_at())
        .bind(payload)
        .execute(&db.pool)
        .await
        .expect("write the create event");
        assert_eq!(
            opened_at_of(&db.pool, id).await,
            None,
            "the row starts where every existing packet starts: unstamped"
        );
    }

    let migration = std::fs::read_to_string(
        boss_testing::repo_root()
            .join("infra/postgres/schema")
            .join("20260922055451-a-packet-is-stamped-with-the-instant-it-was-admitted.sql"),
    )
    .expect("the migration is on disk");
    sqlx::raw_sql(&migration)
        .execute(&db.pool)
        .await
        .expect("the back-fill runs");

    let after_migration = (
        opened_at_of(&db.pool, legacy).await,
        opened_at_of(&db.pool, promoted).await,
    );
    assert_eq!(
        after_migration,
        (Some(logged_at()), Some(stamped())),
        "the back-fill reads each packet's own create event: the log's instant for one \
         filed before the field existed, the payload's own stamp for one filed after"
    );

    // And now the other half of the pair, over the same two packets.
    sqlx::query("DELETE FROM steps")
        .execute(&db.pool)
        .await
        .expect("clear steps");
    sqlx::query("DELETE FROM jobs")
        .execute(&db.pool)
        .await
        .expect("clear jobs");
    rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        (
            opened_at_of(&db.pool, legacy).await,
            opened_at_of(&db.pool, promoted).await,
        ),
        after_migration,
        "a full replay must reproduce the columns the migration wrote, not drift from them"
    );
}
