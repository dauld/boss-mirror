//! Postgres contract for `workflow_terminal_report` — the one-query
//! override behind `GET /api/workflows/{kind}/terminal-report`
//! (experiments Tier 1, docs/design/network-experiments.md).
//!
//! The port's default implementation computes the report in Rust over
//! `list_jobs`; the Pg adapter answers with a single SQL statement.
//! Two implementations of one rule, so the SQL's arithmetic —
//! grouping by the PINNED `workflow_version`, outcome extraction from
//! `metadata->>'outcome'` over closed rows only, `percentile_cont`
//! over `(closed_on - opened_on)` — is pinned here against the same
//! numbers the in-memory HTTP tests assert.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::port::JobsRepository;
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[allow(clippy::too_many_arguments)]
fn packet(
    kind: &str,
    version: i32,
    status: JobStatus,
    opened: NaiveDate,
    closed: Option<NaiveDate>,
    outcome: Option<&str>,
    simulated: bool,
) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        kind: kind.to_string(),
        workflow_version: version,
        subject: Subject::new("custom", "subj-1"),
        title: format!("{kind} packet"),
        owner_id: "emp-1".into(),
        status,
        priority: Priority::Standard,
        opened_on: opened,
        due_on: None,
        closed_on: closed,
        metadata: match outcome {
            Some(o) => serde_json::json!({ "outcome": o }),
            None => serde_json::json!({}),
        },
        tags: vec![],
        simulated,
    }
}

fn approx(v: Option<f64>, want: f64) -> bool {
    v.is_some_and(|x| (x - want).abs() < 1e-9)
}

fn with_metadata(mut job: Job, metadata: serde_json::Value) -> Job {
    job.metadata = metadata;
    job
}

