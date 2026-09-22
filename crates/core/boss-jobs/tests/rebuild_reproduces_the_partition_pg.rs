//! The rebuilder reproduces `jobs.partition` from the log (packet
//! 508cc38c; migration 20260915221611).
//!
//! A Job's origin lives on its create event as the `_partition` marker,
//! with the older `_simulated` bool beside it. A replay must reproduce
//! the same value the live write set — read `_partition` first, so a
//! shadow packet comes back shadow, and fall back to the bool for an
//! event that predates the word, so the simulated company is not
//! quietly turned real by a rebuild over an old slice. Both columns are
//! rewritten (expand/contract): the derived `simulated` is the not-real
//! bool an N-1 reader still tests.

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
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

fn job(id: &str, partition: Partition) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "keg-return".to_string(),
        workflow_version: 4,
        subject: Subject::new("account", "acct-1"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition,
    }
}

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

async fn columns(pool: &PgPool, id: &str) -> (String, bool) {
    sqlx::query_as("SELECT partition, simulated FROM jobs WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the row back")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_reproduces_every_partition_from_the_create_event() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    // Three packets written the way the create handler writes them:
    // the row and its JOB_CREATED event, stamped with the packet's
    // admission-fixed partition, in one transaction.
    let real = "00000000-0000-0000-0000-00000000d001";
    let sim = "00000000-0000-0000-0000-00000000d002";
    let shadow = "00000000-0000-0000-0000-00000000d003";
    for (id, partition) in [
        (real, Partition::Real),
        (sim, Partition::Simulated),
        (shadow, Partition::Shadow),
    ] {
        let j = job(id, partition);
        let stamp =
            EventStamp::new("jobs", ActorId::Human("emp-1".into())).with_partition(partition);
        let event = stamp.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&j).unwrap(),
        );
        repo.create_job_at(&j, Utc::now(), &[event]).await.unwrap();
    }
    // And one packet from before the word existed: its create event
    // carries only the bool, as every event did until this car.
    let legacy = "00000000-0000-0000-0000-00000000d004";
    let mut legacy_payload = serde_json::to_value(job(legacy, Partition::Simulated)).unwrap();
    legacy_payload.as_object_mut().unwrap().remove("partition");
    legacy_payload["_simulated"] = serde_json::Value::Bool(true);
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload)
         VALUES (gen_random_uuid(), 'jobs.job.created', 'jobs', now(), $1)",
    )
    .bind(legacy_payload)
    .execute(&db.pool)
    .await
    .unwrap();
    drain_outbox(&db.pool).await;

    assert_eq!(columns(&db.pool, shadow).await, ("shadow".into(), true));

    sqlx::query("DELETE FROM steps")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_jobs_and_steps(&db.pool)
        .await
        .expect("rebuild succeeds");
    assert_eq!(report.jobs_inserted, 4);

    assert_eq!(columns(&db.pool, real).await, ("real".into(), false));
    assert_eq!(columns(&db.pool, sim).await, ("simulated".into(), true));
    assert_eq!(
        columns(&db.pool, shadow).await,
        ("shadow".into(), true),
        "a shadow packet replays as shadow, and its derived bool stays not-real"
    );
    assert_eq!(
        columns(&db.pool, legacy).await,
        ("simulated".into(), true),
        "a pre-shadow event carrying only _simulated replays as the simulated company"
    );
    // And the adapter reads the word back as the type.
    let got = repo
        .get_job(&JobId::from_uuid(Uuid::parse_str(shadow).unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.partition, Partition::Shadow);
}
