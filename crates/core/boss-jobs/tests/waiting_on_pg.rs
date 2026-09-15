//! `waiting_on` — a cross-job wait as a first-class job edge
//! (e9291570, reopening 50e78d70: six of eight "sitting" Jobs were
//! truthfully waiting on another Job, but the wait lived in metadata
//! prose no board renders, so blocked and stalled looked identical).
//!
//! Contracts pinned:
//! 1. **The edge is declared once, for every kind**: migration 110
//!    seeds `('*', 'waiting_on')` and teaches the write-path guard
//!    the wildcard — a per-kind roster would drift the same way the
//!    gate definition did (CLAUDE.md §9a).
//! 2. **Validated like backlog_item, dialed to abort**: nothing wrote
//!    `waiting_on` before the edge existed (measured live: 0 rows),
//!    so there is no dirty folklore to grandfather — a wait pointing
//!    at a Job that doesn't resolve is exactly the invisible-sitting
//!    disease this exists to cure. Prefix resolution (>= 8 chars,
//!    unambiguous) matches `job_edge_resolves`.
//! 3. **Waiters are findable**: `list_jobs` filters by the BLOCKER's
//!    full id and returns jobs whose `waiting_on` wrote either the
//!    full id or a >= 8-char prefix of it — the clear-on-close
//!    handler's query, so a prefix-writing waiter still wakes.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, kind: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.to_string(),
        workflow_version: 1,
        subject: Subject::new("custom", "main"),
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_seeds_the_wildcard_waiting_on_edge() {
    let db = TestDb::new().await;
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT field_kind, on_missing FROM job_edges \
         WHERE source_kind = '*' AND field_path = 'waiting_on'",
    )
    .fetch_optional(&db.pool)
    .await
    .expect("query");
    let (field_kind, on_missing) = row.expect("the wildcard waiting_on edge is seeded");
    assert_eq!(field_kind, "job_id");
    assert_eq!(
        on_missing, "abort",
        "no folklore predates this edge — a dangling wait must refuse"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wildcard_guard_applies_to_every_kind() {
    let db = TestDb::new().await;
    // Distinct leading bytes: the waiter also lives in `jobs`, and a
    // shared prefix would make the prefix-resolution case ambiguous
    // by construction (the guard correctly refuses ambiguity).
    let blocker_id = "aaaaaaaa-1111-4000-8000-0000000000aa";
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1::uuid, 'blocker-kind', 'custom', 'main', 'B', 'emp-o', 'standard', 'open', CURRENT_DATE), \
                ($2::uuid, 'never-registered-kind', 'custom', 'main', 'W', 'emp-o', 'standard', 'open', CURRENT_DATE)",
    )
    .bind(blocker_id)
    .bind("bbbbbbbb-2222-4000-8000-0000000000bb")
    .execute(&db.pool)
    .await
    .expect("seed jobs");

    // TestDb ships with the restore hatch open (ref_check off);
    // exercising the guard needs it back on for this session.
    let mut conn = db.pool.acquire().await.expect("conn");
    sqlx::query("SET audit_log.ref_check = 'on'")
        .execute(&mut *conn)
        .await
        .expect("re-enable ref check");

    let set = |meta: serde_json::Value| {
        sqlx::query("UPDATE jobs SET metadata = $2 WHERE id = $1::uuid")
            .bind("bbbbbbbb-2222-4000-8000-0000000000bb")
            .bind(meta)
    };

    // A kind no per-kind roster ever named still gets the guard.
    let err = set(serde_json::json!({"waiting_on": "not-a-job"}))
        .execute(&mut *conn)
        .await
        .expect_err("a dangling wait must abort");
    let text = err.to_string();
    assert!(
        text.contains("job edge") && text.contains("waiting_on"),
        "the refusal names the edge: {text}"
    );

    // The full blocker id resolves.
    set(serde_json::json!({"waiting_on": blocker_id}))
        .execute(&mut *conn)
        .await
        .expect("a real wait is accepted");

    // An unambiguous >= 8-char prefix resolves (job_edge_resolves).
    set(serde_json::json!({"waiting_on": &blocker_id[..12]}))
        .execute(&mut *conn)
        .await
        .expect("an unambiguous prefix is accepted");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_filters_by_waiting_on_including_prefix_writers() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let blocker = "00000000-0000-0000-0000-0000000000aa";
    repo.create_job(&job(blocker, "blocker-kind", serde_json::Value::Null))
        .await
        .unwrap();
    // Waiter naming the full id.
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000001",
        "user-feedback",
        serde_json::json!({"waiting_on": blocker}),
    ))
    .await
    .unwrap();
    // Waiter naming an 8+-char prefix (the folklore's dominant shape).
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000002",
        "ship-a-change",
        serde_json::json!({"waiting_on": &blocker[..8]}),
    ))
    .await
    .unwrap();
    // Waiting on a DIFFERENT job — must not match.
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000003",
        "user-feedback",
        serde_json::json!({"waiting_on": "00000000-0000-0000-0000-0000000000cc"}),
    ))
    .await
    .unwrap();
    // No wait at all.
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000004",
        "user-feedback",
        serde_json::Value::Null,
    ))
    .await
    .unwrap();

    let (rows, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                waiting_on: Some(blocker.to_string()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 2, "full-id and prefix waiters, nothing else");
    let mut kinds: Vec<_> = rows.iter().map(|j| j.kind.as_str()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["ship-a-change", "user-feedback"]);
}