/// The stamped-instant preference, formula-for-formula with the
/// in-memory HTTP test's `precise_stamps_beat_the_one_day_date_resolution`:
/// `EXTRACT(EPOCH FROM closed_at - opened_at) / 86400.0` when both
/// RFC3339 metadata stamps are present, COALESCEd to the
/// `(closed_on - opened_on)` date arithmetic when they are not.
#[tokio::test(flavor = "multi_thread")]
async fn stamped_instants_override_the_date_arithmetic() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let fixture = vec![
        // v2: one stamped packet, closed 30 minutes after opening.
        // Date math reads it as 0 days.
        with_metadata(
            packet(
                "cold-crash",
                2,
                JobStatus::Closed,
                d(2026, 8, 20),
                Some(d(2026, 8, 20)),
                None,
                false,
            ),
            serde_json::json!({
                "outcome": "done",
                "opened_at": "2026-08-20T09:00:00+00:00",
                "closed_at": "2026-08-20T09:30:00+00:00",
            }),
        ),
        // v1, pre-stamp: keeps `closed_on - opened_on` = 2.
        packet(
            "cold-crash",
            1,
            JobStatus::Closed,
            d(2026, 8, 20),
            Some(d(2026, 8, 22)),
            Some("done"),
            false,
        ),
        // v1, half a stamp: no `opened_at`, so the dates answer = 1.
        with_metadata(
            packet(
                "cold-crash",
                1,
                JobStatus::Closed,
                d(2026, 8, 20),
                Some(d(2026, 8, 21)),
                None,
                false,
            ),
            serde_json::json!({
                "outcome": "done",
                "closed_at": "2026-08-21T09:00:00+00:00",
            }),
        ),
        // v1, stamped but never dated: the stamps alone make it a
        // sample (12 hours = 0.5), where an undated close was none.
        with_metadata(
            packet(
                "cold-crash",
                1,
                JobStatus::Closed,
                d(2026, 8, 20),
                None,
                None,
                false,
            ),
            serde_json::json!({
                "outcome": "done",
                "opened_at": "2026-08-20T10:00:00+00:00",
                "closed_at": "2026-08-20T22:00:00+00:00",
            }),
        ),
    ];
    for p in &fixture {
        repo.create_job(p).await.unwrap();
    }

    let report = repo
        .workflow_terminal_report("cold-crash", None, None)
        .await
        .unwrap();
    assert_eq!(report.len(), 2);

    let v2 = &report[0];
    assert_eq!(v2.version, 2);
    assert_eq!(v2.cycle_time_days.samples, 1);
    assert!(
        approx(v2.cycle_time_days.median, 1800.0 / 86400.0),
        "30 stamped minutes is ~0.0208 days, not 0: {:?}",
        v2.cycle_time_days
    );

    let v1 = &report[1];
    assert_eq!(v1.version, 1);
    assert_eq!(
        v1.cycle_time_days.samples, 3,
        "the stamped-but-undated close is a sample: {:?}",
        v1.cycle_time_days
    );
    assert!(
        approx(v1.cycle_time_days.median, 1.0),
        "median of [0.5, 1, 2]: {:?}",
        v1.cycle_time_days
    );
    assert!(
        approx(v1.cycle_time_days.p90, 1.8),
        "percentile_cont(0.9) of [0.5, 1, 2]: {:?}",
        v1.cycle_time_days
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sql_report_matches_the_port_contract() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    // The same tasting-panel week the in-memory HTTP test measures —
    // one fixture, two adapters, one set of numbers.
    let fixture = vec![
        // v2: dated closes with cycle days 1, 3, 5; one open; one
        // closed row the operator never dated (closed + outcome, but
        // no cycle sample).
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 10),
            Some(d(2026, 8, 11)),
            Some("approved"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 10),
            Some(d(2026, 8, 13)),
            Some("approved"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 12),
            Some(d(2026, 8, 17)),
            Some("rejected"),
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Open,
            d(2026, 8, 15),
            None,
            None,
            false,
        ),
        packet(
            "tasting-panel",
            2,
            JobStatus::Closed,
            d(2026, 8, 14),
            None,
            Some("approved"),
            false,
        ),
        // v1: cycle days 2 and 8; one catch-all close without an
        // outcome; one cancellation (terminal but not closed).
        packet(
            "tasting-panel",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 3)),
            Some("rejected"),
            false,
        ),
        packet(
            "tasting-panel",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 9)),
            None,
            false,
        ),
        packet(
            "tasting-panel",
            1,
            JobStatus::Cancelled,
            d(2026, 8, 2),
            None,
            None,
            false,
        ),
        // Another kind — proves the WHERE clause scopes to one kind.
        packet(
            "keg-return",
            1,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 2)),
            Some("returned"),
            false,
        ),
    ];
    for p in &fixture {
        repo.create_job(p).await.unwrap();
    }

    let report = repo
        .workflow_terminal_report("tasting-panel", None, None)
        .await
        .unwrap();
    assert_eq!(report.len(), 2, "two pinned versions, nothing else");

    let v2 = &report[0];
    assert_eq!(v2.version, 2, "versions sort newest first");
    assert_eq!(v2.total, 5);
    assert_eq!(
        v2.by_status,
        [("closed".to_string(), 4), ("open".to_string(), 1)]
            .into_iter()
            .collect()
    );
    assert_eq!(
        v2.outcomes,
        [("approved".to_string(), 3), ("rejected".to_string(), 1)]
            .into_iter()
            .collect()
    );
    assert_eq!(v2.closed_without_outcome, 0);
    assert_eq!(
        v2.cycle_time_days.samples, 3,
        "the undated close is no sample"
    );
    assert!(
        approx(v2.cycle_time_days.median, 3.0),
        "median of [1,3,5]: {:?}",
        v2.cycle_time_days
    );
    assert!(
        approx(v2.cycle_time_days.p90, 4.6),
        "percentile_cont(0.9) of [1,3,5]: {:?}",
        v2.cycle_time_days
    );

    let v1 = &report[1];
    assert_eq!(v1.version, 1);
    assert_eq!(v1.total, 3);
    assert_eq!(
        v1.by_status,
        [("cancelled".to_string(), 1), ("closed".to_string(), 2)]
            .into_iter()
            .collect()
    );
    assert_eq!(
        v1.outcomes,
        [("rejected".to_string(), 1)].into_iter().collect()
    );
    assert_eq!(v1.closed_without_outcome, 1);
    assert_eq!(v1.cycle_time_days.samples, 2);
    assert!(approx(v1.cycle_time_days.median, 5.0));
    assert!(approx(v1.cycle_time_days.p90, 7.4));
}

