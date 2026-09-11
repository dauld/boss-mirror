//! Postgres-level pin for the jobs list order.
//!
//! `jobs.opened_on` is a DATE. A list ordered by it alone leaves every
//! packet admitted on one day tied, and a LIMIT over a tie returns an
//! arbitrary subset: on 2026-09-07 one day held 398 closed pr-trains,
//! and the yard's window of 60 showed eight morning trains while
//! dropping the five that closed that evening — the third "a limit is
//! not a filter" instance in a day. Its stranded-green cross-ref and
//! the page's own `?kind=pr-train&limit=40` had the same hole.
//!
//! The fix is `ORDER BY opened_on DESC, created_at DESC, id`. The
//! in-memory adapter sorts the same way (its `job_created_at` mirror
//! is written from the same `now` this adapter binds into
//! `created_at`); its side of the pin is the `same_day_jobs_*` /
//! `a_day_of_jobs_*` tests in `src/in_memory.rs`. This file runs the
//! same shape against `PgJobs`, where the tie actually lives.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use uuid::Uuid;

fn job(id: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "pr-train".to_string(),
        workflow_version: 1,
        subject: Subject::new("branch", "fix/x"),
        title: title.into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated: false,
    }
}

fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 7, h, m, s).unwrap()
}

fn all() -> JobFilter {
    JobFilter {
        scope: JobScope::All,
        ..Default::default()
    }
}

fn titles(page: &[Job]) -> Vec<&str> {
    page.iter().map(|j| j.title.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_day_of_jobs_lists_newest_first_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    // Admitted out of clock order, so heap/insertion order is not what
    // the assertion accidentally passes on.
    for (id, title, now) in [
        (
            "00000000-0000-0000-0000-000000000003",
            "20:07",
            at(20, 7, 0),
        ),
        (
            "00000000-0000-0000-0000-000000000001",
            "07:39",
            at(7, 39, 0),
        ),
        (
            "00000000-0000-0000-0000-000000000002",
            "08:15",
            at(8, 15, 0),
        ),
    ] {
        repo.create_job_at(&job(id, title), now, &[]).await.unwrap();
    }

    // The same order every time.
    for _ in 0..3 {
        let (page, total) = repo.list_jobs(&all(), 10, 0).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(titles(&page), ["20:07", "08:15", "07:39"]);
    }

    // A window of one on a same-day set is the newest, not an
    // arbitrary one — and the next window continues from there.
    let (page, total) = repo.list_jobs(&all(), 1, 0).await.unwrap();
    assert_eq!(total, 3, "the window narrows the page, not the total");
    assert_eq!(titles(&page), ["20:07"]);
    let (page, _) = repo.list_jobs(&all(), 1, 1).await.unwrap();
    assert_eq!(titles(&page), ["08:15"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn same_instant_ties_break_on_id_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let same = at(12, 0, 0);
    for (id, title) in [
        ("00000000-0000-0000-0000-000000000003", "03"),
        ("00000000-0000-0000-0000-000000000001", "01"),
        ("00000000-0000-0000-0000-000000000002", "02"),
    ] {
        repo.create_job_at(&job(id, title), same, &[])
            .await
            .unwrap();
    }

    let (page, _) = repo.list_jobs(&all(), 10, 0).await.unwrap();
    assert_eq!(titles(&page), ["01", "02", "03"]);
}
