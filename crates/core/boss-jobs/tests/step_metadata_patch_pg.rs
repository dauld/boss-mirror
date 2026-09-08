//! Postgres half of the `merge_step_metadata_at` contract.
//!
//! The in-memory adapter expresses the merge as Rust over a Map; the
//! Pg adapter expresses it as ONE jsonb UPDATE whose WHERE clause
//! carries the terminal freeze. Two implementations of one rule, so
//! the rule is pinned against the real SQL: add-preserves,
//! null-removes, the jsonb-null fold, the terminal REFUSAL (not a
//! silent freeze — the distinction job 903e6b90 exists for), the
//! missing-vs-frozen disambiguation, and the STEP_UPDATED outbox row
//! carrying the post-merge state.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::NaiveDate;
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
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
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
async fn merge_adds_removes_and_touches_nothing_else() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-000000000001");
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Do the work", 0).with_assignee("emp-1");
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "route": "/ux/jobs", "stale": "x" });
    repo.add_step(&step).await.unwrap();

    let merged = repo
        .merge_step_metadata_at(
            &step.id,
            &patch(serde_json::json!({ "annotation": "checked", "stale": null })),
            &stamp(),
        )
        .await
        .unwrap();

    // The returned Step is the post-merge row.
    assert_eq!(merged.metadata["annotation"], "checked");
    assert_eq!(merged.metadata["route"], "/ux/jobs");
    assert!(merged.metadata.get("stale").is_none(), "null removes");
    assert_eq!(merged.status, StepStatus::Ready, "status untouched");
    assert_eq!(merged.assignee_id.as_deref(), Some("emp-1"));

    // And so is the stored one.
    let after = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["annotation"], "checked");
    assert!(after.metadata.get("stale").is_none());
    assert_eq!(after.status, StepStatus::Ready);

    // The outbox row rides the same transaction and carries the
    // post-merge state under the marker identity key — what the
    // rebuild will replay.
    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM event_outbox WHERE kind = 'jobs.step.updated' ORDER BY timestamp DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["step_id"], step.id.to_string());
    assert_eq!(payload["metadata"]["annotation"], "checked");
    assert_eq!(payload["status"], "ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jsonb_null_metadata_folds_to_an_object() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-000000000002");
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Do the work", 0);
    step.metadata = serde_json::Value::Null;
    repo.add_step(&step).await.unwrap();

    let merged = repo
        .merge_step_metadata_at(&step.id, &patch(serde_json::json!({ "a": "1" })), &stamp())
        .await
        .unwrap();
    assert_eq!(merged.metadata, serde_json::json!({ "a": "1" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_step_is_refused_with_its_status_and_left_untouched() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let j = job("00000000-0000-0000-0000-000000000003");
    repo.create_job(&j).await.unwrap();
    let mut step = Step::new(j.id, "task", "Finished work", 0);
    step.status = StepStatus::Completed;
    step.completed_on = NaiveDate::from_ymd_opt(2026, 8, 20);
    step.metadata = serde_json::json!({ "evidence": "the record" });
    repo.add_step(&step).await.unwrap();

    let err = repo
        .merge_step_metadata_at(
            &step.id,
            &patch(serde_json::json!({ "annotation": "late edit" })),
            &stamp(),
        )
        .await
        .unwrap_err();
    match err {
        JobsError::TerminalStep { id, status } => {
            assert_eq!(id, step.id);
            assert_eq!(status, "completed", "the refusal names the state");
        }
        other => panic!("expected TerminalStep, got: {other}"),
    }

    let after = repo.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.metadata,
        serde_json::json!({ "evidence": "the record" })
    );

    // A refused merge records nothing — the outbox stays empty of
    // step.updated rows for this write.
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.step.updated'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "no event for a write that did not land");
}

#[tokio::test(flavor = "multi_thread")]
async fn merging_into_a_missing_step_is_not_found_not_terminal() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let missing =
        StepId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-0000000000ff").unwrap());
    let err = repo
        .merge_step_metadata_at(&missing, &patch(serde_json::json!({ "a": "1" })), &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::StepNotFound(_)), "got: {err}");
}
