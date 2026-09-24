//! Postgres half of `repin_workflow_version_at` (design 7cf202a9,
//! backlog 4347a1af).
//!
//! The in-memory adapter states the move as Rust under one lock; the Pg
//! adapter states it as one transaction of UPDATEs and INSERTs with the
//! terminal-row freeze in SQL `CASE`s. Two implementations of one rule,
//! so the rule is pinned against the real SQL: the pin moves, the
//! `repins` record is appended, a pending row reads the target's text,
//! an inserted step gets a row, a row that finished after the plan was
//! read keeps what it ran under — and every event rides the outbox.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, StepId, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::registry::{WorkflowSpec, materialize_steps_at, seedable_platform_workflows};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "ship-a-change")
        .expect("ship-a-change is a platform workflow")
}

fn set_procedure(spec: &mut WorkflowSpec, slug: &str, text: &str) {
    let step = spec
        .steps
        .iter_mut()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("step {slug}"));
    let mut defaults = step
        .metadata_defaults
        .as_object()
        .cloned()
        .unwrap_or_default();
    defaults.insert("procedure".into(), serde_json::json!(text));
    step.metadata_defaults = serde_json::Value::Object(defaults);
}

fn job(from: &WorkflowSpec) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-00000000a4e1").unwrap()),
        kind: from.kind.clone(),
        workflow_version: from.version,
        subject: Subject::new("custom", "feat/x"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "note": "kept" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

async fn outbox(db: &TestDb, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY timestamp",
    )
    .bind(kind)
    .fetch_all(&db.pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(p,)| p)
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repin_writes_the_whole_move_in_one_transaction() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let from = spec();
    let mut to = spec();
    to.version = from.version + 1;
    set_procedure(&mut to, "build", "Build it, and say what you measured.");
    set_procedure(&mut to, "merged", "Merge it, and say where.");
    let mut extra = to
        .steps
        .iter()
        .find(|s| s.title == "settled")
        .expect("settled")
        .clone();
    extra.title = "archived".into();
    to.steps.push(extra);

    let j = job(&from);
    repo.create_job(&j).await.unwrap();
    let rows = materialize_steps_at(
        &from,
        &j.subject,
        j.id,
        &j.metadata,
        StepId::new,
        Some(j.opened_on),
        None,
    );
    for s in &rows {
        repo.add_step(s).await.unwrap();
    }
    let plan = boss_jobs::repin::plan(&from, &to, &j, &rows).expect("planned");

    // THE RACE: `merged` finishes after the plan was read. Its row
    // must keep the text it ran under; only the plan's other rows move.
    let mut merged = rows
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("merged"))
        .expect("merged row")
        .clone();
    let merged_text = merged.metadata["procedure"].clone();
    merged.status = StepStatus::Completed;
    repo.update_step(&merged).await.unwrap();

    let stamp = EventStamp::new("jobs", ActorId::Human("emp-bootstrap-admin".into()));
    let record = boss_jobs::repin::record(
        &plan,
        from.version,
        to.version,
        "emp-bootstrap-admin",
        stamp.timestamp,
    );
    let moved = repo
        .repin_workflow_version_at(&j.id, to.version, &plan, &record, &stamp)
        .await
        .unwrap();

    assert_eq!(moved.workflow_version, to.version);
    assert_eq!(moved.metadata["note"], "kept", "other keys survive");
    assert_eq!(
        moved.metadata["repins"].as_array().map(Vec::len),
        Some(1),
        "{}",
        moved.metadata
    );

    let steps = repo.list_steps(&j.id).await.unwrap();
    let by = |slug: &str| {
        steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("{slug} row: {steps:?}"))
    };
    assert_eq!(
        by("build").metadata["procedure"],
        "Build it, and say what you measured."
    );
    assert_eq!(
        by("merged").metadata["procedure"],
        merged_text,
        "a row that finished after the plan was read keeps its text"
    );
    assert_eq!(by("merged").status, StepStatus::Completed);
    assert_eq!(by("archived").status, StepStatus::Pending);
    assert_eq!(steps.len(), rows.len() + 1);

    assert_eq!(outbox(&db, "jobs.job.repinned").await.len(), 1);
    assert_eq!(
        outbox(&db, "jobs.job.repinned").await[0]["job_id"],
        j.id.to_string().as_str()
    );
    assert!(
        outbox(&db, "jobs.step.created")
            .await
            .iter()
            .any(|p| p["spec_slug"] == "archived"),
        "the inserted row's state event rides the outbox"
    );
    let updated = outbox(&db, "jobs.step.updated").await;
    assert!(
        updated.iter().any(|p| p["spec_slug"] == "build"
            && p["metadata"]["procedure"] == "Build it, and say what you measured."),
        "the re-projected row's state event carries the row as written"
    );
}
