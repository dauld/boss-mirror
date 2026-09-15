//! `PgSubjectExistence` — the uniform gate adapter over the
//! `subjects` identity table (R1). Every kind is checked the same
//! way; there are no fall-through kinds anymore.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::JobsRepository;
use boss_jobs::subject_existence::{
    PgSubjectExistence, SubjectExistenceCheck, SubjectExistenceError,
};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn known_identity_passes_unknown_fails_every_kind() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ('account','acc-1'), ('vendor','vnd-1'), ('campaign','cmp-1')")
        .execute(&db.pool)
        .await
        .unwrap();
    let check = PgSubjectExistence::new(db.pool.clone());

    for (kind, id) in [
        ("account", "acc-1"),
        ("vendor", "vnd-1"),
        ("campaign", "cmp-1"),
    ] {
        check
            .check(&Subject::new(kind, id))
            .await
            .unwrap_or_else(|e| panic!("{kind}/{id} should exist: {e}"));
    }
    // vendor + campaign were the fall-through kinds of the old HTTP
    // prober — now they are checked like everything else.
    for (kind, id) in [
        ("vendor", "vnd-ghost"),
        ("campaign", "cmp-ghost"),
        ("account", "acc-ghost"),
    ] {
        match check.check(&Subject::new(kind, id)).await {
            Err(SubjectExistenceError::NotFound(_)) => {}
            other => panic!("{kind}/{id} must be NotFound, got {other:?}"),
        }
    }
}

fn job_about(id: &str, kind: &str, subject: Subject) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.to_string(),
        workflow_version: 1,
        subject,
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// Birth-by-job kinds — declared in the SubjectKind registry via
/// `metadata.birth = "job"` — pass the gate WITHOUT a pre-existing
/// identity row: the Job that references them IS the subject's birth
/// record (`workflow-design` Jobs about the kind under design,
/// `design-doc-review` Jobs about a `custom` doc path). Their identity
/// row is minted inside `create_job_at`'s transaction — the write-side
/// mirror of the rebuilder's `jobs.job.created` pass, which already
/// reproduces exactly these rows from the log. Without the write-side
/// mint, live and rebuilt `subjects` diverge; without the gate pass,
/// the brewery prepare can't open a single `workflow-design` Job and
/// the whole tenant seed starves (the 2026-07-15 install-smoke red).
#[tokio::test(flavor = "multi_thread")]
async fn birth_by_workflows_pass_gate_and_create_mints_identity() {
    let db = TestDb::new().await;
    let check = PgSubjectExistence::new(db.pool.clone());

    // Asserted against the REAL platform seed rows (01-registries.sql),
    // not synthetic fixtures — a seed regression must fail here.
    for (kind, id) in [
        ("workflow", "wholesale-keg-order"),
        (
            "custom",
            "docs/design/subject-identity-and-relationships.md",
        ),
    ] {
        check
            .check(&Subject::new(kind, id))
            .await
            .unwrap_or_else(|e| panic!("{kind}/{id} is birth-by-job and must pass the gate: {e}"));
    }

    // Domain kinds stay fail-closed — birth-by-job is a declared
    // registry property, not a gate bypass.
    match check.check(&Subject::new("account", "acc-ghost")).await {
        Err(SubjectExistenceError::NotFound(_)) => {}
        other => panic!("account/acc-ghost must stay NotFound, got {other:?}"),
    }

    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    // Creating the Job mints the identity row in the same transaction.
    repo.create_job(&job_about(
        "00000000-0000-0000-0000-00000000b001",
        "workflow-design",
        Subject::new("workflow", "wholesale-keg-order"),
    ))
    .await
    .unwrap();
    let minted: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE kind='workflow' AND id='wholesale-keg-order')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(minted, "create_job_at must mint the birth-by-job identity");

    // A domain-kind Job does NOT auto-mint — its subject's identity
    // belongs to the domain write-through, and the gate upstream is
    // what rejects ghosts.
    repo.create_job(&job_about(
        "00000000-0000-0000-0000-00000000b002",
        "sale",
        Subject::new("account", "acc-unminted"),
    ))
    .await
    .unwrap();
    let leaked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE kind='account' AND id='acc-unminted')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!leaked, "domain kinds must not auto-mint on job create");
}
