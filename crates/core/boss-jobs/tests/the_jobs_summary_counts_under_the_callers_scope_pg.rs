//! `JobsRepository::count_jobs_by_kind` against Postgres counts under
//! the caller's scope, and its counts are `list_jobs`'s totals.
//!
//! WHY IT EXISTS (backlog 19f08bd6, 2026-09-26). `/api/jobs/summary`
//! counted every packet for any caller because the port took no scope.
//! It now takes the list's `JobScope`, and the Postgres adapter spells
//! the scope as its own SQL — a second statement of the rule, so it is
//! pinned here against the first: for every scope shape and status, the
//! per-kind counts equal what `list_jobs` totals under the same scope.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

/// `(kind, owner, subject_kind, subject_id, status)`.
const PACKETS: &[(&str, &str, &str, &str, JobStatus)] = &[
    ("brew-day", "emp-a", "asset", "FV-1", JobStatus::Open),
    ("brew-day", "emp-a", "asset", "FV-1", JobStatus::Closed),
    ("sale", "emp-a", "account", "acct-1", JobStatus::Open),
    ("brew-day", "emp-b", "asset", "FV-2", JobStatus::Open),
    ("sale", "emp-b", "account", "acct-2", JobStatus::Closed),
    ("sale", "emp-c", "account", "acct-1", JobStatus::Open),
    (
        "cellar-check",
        "emp-c",
        "employee",
        "emp-a",
        JobStatus::Open,
    ),
];

fn job(n: usize, (kind, owner, sk, sid, status): (&str, &str, &str, &str, JobStatus)) -> Job {
    let id = Uuid::parse_str(&format!("19f08bd6-0000-0000-0001-{n:012}")).expect("uuid");
    Job {
        id: JobId::from_uuid(id),
        kind: kind.to_string(),
        workflow_version: 1,
        subject: Subject::new(sk, sid),
        title: format!("packet {n}"),
        owner_id: owner.into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 20).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 21).expect("day")),
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_counts_by_kind_are_the_lists_totals_under_every_scope_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    for (n, p) in PACKETS.iter().enumerate() {
        repo.create_job(&job(n, *p)).await.expect("create");
    }

    let scopes = [
        JobScope::All,
        JobScope::None,
        JobScope::OwnerIs("emp-a".into()),
        JobScope::OwnerIn(vec!["emp-b".into(), "emp-c".into()]),
        JobScope::AccountIn(vec!["acct-1".into(), "emp-a".into()]),
    ];
    for scope in scopes {
        for status in [None, Some(JobStatus::Open), Some(JobStatus::Closed)] {
            let counts = repo
                .count_jobs_by_kind(status, &scope)
                .await
                .expect("count");
            let total: i64 = counts.iter().map(|(_, n)| *n).sum();
            let filter = JobFilter {
                status,
                scope: scope.clone(),
                ..Default::default()
            };
            let (_, list_total) = repo.list_jobs(&filter, 1, 0).await.expect("list");
            assert_eq!(total, list_total, "{scope:?} {status:?}: {counts:?}");
            for (kind, n) in &counts {
                let by_kind = JobFilter {
                    kind: Some(kind.clone()),
                    ..filter.clone()
                };
                let (_, list_n) = repo.list_jobs(&by_kind, 1, 0).await.expect("list");
                assert_eq!(*n, list_n, "{scope:?} {status:?} {kind}");
            }
        }
    }

    // The fixture's own answer, so equality with the list cannot pass
    // by both being empty.
    let own = repo
        .count_jobs_by_kind(None, &JobScope::OwnerIs("emp-a".into()))
        .await
        .expect("count");
    assert_eq!(own, [("brew-day".into(), 2), ("sale".into(), 1)]);
    let territory = repo
        .count_jobs_by_kind(
            Some(JobStatus::Open),
            &JobScope::AccountIn(vec!["acct-1".into(), "emp-a".into()]),
        )
        .await
        .expect("count");
    assert_eq!(territory, [("cellar-check".into(), 1), ("sale".into(), 2)]);
    let all = repo
        .count_jobs_by_kind(None, &JobScope::All)
        .await
        .expect("count");
    assert_eq!(
        all.iter().map(|(_, n)| n).sum::<i64>(),
        PACKETS.len() as i64
    );
}