#[tokio::test(flavor = "multi_thread")]
async fn since_and_simulated_push_into_the_sql() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let fixture = vec![
        // Real, opened early.
        packet(
            "keg-return",
            4,
            JobStatus::Closed,
            d(2026, 8, 1),
            Some(d(2026, 8, 2)),
            Some("returned"),
            false,
        ),
        // Simulated, opened late — the brewery's experiment traffic.
        packet(
            "keg-return",
            4,
            JobStatus::Closed,
            d(2026, 8, 10),
            Some(d(2026, 8, 12)),
            Some("returned"),
            true,
        ),
        packet(
            "keg-return",
            4,
            JobStatus::Closed,
            d(2026, 8, 11),
            Some(d(2026, 8, 16)),
            Some("lost"),
            true,
        ),
    ];
    for p in &fixture {
        repo.create_job(p).await.unwrap();
    }

    // Default: everything, one version block.
    let all = repo
        .workflow_terminal_report("keg-return", None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].total, 3);

    // simulated=true keeps only the experiment traffic.
    let sim = repo
        .workflow_terminal_report("keg-return", None, Some(true))
        .await
        .unwrap();
    assert_eq!(sim[0].total, 2);
    assert_eq!(
        sim[0].outcomes,
        [("lost".to_string(), 1), ("returned".to_string(), 1)]
            .into_iter()
            .collect()
    );
    assert_eq!(sim[0].cycle_time_days.samples, 2);
    assert!(
        approx(sim[0].cycle_time_days.median, 3.5),
        "[2,5] interpolates"
    );

    // simulated=false keeps only real work.
    let real = repo
        .workflow_terminal_report("keg-return", None, Some(false))
        .await
        .unwrap();
    assert_eq!(real[0].total, 1);

    // since= filters on the opened date.
    let recent = repo
        .workflow_terminal_report("keg-return", Some(d(2026, 8, 5)), None)
        .await
        .unwrap();
    assert_eq!(recent[0].total, 2);

    // A window past every packet: the kind reports no versions at all
    // — an empty report, not an error.
    let none = repo
        .workflow_terminal_report("keg-return", Some(d(2027, 1, 1)), None)
        .await
        .unwrap();
    assert!(none.is_empty());
}

/// The arm dimension (Tier 2, packet 6ea5a12a) in SQL: cohorts group
/// by (version, `metadata->>'experiment_arm'`), the unstamped
/// bystander rides a NULL arm — which is why the CTE joins compare
/// with IS NOT DISTINCT FROM — and the whole answer must equal the
/// port's pure function over the same fixture, row for row.
#[tokio::test(flavor = "multi_thread")]
async fn arm_cohorts_group_apart_and_match_the_port_contract() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let arm = |a: &str, outcome: &str| serde_json::json!({ "outcome": outcome, "experiment_arm": a, "experiment_id": "e-1" });
    let fixture = vec![
        // Candidate cohort on v3: closes in 1 and 2 days.
        with_metadata(
            packet(
                "keg-return",
                3,
                JobStatus::Closed,
                d(2026, 8, 20),
                Some(d(2026, 8, 21)),
                None,
                false,
            ),
            arm("candidate", "returned"),
        ),
        with_metadata(
            packet(
                "keg-return",
                3,
                JobStatus::Closed,
                d(2026, 8, 20),
                Some(d(2026, 8, 22)),
                None,
                false,
            ),
            arm("candidate", "lost"),
        ),
        // Control cohort on v2: closes in 5 days.
        with_metadata(
            packet(
                "keg-return",
                2,
                JobStatus::Closed,
                d(2026, 8, 20),
                Some(d(2026, 8, 25)),
                None,
                false,
            ),
            arm("control", "returned"),
        ),
        // Bystander on the control version — no stamp, still open.
        packet(
            "keg-return",
            2,
            JobStatus::Open,
            d(2026, 8, 1),
            None,
            None,
            false,
        ),
    ];
    for p in &fixture {
        repo.create_job(p).await.unwrap();
    }

    let sql = repo
        .workflow_terminal_report("keg-return", None, None)
        .await
        .unwrap();
    let pure = boss_jobs::port::terminal_report_from_jobs(&fixture, None);
    assert_eq!(
        sql, pure,
        "two implementations of one rule — the SQL and the port helper \
         must produce identical cohort rows"
    );

    assert_eq!(sql.len(), 3, "(v3, candidate), (v2, control), (v2, none)");
    assert_eq!(
        (sql[0].version, sql[0].arm.as_deref()),
        (3, Some("candidate"))
    );
    assert_eq!(sql[0].total, 2);
    assert!(approx(sql[0].cycle_time_days.median, 1.5));
    assert_eq!(
        (sql[1].version, sql[1].arm.as_deref()),
        (2, Some("control"))
    );
    assert!(approx(sql[1].cycle_time_days.median, 5.0));
    assert_eq!(
        (sql[2].version, sql[2].arm.as_deref()),
        (2, None),
        "bystanders stay out of both cohorts"
    );
    assert_eq!(
        sql[2].by_status,
        [("open".to_string(), 1)].into_iter().collect()
    );
}
