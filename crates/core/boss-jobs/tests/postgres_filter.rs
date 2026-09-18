//! Postgres-level regression test for the jobs list filter.
//!
//! Guards a `subject_id` filter bug class with two failure modes:
//!   1. The client passes `?subject_id=` but the HTTP handler reads a
//!      different param name, so the filter silently falls through and
//!      the call returns every job system-wide.
//!   2. The handler translates the query param into `filter.subject_id`
//!      correctly, but the Postgres `list_jobs` SQL has no
//!      `subject_id = $X` predicate and ignores the filter, so the
//!      call returns an empty set.
//!
//! The in-memory adapter honors `filter.subject_id`, so the sibling
//! filter test in `policy_gated_handlers.rs` (which runs against
//! `InMemoryJobs`) wouldn't catch a Postgres-only gap. This file runs
//! the same shape against `PgJobs`.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::partition::Partition;
use boss_jobs::port::{JobFilter, JobScope, JobsRepository};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn job(id: &str, kind: &str, subject: Subject) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.to_string(),
        workflow_version: 1,
        subject,
        title: "t".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_jobs_filters_by_subject_id_in_postgres() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let account_a = Subject::new("account", "prac-A");
    let account_b = Subject::new("account", "prac-B");
    let system_a = Subject::new("asset", "SYS-A");

    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000001",
        "sale",
        account_a.clone(),
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000002",
        "sale",
        account_a.clone(),
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000003",
        "sale",
        account_b,
    ))
    .await
    .unwrap();
    repo.create_job(&job(
        "00000000-0000-0000-0000-000000000004",
        "field-service",
        system_a,
    ))
    .await
    .unwrap();

    // No filter → all 4.
    let (_, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 4);

    // subject_id = prac-A → exactly 2 sale jobs.
    let (rows, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("prac-A".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 2, "prac-A should have 2 jobs");
    assert_eq!(rows.len(), 2);

    // subject_id = SYS-A → exactly 1 field-service job.
    let (rows, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("SYS-A".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 1, "SYS-A should have 1 job");
    assert_eq!(rows.len(), 1);

    // subject_id = unknown → zero, not "everything".
    let (_, total) = repo
        .list_jobs(
            &JobFilter {
                scope: JobScope::All,
                subject_id: Some("does-not-exist".into()),
                ..Default::default()
            },
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(total, 0);
}

/// The terminal retention window: everything live, plus what closed
/// recently.
///
/// THE PROBLEM IT SOLVES. A board renders each card in the column of
/// its current step, so terminal packets have to be fetched for the
/// terminal columns to have anything in them. The feedback board
/// therefore asked for `kind=user-feedback&limit=200` with no status
/// filter and got all 173 packets in order to show 14 live ones — 92%
/// finished work, and 27 short of silently truncating at its own
/// limit. Filtering after the fetch does not fix the truncation; the
/// window has to be in the query.
///
/// This runs against Postgres because the rule there is a CASE over
/// two columns while the in-memory adapter expresses it as Rust: two
/// implementations of one contract.
#[tokio::test(flavor = "multi_thread")]
async fn closed_since_keeps_live_and_recent_and_drops_the_rest() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
    let board = Subject::new("account", "board");

    let mut open = job(
        "00000000-0000-0000-0000-0000000000a1",
        "user-feedback",
        board.clone(),
    );
    let mut recent = job(
        "00000000-0000-0000-0000-0000000000a2",
        "user-feedback",
        board.clone(),
    );
    recent.status = JobStatus::Closed;
    recent.closed_on = Some(d(8, 14));
    let mut old = job(
        "00000000-0000-0000-0000-0000000000a3",
        "user-feedback",
        board.clone(),
    );
    old.status = JobStatus::Closed;
    old.closed_on = Some(d(1, 5));
    let mut cancelled = job(
        "00000000-0000-0000-0000-0000000000a4",
        "user-feedback",
        board.clone(),
    );
    cancelled.status = JobStatus::Cancelled;
    cancelled.closed_on = Some(d(1, 6));
    // A blocked packet with no close date: live is live regardless of
    // how long ago it opened, and it is the half of the rule that a
    // naive `closed_on >= $x` would silently delete. Blocked rather
    // than Open so the assertion cannot pass by accident on a default.
    open.status = JobStatus::Blocked;

    for j in [&open, &recent, &old, &cancelled] {
        repo.create_job(j).await.unwrap();
    }

    let filter = JobFilter {
        kind: Some("user-feedback".into()),
        closed_since: Some(d(8, 1)),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
    let got: Vec<String> = rows.iter().map(|j| j.id.to_string()).collect();

    assert_eq!(
        rows.len(),
        2,
        "expected the in-progress packet and the one closed in August, got {got:?}"
    );
    assert_eq!(
        total, 2,
        "the COUNT query must apply the same window as the page"
    );
    assert!(
        rows.iter().any(|j| j.id == open.id),
        "live packets always survive"
    );
    assert!(rows.iter().any(|j| j.id == recent.id));
    assert!(
        !rows.iter().any(|j| j.id == cancelled.id),
        "cancelled is terminal too — an old cancellation is not recent work"
    );
}

/// Without the window, every existing caller behaves exactly as before.
///
/// This is why the window is a new optional field rather than a change
/// to how `status` is interpreted.
#[tokio::test(flavor = "multi_thread")]
async fn no_window_means_the_old_status_behaviour() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let board = Subject::new("account", "board");

    let mut closed = job(
        "00000000-0000-0000-0000-0000000000b1",
        "user-feedback",
        board.clone(),
    );
    closed.status = JobStatus::Closed;
    closed.closed_on = NaiveDate::from_ymd_opt(2026, 1, 5);
    let open = job(
        "00000000-0000-0000-0000-0000000000b2",
        "user-feedback",
        board,
    );
    repo.create_job(&closed).await.unwrap();
    repo.create_job(&open).await.unwrap();

    let all = JobFilter {
        kind: Some("user-feedback".into()),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&all, 100, 0).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "an unfiltered list still returns closed packets"
    );
    assert_eq!(total, 2);

    let only_open = JobFilter {
        kind: Some("user-feedback".into()),
        status: Some(JobStatus::Open),
        ..Default::default()
    };
    let (rows, _) = repo.list_jobs(&only_open, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 1, "status=open still means open only");
    assert_eq!(rows[0].id, open.id);
}

