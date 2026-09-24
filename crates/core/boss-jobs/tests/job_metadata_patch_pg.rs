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

/// The Pg half of `close_job_at` (backlog 29a7ea09), in the shape
/// measured on car 6b23d135 at 2026-09-24T22:18:10Z: both closers read
/// the packet open, a writer merges a key, the terminal close lands,
/// then the catch-all close — whose copy predates both — lands after
/// it. The close is ONE UPDATE that writes only what a close owns and
/// only while the row is open, so the merged key survives the first
/// close, the second writes and records nothing, and the outcome
/// survives it. The JOB_UPDATED outbox row is the post-close row.
#[tokio::test(flavor = "multi_thread")]
async fn a_close_keeps_a_key_merged_after_the_closers_read_the_row() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job(
        "00000000-0000-0000-0000-00000000c105",
        serde_json::json!({ "branch": "fix/x" }),
    );
    repo.create_job(&j).await.unwrap();

    repo.merge_job_metadata_at(
        &j.id,
        &patch(serde_json::json!({ "merged_sha": "abc123" })),
        &stamp(),
    )
    .await
    .unwrap();

    let s = stamp();
    let marker = |closed: &Job| {
        vec![s.event(
            "jobs.job.closed",
            serde_json::json!({
                "id": closed.id.to_string(),
                "outcome": closed.metadata.get("outcome"),
            }),
        )]
    };
    let first_day = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
    let closed = repo
        .close_job_at(
            &j.id,
            first_day,
            &patch(serde_json::json!({
                "outcome": "disproved",
                "closed_at": "2026-09-24T22:18:10.556307+00:00",
            })),
            &s,
            &marker,
        )
        .await
        .unwrap()
        .expect("an open packet closes");
    assert_eq!(closed.status, JobStatus::Closed);
    assert_eq!(closed.closed_on, Some(first_day));
    assert_eq!(closed.metadata["outcome"], "disproved");
    assert_eq!(
        closed.metadata["merged_sha"], "abc123",
        "a key merged after the closer read the row must survive the close"
    );
    assert_eq!(closed.metadata["branch"], "fix/x");

    let second = repo
        .close_job_at(
            &j.id,
            NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
            &patch(serde_json::json!({ "closed_at": "2026-09-24T22:18:10.586476+00:00" })),
            &s,
            &marker,
        )
        .await
        .unwrap();
    assert!(second.is_none(), "a closed packet does not close twice");

    let stored = repo.get_job(&j.id).await.unwrap().unwrap();
    assert_eq!(stored.status, JobStatus::Closed);
    assert_eq!(stored.closed_on, Some(first_day));
    assert_eq!(stored.metadata["outcome"], "disproved");
    assert_eq!(stored.metadata["merged_sha"], "abc123");
    assert_eq!(
        stored.metadata["closed_at"],
        "2026-09-24T22:18:10.556307+00:00"
    );

    // Exactly one close was recorded, both events in its transaction,
    // and the state event carries the key the close did not own.
    let (closes,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM event_outbox WHERE kind = 'jobs.job.closed' AND payload->>'id' = $1",
    )
    .bind(j.id.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(closes, 1, "the losing close records nothing");
    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.job.updated' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["status"], "closed");
    assert_eq!(payload["metadata"]["outcome"], "disproved");
    assert_eq!(payload["metadata"]["merged_sha"], "abc123");

    let missing =
        JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-00000000c1ff").unwrap());
    let err = repo
        .close_job_at(
            &missing,
            first_day,
            &patch(serde_json::json!({})),
            &s,
            &marker,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::NotFound(_)), "got: {err}");
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

/// The Pg half of `append_step_correction_at` (design 4105b020): the
/// append is one jsonb UPDATE, so the rule `corrections::appended`
/// states in Rust is pinned against the real SQL — the list grows at
/// the tail, other keys survive, a jsonb-null metadata and a non-list
/// under the key both fold to an empty list first, and the two events
/// ride the write's own transaction.
#[tokio::test(flavor = "multi_thread")]
async fn a_correction_appends_at_the_tail_and_records_both_events() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job(
        "00000000-0000-0000-0000-00000000c0a1",
        serde_json::json!({ "area": "boss-jobs" }),
    );
    repo.create_job(&j).await.unwrap();

    let first = serde_json::json!({ "step": "s-1", "field": "evidence", "reads": "a" });
    let second = serde_json::json!({ "step": "s-1", "field": "evidence", "reads": "b" });
    let (_, i0) = repo
        .append_step_correction_at(&j.id, &first, &stamp())
        .await
        .unwrap();
    let (after, i1) = repo
        .append_step_correction_at(&j.id, &second, &stamp())
        .await
        .unwrap();
    assert_eq!((i0, i1), (0, 1));
    assert_eq!(after.metadata["area"], "boss-jobs");
    assert_eq!(
        after.metadata["corrections"],
        serde_json::json!([first, second])
    );
    let stored = repo.get_job(&j.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["corrections"][1]["reads"], "b");

    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.step.corrected' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["index"], 1);
    assert_eq!(payload["step_id"], "s-1");
    assert_eq!(payload["correction"]["reads"], "b");
    let (updated,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.job.updated' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(updated["metadata"]["corrections"][1]["reads"], "b");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_correction_folds_a_null_metadata_and_a_non_list_key_to_an_empty_list() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let entry = serde_json::json!({ "step": "s", "field": "f" });

    let bare = job(
        "00000000-0000-0000-0000-00000000c0a2",
        serde_json::Value::Null,
    );
    repo.create_job(&bare).await.unwrap();
    let (after, i) = repo
        .append_step_correction_at(&bare.id, &entry, &stamp())
        .await
        .unwrap();
    assert_eq!(i, 0);
    assert_eq!(
        after.metadata,
        serde_json::json!({ "corrections": [entry] })
    );

    let odd = job(
        "00000000-0000-0000-0000-00000000c0a3",
        serde_json::json!({ "corrections": "an author's own note" }),
    );
    repo.create_job(&odd).await.unwrap();
    let (after, i) = repo
        .append_step_correction_at(&odd.id, &entry, &stamp())
        .await
        .unwrap();
    assert_eq!(i, 0);
    assert_eq!(after.metadata["corrections"], serde_json::json!([entry]));

    let missing =
        JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-00000000c0ff").unwrap());
    let err = repo
        .append_step_correction_at(&missing, &entry, &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::NotFound(_)), "got: {err}");
}
