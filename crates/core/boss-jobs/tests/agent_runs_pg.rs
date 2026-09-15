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
    AgentRunLog, NewAgentRun, PgAgentRuns, RunFilter, RunOutcome, TokenUsage, rebuild_agent_runs,
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
        // The 2026-09-10 shape: the model rode inside the actor id.
        model: None,
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

// --------------------------------------------------------------------
// THE MODEL IS A COLUMN (design 6fda05ae, resolution model-on-run).
// The port test proves the rule; these prove the schema holds it, the
// live write reads the agent's default from the `agents` table inside
// the recording transaction, and a rebuild reproduces the column for
// an event written before the column existed.
// --------------------------------------------------------------------

/// `agent-claude` is the one row 20260915212644 seeds, with
/// `default_model = 'opus-5[1m]'`; a run by it that names no model is
/// priced against that default, read from the table, not from a copy.
#[tokio::test(flavor = "multi_thread")]
async fn a_registered_agents_run_is_stored_with_the_registry_default_model() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());

    let run = NewAgentRun {
        run_id: "run-registered".into(),
        actor_id: ActorId::RegisteredAgent("agent-claude".into()),
        ..a_run(
            "run-registered",
            TokenUsage::Split {
                input: 128_684,
                output: 14_298,
            },
        )
    };
    let out = log.record_run(&run, &filer()).await.expect("records");
    assert_eq!(out.run.model(), Some("opus-5[1m]"));

    let row: (String, Option<String>, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT actor_id, model, usd_micros, priced_by FROM agent_runs WHERE run_id = $1",
    )
    .bind("run-registered")
    .fetch_one(&db.pool)
    .await
    .expect("the row is there");
    assert_eq!(
        row.0, "agent-claude",
        "the actor is the registered id, model-free"
    );
    assert_eq!(
        row.1.as_deref(),
        Some("opus-5[1m]"),
        "the model is its own column"
    );
    // opus-5[1m] is priced at the opus-5 rate: 1,000,870 micro-USD.
    assert_eq!(row.2, Some(1_000_870));
    assert_eq!(row.3.as_deref(), Some("opus-5[1m]"));

    // The event carries the resolved model, so the rebuild below never
    // has to ask the registry.
    let model: serde_json::Value =
        sqlx::query_scalar("SELECT payload -> 'model' FROM event_outbox WHERE kind = $1")
            .bind("agents.run.recorded")
            .fetch_one(&db.pool)
            .await
            .expect("the event is on the outbox");
    assert_eq!(model, serde_json::json!("opus-5[1m]"));
}

/// A run by a registered id that names its own model keeps it: the
/// agent's default is a fallback, not an override.
#[tokio::test(flavor = "multi_thread")]
async fn a_registered_agents_run_that_names_its_model_keeps_it() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());

    let run = NewAgentRun {
        run_id: "run-named".into(),
        actor_id: ActorId::RegisteredAgent("agent-claude".into()),
        model: Some("haiku-4-5".into()),
        ..a_run(
            "run-named",
            TokenUsage::Split {
                input: 900,
                output: 100,
            },
        )
    };
    let out = log.record_run(&run, &filer()).await.expect("records");
    assert_eq!(out.run.model(), Some("haiku-4-5"));
    assert_eq!(out.run.priced_by.as_deref(), Some("haiku-4-5"));
    assert_eq!(out.run.usd_micros, Some(1_400));
}

/// A legacy colon-form report is stored with the model it implies, so
/// the column answers for every row and no reader parses an actor id.
#[tokio::test(flavor = "multi_thread")]
async fn a_colon_form_run_is_stored_with_the_model_its_actor_id_carries() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());
    log.record_run(
        &a_run("run-colon", TokenUsage::TotalOnly { total: 142_982 }),
        &filer(),
    )
    .await
    .expect("records");
    let model: Option<String> =
        sqlx::query_scalar("SELECT model FROM agent_runs WHERE run_id = $1")
            .bind("run-colon")
            .fetch_one(&db.pool)
            .await
            .expect("the row is there");
    assert_eq!(model.as_deref(), Some("opus-5"));
}

