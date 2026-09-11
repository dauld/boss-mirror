//! Postgres half of "a completion names its actor" (c17871fe).
//!
//! The in-memory adapter carries `completed_by` / `completed_at` as
//! two struct fields and freezes them in Rust; the Pg adapter carries
//! them as two columns and freezes them in the UPDATE's CASE. Two
//! implementations of one rule, so the rule is pinned against the
//! real SQL: the stamps persist through every step SELECT, a write
//! against a terminal row cannot re-attribute it, and the per-packet
//! audit read walks the job's slice of `audit_log` after the same
//! outbox → relay hops production takes.

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_jobs::PgJobs;
use boss_jobs::port::JobsRepository;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

fn job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/ux/jobs"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stamps_persist_and_a_terminal_row_keeps_them() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-000000000011");
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Do the work", 0).with_assignee("emp-1");
    step.status = StepStatus::Ready;
    repo.add_step(&step).await.unwrap();

    let before = repo.get_step(&step.id).await.unwrap().unwrap();
    assert!(before.completed_by.is_none() && before.completed_at.is_none());

    // The flip, stamped the way the handler stamps it.
    let at = Utc.with_ymd_and_hms(2026, 9, 8, 4, 30, 0).unwrap();
    let mut done = step.clone();
    done.status = StepStatus::Completed;
    done.completed_on = Some(at.date_naive());
    done.completed_by = Some(ActorId::Human("emp-david".into()));
    done.completed_at = Some(at);
    repo.update_step_at(&done, at, &[]).await.unwrap();

    let after = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Completed);
    assert_eq!(
        after.completed_by,
        Some(ActorId::Human("emp-david".into())),
        "completed_by round-trips through the column as the typed actor"
    );
    assert_eq!(after.completed_at, Some(at));

    // Every step read carries the columns, not only get_step.
    let listed = repo.list_steps(&j.id).await.unwrap();
    assert_eq!(listed[0].completed_by, after.completed_by);
    assert_eq!(listed[0].completed_at, after.completed_at);

    // A later write against the terminal row — a stale re-PUT, a
    // redelivery — cannot re-attribute it: the CASE keeps the stamps
    // with the status, exactly as it keeps `completed_on`.
    let later = Utc.with_ymd_and_hms(2026, 9, 8, 5, 0, 0).unwrap();
    let mut again = done.clone();
    again.completed_by = Some(ActorId::Automation("rule:copy-answer".into()));
    again.completed_at = Some(later);
    repo.update_step_at(&again, later, &[]).await.unwrap();

    let frozen = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        frozen.completed_by,
        Some(ActorId::Human("emp-david".into())),
        "a terminal row keeps who completed it"
    );
    assert_eq!(frozen.completed_at, Some(at), "and when");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_actor_round_trips_in_its_wire_form() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-000000000012");
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Automated work", 0);
    step.status = StepStatus::Ready;
    repo.add_step(&step).await.unwrap();

    let at = Utc::now();
    let mut done = step.clone();
    done.status = StepStatus::Completed;
    done.completed_by = Some(ActorId::Agent {
        mode: "claude".into(),
        model: "fable".into(),
    });
    done.completed_at = Some(at);
    repo.update_step_at(&done, at, &[]).await.unwrap();

    let after = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.completed_by,
        Some(ActorId::Agent {
            mode: "claude".into(),
            model: "fable".into(),
        }),
        "an agent session is visibly not a person after the round trip"
    );
    let (raw,): (Option<String>,) = sqlx::query_as("SELECT completed_by FROM steps WHERE id = $1")
        .bind(*step.id.inner().as_uuid())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        raw.as_deref(),
        Some("claude:fable"),
        "stored in the wire form"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn events_for_job_reads_the_packets_slice_oldest_first() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let mine = "00000000-0000-0000-0000-000000000021";
    let other = "00000000-0000-0000-0000-000000000022";
    let step_id = Uuid::new_v4().to_string();

    let ev = |kind: &str, payload: serde_json::Value| Event::new("jobs", kind, payload, Utc::now());
    repo.record_events(&[
        // The job's own lifecycle event names it as `id`.
        ev(
            "jobs.job.created",
            serde_json::json!({"id": mine, "_actor": "emp-david"}),
        ),
        // A step event names it as `job_id`.
        ev(
            "step.done.task",
            serde_json::json!({"job_id": mine, "step_id": step_id, "_actor": "emp-david"}),
        ),
        // Another packet's history is not this one's.
        ev(
            "step.done.task",
            serde_json::json!({"job_id": other, "step_id": "x", "_actor": "automation:rule"}),
        ),
    ])
    .await
    .expect("events record");
    drain_outbox(&db.pool).await;

    let job_id = JobId::from_uuid(Uuid::parse_str(mine).unwrap());
    let rows = repo.events_for_job(&job_id, 100).await.expect("read back");
    let kinds: Vec<&str> = rows.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["jobs.job.created", "step.done.task"],
        "the packet's slice, oldest first, both spellings of the job id"
    );
    assert_eq!(rows[1].payload["step_id"], step_id);
    assert_eq!(rows[1].payload["_actor"], "emp-david");

    // The limit keeps the NEWEST rows, still answered oldest first.
    let last = repo.events_for_job(&job_id, 1).await.expect("read back");
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].kind, "step.done.task");
}