/// `closed_since` and `status` are OR, not AND.
///
/// The board's whole purpose is showing live work next to what just
/// finished. If the two combined as AND, `status=open&closed_within=14`
/// would return only open packets and the terminal columns would be
/// empty again — the bug this window exists to fix. The documented
/// rule is that `closed_since` WINS: it already means "everything
/// live", so a status filter alongside it is redundant at best.
#[tokio::test(flavor = "multi_thread")]
async fn closed_since_overrides_status_rather_than_intersecting_it() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
    let board = Subject::new("account", "board");

    let open = job(
        "00000000-0000-0000-0000-0000000000c1",
        "user-feedback",
        board.clone(),
    );
    let mut recent = job(
        "00000000-0000-0000-0000-0000000000c2",
        "user-feedback",
        board,
    );
    recent.status = JobStatus::Closed;
    recent.closed_on = Some(d(8, 14));
    repo.create_job(&open).await.unwrap();
    repo.create_job(&recent).await.unwrap();

    let filter = JobFilter {
        kind: Some("user-feedback".into()),
        status: Some(JobStatus::Open),
        closed_since: Some(d(8, 1)),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the recently-closed packet must survive a status=open filter"
    );
    assert_eq!(total, 2);
}

/// `simulated` partitions the packet population, and BOTH the rows
/// and the `total` have to obey it.
///
/// WHY THIS IS A REAL FILTER AND NOT A CLIENT CONCERN. Measured on the
/// live system 2026-08-17: **5,201 of 5,964 packets (87%) are
/// simulated**, and of 39 kinds **zero are mixed** — a kind is either
/// entirely the demo tenant's or entirely real. A surface that fetched
/// a page and dropped the simulated rows would draw roughly 26 real
/// packets from a page of 200, report a `total` of 200, and truncate
/// without saying so. That is the failure `closed_since` above was
/// added to prevent, one order of magnitude worse.
///
/// Runs against Postgres because the list and the count are two
/// separate SQL statements with independently numbered binds, and the
/// way this breaks is that one of them gets the clause and the other
/// does not — which no in-memory test can see.
#[tokio::test(flavor = "multi_thread")]
async fn partition_filters_the_rows_and_the_total() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let board = Subject::new("account", "board");

    let mut real = job(
        "00000000-0000-0000-0000-0000000000b1",
        "ship-a-change",
        board.clone(),
    );
    real.title = "real work".into();
    let mut sim_a = job(
        "00000000-0000-0000-0000-0000000000b2",
        "ship-a-change",
        board.clone(),
    );
    sim_a.partition = Partition::Simulated;
    let mut sim_b = job(
        "00000000-0000-0000-0000-0000000000b3",
        "ship-a-change",
        board.clone(),
    );
    sim_b.partition = Partition::Simulated;
    // A shadow packet (packet 508cc38c; migration 20260915221611):
    // created through the port like any other, excluded by the real-
    // only filter exactly as a simulated one is, AND by the simulated-
    // only filter (the sim never sees it, Q5).
    let mut shadow = job(
        "00000000-0000-0000-0000-0000000000b4",
        "ship-a-change",
        board.clone(),
    );
    shadow.title = "shadow run".into();
    shadow.partition = Partition::Shadow;

    for j in [&real, &sim_a, &sim_b, &shadow] {
        repo.create_job(j).await.unwrap();
    }

    let only_real = JobFilter {
        kind: Some("ship-a-change".into()),
        partition: Some(Partition::Real),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&only_real, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 1, "one real packet");
    assert_eq!(
        total, 1,
        "the count query must carry the same clause as the list query — \
         a total that disagrees with the rows is how this breaks"
    );
    assert_eq!(rows[0].title, "real work");

    let only_sim = JobFilter {
        kind: Some("ship-a-change".into()),
        partition: Some(Partition::Simulated),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&only_sim, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(total, 2);

    let only_shadow = JobFilter {
        kind: Some("ship-a-change".into()),
        partition: Some(Partition::Shadow),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&only_shadow, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(total, 1);
    assert_eq!(rows[0].title, "shadow run");
    assert_eq!(
        rows[0].partition,
        Partition::Shadow,
        "the column round-trips the third value"
    );

    // Both columns are written (expand/contract): the derived legacy
    // bool is TRUE for shadow, so an N-1 reader of `jobs.simulated`
    // fails closed on it without knowing the word.
    let (legacy,): (bool,) = sqlx::query_as("SELECT simulated FROM jobs WHERE id = $1")
        .bind(*shadow.id.inner().as_uuid())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(legacy, "jobs.simulated is derived as not-real");

    // Absent means everything, so nothing that exists today moves.
    let all = JobFilter {
        kind: Some("ship-a-change".into()),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&all, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(total, 4);
}

/// `metadata_has` and `metadata_contains` at the Postgres layer.
///
/// The claim behind 4d9aa761 (2026-09-14): three probes written in one
/// day each paged 60-200 rows of a kind and filtered by a metadata key
/// in jq, and one measured the closed listing at 356 rows within 14
/// days against a 200-row page. A limit is not a filter. Both clauses
/// here (`metadata ? $n`, `metadata @> $n`) are asserted on the count
/// query as well as the list query — the two queries are written
/// separately in the adapter, so this is where they drift.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_has_and_contains_narrow_the_rows_and_the_total() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let net = Subject::new("asset", "BOSSNET");

    let mut car = job(
        "00000000-0000-0000-0000-0000000000c1",
        "ship-a-change",
        net.clone(),
    );
    car.title = "the car".into();
    car.metadata = serde_json::json!({ "branch": "feat/x", "proof_probe": "true" });
    let mut twin = job(
        "00000000-0000-0000-0000-0000000000c2",
        "ship-a-change",
        net.clone(),
    );
    twin.title = "another car".into();
    twin.metadata = serde_json::json!({ "branch": "feat/y" });
    // Null metadata — the seeded default — carries no key at all.
    let mut bare = job(
        "00000000-0000-0000-0000-0000000000c3",
        "ship-a-change",
        net.clone(),
    );
    bare.title = "bare".into();

    for j in [&car, &twin, &bare] {
        repo.create_job(j).await.unwrap();
    }

    let has_probe = JobFilter {
        kind: Some("ship-a-change".into()),
        metadata_has: Some("proof_probe".into()),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&has_probe, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 1, "one packet carries proof_probe");
    assert_eq!(
        total, 1,
        "the count query must carry the same `?` clause as the list query"
    );
    assert_eq!(rows[0].title, "the car");

    let has_branch = JobFilter {
        kind: Some("ship-a-change".into()),
        metadata_has: Some("branch".into()),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&has_branch, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(total, 2, "null metadata does not carry the key");

    let one_branch = JobFilter {
        kind: Some("ship-a-change".into()),
        metadata_contains: Some(serde_json::json!({ "branch": "feat/y" })),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&one_branch, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(total, 1);
    assert_eq!(rows[0].title, "another car");

    // Both at once intersect: the key filter and the containment
    // document are separate binds and must both hold.
    let both = JobFilter {
        kind: Some("ship-a-change".into()),
        metadata_has: Some("proof_probe".into()),
        metadata_contains: Some(serde_json::json!({ "branch": "feat/y" })),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&both, 100, 0).await.unwrap();
    assert_eq!((rows.len(), total), (0, 0));
}

/// `kinds` — the kind SET the department listing narrows on — at the
/// Postgres layer, asserted on the count query as well as the list
/// query, since the two are written separately in the adapter.
///
/// The claim behind cc76f755 (2026-09-18): a packet carries no
/// department, its workflow does, so `?department=sales` resolves to
/// the kinds declaring `sales` and asks for exactly those. The leg
/// that matters most is the EMPTY set: `kind = ANY('{}')` must be a
/// real bind matching nothing, because a department nobody declares
/// that answered the unfiltered count is the trap the packet was
/// filed on (prod answered 1944 for a param it never read).
#[tokio::test(flavor = "multi_thread")]
async fn a_kind_set_narrows_the_rows_and_the_total_and_an_empty_set_is_none() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let co = Subject::new("custom", "algedonic");

    for (id, kind) in [
        ("00000000-0000-0000-0000-0000000000d1", "receive-an-inquiry"),
        (
            "00000000-0000-0000-0000-0000000000d2",
            "receive-a-sponsorship",
        ),
        (
            "00000000-0000-0000-0000-0000000000d3",
            "publish-the-landing-page",
        ),
        ("00000000-0000-0000-0000-0000000000d4", "backlog-item"),
    ] {
        repo.create_job(&job(id, kind, co.clone())).await.unwrap();
    }

    let sales = JobFilter {
        kinds: Some(vec![
            "receive-an-inquiry".into(),
            "receive-a-sponsorship".into(),
        ]),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&sales, 100, 0).await.unwrap();
    assert_eq!(rows.len(), 2, "two packets are of the two sales kinds");
    assert_eq!(
        total, 2,
        "the count query must carry the same kind-set clause as the list query"
    );
    assert!(rows.iter().all(|j| j.kind.starts_with("receive-")));

    // The control leg: an EMPTY set is no packet, not every packet.
    let nobody = JobFilter {
        kinds: Some(vec![]),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&nobody, 100, 0).await.unwrap();
    assert_eq!(
        (rows.len(), total),
        (0, 0),
        "an empty kind set must not fall through"
    );

    // And `None` is still no filter.
    let (rows, total) = repo.list_jobs(&JobFilter::default(), 100, 0).await.unwrap();
    assert_eq!((rows.len(), total), (4, 4));

    // Composes with `kind` as an intersection.
    let both = JobFilter {
        kind: Some("backlog-item".into()),
        kinds: Some(vec!["receive-an-inquiry".into()]),
        ..Default::default()
    };
    let (rows, total) = repo.list_jobs(&both, 100, 0).await.unwrap();
    assert_eq!((rows.len(), total), (0, 0));
}