/// The rebuild reproduces the column from an event written BEFORE the
/// column existed — the same rule the migration's backfill applies,
/// so a rebuilt table and a migrated one agree (determinism).
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_derives_the_model_for_an_event_written_before_the_column() {
    let db = TestDb::new().await;
    // A 2026-09-10-shaped payload: no `model` key at all.
    let payload = serde_json::json!({
        "run_id": "run-old",
        "actor_id": "claude:opus-5[1m]",
        "started_at": "2026-09-10T02:37:27Z",
        "finished_at": "2026-09-10T02:47:58Z",
        "outcome": "success",
        "error": null,
        "input_tokens": null,
        "output_tokens": null,
        "total_tokens": 142982,
        "tool_calls": 52,
        "job_id": null,
        "branch": "fix/x",
        "detail": {},
        "usd_micros": null,
        "priced_by": null,
        "_actor": "claude:opus-5[1m]"
    });
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         VALUES (gen_random_uuid(), 'agents.run.recorded', 'jobs', NOW(), $1)",
    )
    .bind(&payload)
    .execute(&db.pool)
    .await
    .expect("seed the legacy event");

    let report = rebuild_agent_runs(&db.pool).await.expect("rebuilds");
    assert_eq!(report.runs_inserted, 1);

    let row: (String, Option<String>) =
        sqlx::query_as("SELECT actor_id, model FROM agent_runs WHERE run_id = 'run-old'")
            .fetch_one(&db.pool)
            .await
            .expect("the rebuilt row is there");
    assert_eq!(row.0, "claude:opus-5[1m]", "the log is not rewritten");
    assert_eq!(
        row.1.as_deref(),
        Some("opus-5[1m]"),
        "the model is derived from the colon form, as the backfill derives it"
    );
}

/// The migration's backfill and the rebuild's derivation are the SAME
/// rule written twice — once in SQL for the rows that exist, once in
/// Rust for the events they came from — and §9a says a fact that lives
/// twice gets an equality test. This runs the actual migration file
/// (idempotent by construction) over a row shaped as every row was
/// before the column, and asks the two answers to agree.
#[tokio::test(flavor = "multi_thread")]
async fn the_backfill_and_the_rebuild_derive_the_same_model() {
    let db = TestDb::new().await;
    // A pre-column row, written around the Rust writer with model NULL.
    sqlx::query(
        "INSERT INTO agent_runs \
         (run_id, actor_id, started_at, finished_at, outcome, total_tokens, recorded_at) \
         VALUES ('run-pre', 'claude:opus-5[1m]', NOW(), NOW(), 'success', 1, NOW())",
    )
    .execute(&db.pool)
    .await
    .expect("a pre-column row");

    let migration = boss_testing::repo_root()
        .join("infra/postgres/schema/20260915223855-the-model-is-a-fact-about-the-run.sql");
    let sql = std::fs::read_to_string(&migration).expect("the migration file is in the tree");
    sqlx::raw_sql(&sql)
        .execute(&db.pool)
        .await
        .expect("the migration re-applies cleanly");

    let backfilled: Option<String> =
        sqlx::query_scalar("SELECT model FROM agent_runs WHERE run_id = 'run-pre'")
            .fetch_one(&db.pool)
            .await
            .expect("the row is there");
    assert_eq!(backfilled.as_deref(), Some("opus-5[1m]"));

    // The Rust half of the same rule, on the same actor id.
    let run = NewAgentRun {
        actor_id: ActorId::agent("claude", "opus-5[1m]"),
        ..a_run("run-pre", TokenUsage::TotalOnly { total: 1 })
    };
    assert_eq!(run.model(), backfilled.as_deref());
}
