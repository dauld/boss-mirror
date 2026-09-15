//! Postgres-level contract for the Job's admission-fixed partition
//! (real | simulated | shadow, packet 508cc38c): the column round-trips
//! through the adapter, and the UPDATE path cannot move it — the
//! storage enforces the immutability rather than trusting every caller
//! to (the epoch trim in 03-jobs.sql depends on a Job's rows all
//! sharing one fate). The assignments pull surface carries the
//! partition out to My Day, which sees nothing of the Job but the row.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::partition::Partition;
use boss_jobs::port::JobsRepository;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, partition: Partition) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "sale".to_string(),
        workflow_version: 1,
        subject: Subject::new("account", "prac-A"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn partition_round_trips_and_update_cannot_flip_it() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let sim = job("00000000-0000-0000-0000-00000000a001", Partition::Simulated);
    let real = job("00000000-0000-0000-0000-00000000a002", Partition::Real);
    let shadow = job("00000000-0000-0000-0000-00000000a003", Partition::Shadow);
    repo.create_job(&sim).await.unwrap();
    repo.create_job(&real).await.unwrap();
    repo.create_job(&shadow).await.unwrap();

    let got_sim = repo.get_job(&sim.id).await.unwrap().unwrap();
    assert_eq!(
        got_sim.partition,
        Partition::Simulated,
        "column round-trips"
    );
    let got_real = repo.get_job(&real.id).await.unwrap().unwrap();
    assert_eq!(got_real.partition, Partition::Real, "column round-trips");
    let got_shadow = repo.get_job(&shadow.id).await.unwrap().unwrap();
    assert_eq!(
        got_shadow.partition,
        Partition::Shadow,
        "the third value too"
    );

    // An update carrying a flipped value must not move the column —
    // `partition` is absent from the UPDATE's SET list by design.
    let mut flipped = got_shadow.clone();
    flipped.partition = Partition::Real;
    flipped.title = "renamed".into();
    repo.update_job(&flipped).await.unwrap();

    let after = repo.get_job(&shadow.id).await.unwrap().unwrap();
    assert_eq!(after.title, "renamed", "the update itself applied");
    assert_eq!(
        after.partition,
        Partition::Shadow,
        "the partition is immutable at the storage layer"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assignment_rows_carry_the_sim_facts() {
    // My Day renders assignment rows and nothing else, so a simulated
    // packet is only markable there if the row says so. The indexed
    // JOIN is a separate SELECT list from get_job's — pin it.
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let mut sim = job("00000000-0000-0000-0000-00000000b001", Partition::Simulated);
    sim.tags = vec!["nightly".to_string()];
    let real = job("00000000-0000-0000-0000-00000000b002", Partition::Real);
    repo.create_job(&sim).await.unwrap();
    repo.create_job(&real).await.unwrap();
    for j in [&sim, &real] {
        let mut s = Step::new(j.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        s.status = StepStatus::Ready;
        repo.add_step(&s).await.unwrap();
    }

    let rows = repo
        .list_assignments(Some("emp-1"), &[], 100)
        .await
        .unwrap();
    let sim_row = rows.iter().find(|r| r.job_id == sim.id).unwrap();
    assert_eq!(
        sim_row.partition,
        Partition::Simulated,
        "simulated job's assignment row reports it"
    );
    assert_eq!(sim_row.tags, vec!["nightly".to_string()]);
    let real_row = rows.iter().find(|r| r.job_id == real.id).unwrap();
    assert_eq!(
        real_row.partition,
        Partition::Real,
        "a real job's row stays real"
    );
    assert!(real_row.tags.is_empty());

    // Same row shape on the sim workforce's bulk pull.
    let bulk = repo.list_assigned_workable(100).await.unwrap();
    assert_eq!(
        bulk.iter().find(|r| r.job_id == sim.id).unwrap().partition,
        Partition::Simulated,
        "bulk backlog rows carry the partition too"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assignment_rows_carry_the_cars_red_train_count() {
    // A builder's own struck car was invisible on their My Day
    // (d6e53a35, 2026-09-14): the lens builds its card through the
    // yard's one constructor, but the row carried no job metadata, so
    // `red_trains` read 0 there while the yard drew the car struck.
    // The indexed JOIN is its own SELECT list — pin the JSON read.
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let mut struck = job("00000000-0000-0000-0000-00000000c001", Partition::Real);
    struck.metadata = serde_json::json!({ "branch": "fix/x", "red_trains": 2 });
    let clean = job("00000000-0000-0000-0000-00000000c002", Partition::Real);
    repo.create_job(&struck).await.unwrap();
    repo.create_job(&clean).await.unwrap();
    for j in [&struck, &clean] {
        let mut s = Step::new(j.id, "review", "Review", 0).with_assignee("emp-1");
        s.status = StepStatus::Ready;
        repo.add_step(&s).await.unwrap();
    }

    let rows = repo
        .list_assignments(Some("emp-1"), &[], 100)
        .await
        .unwrap();
    let struck_row = rows.iter().find(|r| r.job_id == struck.id).unwrap();
    assert_eq!(struck_row.red_trains, 2, "a twice-struck car's row says so");
    let clean_row = rows.iter().find(|r| r.job_id == clean.id).unwrap();
    assert_eq!(clean_row.red_trains, 0, "no stamp reads as no strikes");

    // Same row shape on the sim workforce's bulk pull.
    let bulk = repo.list_assigned_workable(100).await.unwrap();
    assert_eq!(
        bulk.iter()
            .find(|r| r.job_id == struck.id)
            .unwrap()
            .red_trains,
        2,
        "bulk backlog rows carry the count too"
    );
}
