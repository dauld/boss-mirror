//! Postgres-backed coverage for the two token shapes a run can report.
//!
//! The in-memory port test proves the CONTRACT; this file proves the
//! SCHEMA, and the two halves of that are different claims:
//!
//!   * a run that reports only a total can be stored at all — which it
//!     could not before 20260910151729, because `input_tokens` and
//!     `output_tokens` were `NOT NULL` and no coding agent on this pod
//!     has a split to put in them;
//!   * the total and the split cannot drift apart in the table, because
//!     the writer derives one from the other and a CHECK refuses any
//!     row that disagrees. §9a asks for a collapse where one is
//!     possible and a pin where it is not; Postgres cannot GENERATE
//!     `total_tokens` (a generated column could not then be supplied
//!     for a total-only run), so the pin is the constraint, and a pin
//!     nobody tests is a comment.
//!
//! The raw INSERTs below are deliberate: they go around the Rust
//! writer, which is the only way to ask whether the DATABASE refuses a
//! malformed row or merely trusts its callers.

use boss_core::actor::ActorId;
use boss_jobs::agent_runs::{
    AgentRunLog, NewAgentRun, PgAgentRuns, RunFilter, RunOutcome, TokenUsage,
};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};

fn a_run(run_id: &str, tokens: TokenUsage) -> NewAgentRun {
    NewAgentRun {
        run_id: run_id.into(),
        actor_id: ActorId::agent("claude", "opus-5"),
        started_at: Utc.with_ymd_and_hms(2026, 9, 10, 2, 37, 27).unwrap(),
        finished_at: Utc.with_ymd_and_hms(2026, 9, 10, 2, 47, 58).unwrap(),
        outcome: RunOutcome::Success,
        error: None,
        tokens,
        tool_calls: 52,
        job_id: None,
        branch: Some("fix/the-mirrors-own-merge-is-not-foreign-work".into()),
        detail: serde_json::json!({"host": "dev-pod"}),
    }
}

fn filer() -> ActorId {
    ActorId::agent("claude", "opus-5[1m]")
}

/// The first real caller's run, as it could actually be reported:
/// 142,982 tokens, 52 tool calls, 631 seconds, and no split anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_total_only_run_is_stored_recorded_and_unpriced() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());

    let out = log
        .record_run(
            &a_run("run-mirror-drift", TokenUsage::TotalOnly { total: 142_982 }),
            &filer(),
        )
        .await
        .expect("a total-only run is a well-formed run");
    assert!(out.recorded);

    let held = log
        .list_runs(&RunFilter::default())
        .await
        .expect("lists")
        .pop()
        .expect("one run");
    assert_eq!(held.run.tokens, TokenUsage::TotalOnly { total: 142_982 });
    assert_eq!(held.duration_secs(), 631, "duration is derived, not stored");
    assert_eq!(
        held.usd_micros, None,
        "the card prices the halves differently, so a total has no price"
    );
    assert_eq!(held.priced_by, None);

    // What the columns actually hold: the total present, the split NULL
    // rather than zero. Zero would read as "no input tokens", which is
    // a measurement nobody took.
    let row: (i64, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT total_tokens, input_tokens, output_tokens FROM agent_runs WHERE run_id = $1",
    )
    .bind("run-mirror-drift")
    .fetch_one(&db.pool)
    .await
    .expect("the row is there");
    assert_eq!(row, (142_982, None, None));

    // One transaction, one fact: the projection and the outbox event
    // commit together.
    let (kind, total): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload -> 'total_tokens' FROM event_outbox WHERE kind = $1")
            .bind("agents.run.recorded")
            .fetch_one(&db.pool)
            .await
            .expect("the event is on the outbox");
    assert_eq!(kind, "agents.run.recorded");
    assert_eq!(total, serde_json::json!(142_982));
}

/// A reporter that HAS the split: the stored total is derived from it,
/// and the run prices off the seeded card.
#[tokio::test(flavor = "multi_thread")]
async fn a_split_run_stores_a_derived_total_and_prices() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());

    log.record_run(
        &a_run(
            "run-split",
            TokenUsage::Split {
                input: 128_684,
                output: 14_298,
            },
        ),
        &filer(),
    )
    .await
    .expect("records");

    let row: (i64, Option<i64>, Option<i64>, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT total_tokens, input_tokens, output_tokens, usd_micros, priced_by \
         FROM agent_runs WHERE run_id = $1",
    )
    .bind("run-split")
    .fetch_one(&db.pool)
    .await
    .expect("the row is there");
    assert_eq!(
        (row.0, row.1, row.2),
        (142_982, Some(128_684), Some(14_298)),
        "the caller never sent a total; the writer derived it"
    );
    // 128,684 * $5 + 14,298 * $25 per MTok, off the seeded opus-5 row.
    assert_eq!(row.3, Some(1_000_870));
    assert_eq!(row.4.as_deref(), Some("opus-5"));
}

/// The pin itself. A hand-written row whose total disagrees with its
/// split is refused by the database, not merely by the Rust writer.
#[tokio::test(flavor = "multi_thread")]
async fn the_database_refuses_a_total_that_disagrees_with_its_split() {
    let db = TestDb::new().await;

    let err = insert_raw(&db, "run-disagrees", Some(11), Some(10), Some(2))
        .await
        .expect_err("11 is not 10 + 2");
    assert!(
        err.contains("agent_runs_total_matches_split_check"),
        "the refusal names the constraint: {err}"
    );
}

/// Half a split is not a shape. Completing it by subtracting from the
/// total would invent the other half.
#[tokio::test(flavor = "multi_thread")]
async fn the_database_refuses_half_a_split() {
    let db = TestDb::new().await;

    let err = insert_raw(&db, "run-half", Some(12), Some(10), None)
        .await
        .expect_err("a split is both halves or neither");
    assert!(
        err.contains("agent_runs_split_is_whole_check"),
        "the refusal names the constraint: {err}"
    );
}

/// And a row with neither shape is still a refusal: a run that reports
/// no tokens at all is not a record of what it cost.
#[tokio::test(flavor = "multi_thread")]
async fn the_database_refuses_a_run_reporting_no_tokens_at_all() {
    let db = TestDb::new().await;

    let err = insert_raw(&db, "run-nothing", None, None, None)
        .await
        .expect_err("no tokens is not a run");
    assert!(
        err.contains("total_tokens"),
        "the refusal names the missing column: {err}"
    );
}

/// An INSERT that goes around the Rust writer, so the question asked is
/// what the SCHEMA permits.
async fn insert_raw(
    db: &TestDb,
    run_id: &str,
    total: Option<i64>,
    input: Option<i64>,
    output: Option<i64>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_runs \
         (run_id, actor_id, started_at, finished_at, outcome, total_tokens, input_tokens, \
          output_tokens, recorded_at) \
         VALUES ($1, 'claude:opus-5', NOW(), NOW(), 'success', $2, $3, $4, NOW())",
    )
    .bind(run_id)
    .bind(total)
    .bind(input)
    .bind(output)
    .execute(&db.pool)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}
