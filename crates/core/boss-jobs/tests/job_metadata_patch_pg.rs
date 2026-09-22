//! Postgres half of the `merge_job_metadata_at` contract.
//!
//! The in-memory adapter expresses the merge as Rust over a Map; the
//! Pg adapter expresses it as ONE jsonb UPDATE (`CASE … || $2 - $3`).
//! Two implementations of one rule, so the rule is pinned against the
//! real SQL: add-preserves, null-removes, envelope immunity in the
//! audit's exact race shape, the jsonb-null fold, and the JOB_UPDATED
//! outbox row carrying the post-merge state.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/ux/jobs"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

fn patch(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("test patches are objects"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_adds_removes_and_leaves_the_closed_envelope_alone() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    // The audit's race shape: the packet is already closed with its
    // outcome stamped when the one-key merge lands.
    let closed_on = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
    let mut j = job(
        "00000000-0000-0000-0000-000000000001",
        serde_json::json!({ "route": "/ux/jobs", "outcome": "completed", "stale": "x" }),
    );
    j.status = JobStatus::Closed;
    j.closed_on = Some(closed_on);
    repo.create_job(&j).await.unwrap();

    let merged = repo
        .merge_job_metadata_at(
            &j.id,
            &patch(serde_json::json!({ "watchlist_dismissed": "true", "stale": null })),
            &stamp(),
        )
        .await
        .unwrap();

    // The returned Job is the post-merge row.
    assert_eq!(merged.metadata["watchlist_dismissed"], "true");
    assert_eq!(merged.metadata["route"], "/ux/jobs");
    assert!(merged.metadata.get("stale").is_none(), "null removes");
    assert_eq!(merged.status, JobStatus::Closed);
    assert_eq!(merged.closed_on, Some(closed_on));

    // And so is the stored one.
    let after = repo.get_job(&j.id).await.unwrap().unwrap();
    assert_eq!(after.status, JobStatus::Closed, "envelope untouched");
    assert_eq!(after.closed_on, Some(closed_on));
    assert_eq!(after.metadata["outcome"], "completed");
    assert_eq!(after.metadata["watchlist_dismissed"], "true");
    assert!(after.metadata.get("stale").is_none());

    // The outbox row rides the same transaction and carries the
    // post-merge state — what the rebuild will replay.
    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.job.updated' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["status"], "closed");
    assert_eq!(payload["metadata"]["outcome"], "completed");
    assert_eq!(payload["metadata"]["watchlist_dismissed"], "true");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jsonb_null_metadata_folds_to_an_object() {
    // Jobs born with `metadata: null` exist; `null || {…}` is a jsonb
    // error, so the CASE fold is load-bearing.
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job(
        "00000000-0000-0000-0000-000000000002",
        serde_json::Value::Null,
    );
    repo.create_job(&j).await.unwrap();

    let merged = repo
        .merge_job_metadata_at(&j.id, &patch(serde_json::json!({ "a": "1" })), &stamp())
        .await
        .unwrap();
    assert_eq!(merged.metadata, serde_json::json!({ "a": "1" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn merging_into_a_missing_job_is_not_found() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let missing =
        JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-0000000000ff").unwrap());
    let err = repo
        .merge_job_metadata_at(&missing, &patch(serde_json::json!({ "a": "1" })), &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::NotFound(_)), "got: {err}");
}
