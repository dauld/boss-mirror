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
use boss_core::agent::BudgetDecision;
use boss_jobs::agent_runs::{
    AgentRunError, AgentRunLog, NewAgentRun, PgAgentRuns, PricingBasis, RunFilter, RunOutcome,
    TokenUsage, rebuild_agent_runs,
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
        "the seeded `opus-5` row declares no blend, so a bare total on it has no price"
    );
    assert_eq!(held.priced_by, None);
    assert_eq!(held.pricing_basis(), None, "no figure, no basis");

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

/// A run that reports NO count is a row with a NULL total — unknown,
/// not zero, the same distinction `usd_micros` draws for an unpriced
/// run (backlog 65c9c05a). Until 2026-09-19 the column was NOT NULL,
/// so `boss dispatch --report` without `--tokens` wrote a 0 with a
/// `detail.tokens_reported: false` beside it, and 13 of 42 live rows
/// read as a measurement of zero to any query that did not know to
/// check the flag.
#[tokio::test(flavor = "multi_thread")]
async fn the_database_holds_a_run_that_reported_no_tokens_as_null() {
    let db = TestDb::new().await;

    insert_raw(&db, "run-nothing", None, None, None)
        .await
        .expect("a run with no count is a record of the run");

    let (total, input, output): (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT total_tokens, input_tokens, output_tokens FROM agent_runs WHERE run_id = $1",
    )
    .bind("run-nothing")
    .fetch_one(&db.pool)
    .await
    .expect("the row");
    assert_eq!((total, input, output), (None, None, None));

    // And it reads back through the adapter as the shape that means
    // unknown, rather than as a zero the port invented.
    let held = PgAgentRuns::new(db.pool.clone())
        .list_runs(&RunFilter::default())
        .await
        .expect("lists")
        .pop()
        .expect("the row is there");
    assert_eq!(held.run.tokens, TokenUsage::Unreported);
    assert_eq!(held.run.tokens.total(), None);
    assert_eq!(held.usd_micros, None, "no tokens is no price");
}

/// THE OTHER SIDE OF THE CUTOVER. The 13 rows written before
/// 20260919194503 carry `total_tokens = 0` for "the harness printed no
/// usage line", distinguishable only by a companion
/// `detail.tokens_reported: false`, and `agent_runs` is insert-once, so
/// they cannot be corrected by re-reporting and MUST NOT be rewritten —
/// a rebuild from the log would put the zeros straight back. So the
/// reinterpretation lives in the read relation: a legacy zero reads as
/// no count at all, which is what it was (backlog f19589ac).
#[tokio::test(flavor = "multi_thread")]
async fn a_pre_cutover_zero_reads_back_as_no_count() {
    let db = TestDb::new().await;

    insert_legacy(
        &db,
        "run-fake-zero",
        0,
        serde_json::json!({"tokens_reported": false}),
    )
    .await
    .expect("the legacy shape is exactly what the table already holds");

    // The ROW is untouched — this is a reading, not a backfill.
    let stored: Option<i64> =
        sqlx::query_scalar("SELECT total_tokens FROM agent_runs WHERE run_id = $1")
            .bind("run-fake-zero")
            .fetch_one(&db.pool)
            .await
            .expect("the row");
    assert_eq!(stored, Some(0), "history is not rewritten");

    let held = PgAgentRuns::new(db.pool.clone())
        .list_runs(&RunFilter::default())
        .await
        .expect("lists")
        .pop()
        .expect("the row is there");
    assert_eq!(held.run.tokens, TokenUsage::Unreported);
    assert_eq!(held.run.tokens.total(), None, "a fake zero is not a zero");
}

/// And the reinterpretation is bounded by the evidence, not by a
/// remembered date: a zero WITHOUT the flag is a row that said it
/// measured zero, and it keeps saying so.
#[tokio::test(flavor = "multi_thread")]
async fn a_zero_that_claims_to_be_measured_stays_measured() {
    let db = TestDb::new().await;

    insert_legacy(
        &db,
        "run-real-zero",
        0,
        serde_json::json!({"host": "dev-pod"}),
    )
    .await
    .expect("a stated zero is storable");

    let held = PgAgentRuns::new(db.pool.clone())
        .list_runs(&RunFilter::default())
        .await
        .expect("lists")
        .pop()
        .expect("the row is there");
    assert_eq!(held.run.tokens, TokenUsage::TotalOnly { total: 0 });
}

/// The pre-cutover shape, written the only way it can be: around the
/// Rust writer, which no longer emits it.
async fn insert_legacy(
    db: &TestDb,
    run_id: &str,
    total: i64,
    detail: serde_json::Value,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_runs \
         (run_id, actor_id, started_at, finished_at, outcome, total_tokens, detail, recorded_at) \
         VALUES ($1, 'claude:opus-5', NOW(), NOW(), 'success', $2, $3, NOW())",
    )
    .bind(run_id)
    .bind(total)
    .bind(detail)
    .execute(&db.pool)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// A measured split with no total is refused. The equality CHECK was
/// total while `total_tokens` was NOT NULL; once it can be NULL the
/// comparison evaluates to NULL for such a row, and Postgres treats a
/// NULL CHECK as SATISFIED — so the constraint had to state the
/// presence explicitly or the split would have lost its pin silently
/// (backlog 65c9c05a).
#[tokio::test(flavor = "multi_thread")]
async fn the_database_refuses_a_split_with_no_total_beside_it() {
    let db = TestDb::new().await;

    let err = insert_raw(&db, "run-untotalled", None, Some(10), Some(2))
        .await
        .expect_err("a measured split has a total, and it equals the split");
    assert!(
        err.contains("agent_runs_total_matches_split_check"),
        "the refusal names the constraint: {err}"
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

// --------------------------------------------------------------------
// A RUN IS ALLOWED OR DENIED AGAINST A BUDGET (backlog 7dd9f28c). The
// port test proves the CONTRACT over the in-memory map; this half
// proves the SCHEMA: the caps are read off the live `agents` row, the
// decision lands in `agent_runs.budget`, a refusal lands on the outbox
// and NOT in the table, and a rebuild replays the decision.
// --------------------------------------------------------------------

/// Cap the one seeded agent — a data change on its row, no deploy —
/// at 20,000 micro-USD/hour. Every `priced_run` below is 1,000 tokens
/// at opus-5[1m] (900 in, 100 out) = 7,000 micro-USD, so the cap
/// admits two and refuses the third.
async fn cap_the_agent(db: &TestDb, hourly: Option<i64>, concurrent: Option<i32>) {
    sqlx::query(
        "UPDATE agents SET hourly_budget_usd_micros = $1, max_concurrent_runs = $2 \
         WHERE id = 'agent-claude'",
    )
    .bind(hourly)
    .bind(concurrent)
    .execute(&db.pool)
    .await
    .expect("the seeded row takes a cap");
}

fn priced_run(run_id: &str, hh: u32, mm: u32) -> NewAgentRun {
    let started = Utc.with_ymd_and_hms(2026, 9, 15, hh, mm, 0).unwrap();
    NewAgentRun {
        run_id: run_id.into(),
        actor_id: ActorId::RegisteredAgent("agent-claude".into()),
        started_at: started,
        finished_at: started + chrono::Duration::minutes(1),
        ..a_run(
            run_id,
            TokenUsage::Split {
                input: 900,
                output: 100,
            },
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_caps_are_read_off_the_agents_row_and_the_decision_is_stored() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(20_000), None).await;
    let log = PgAgentRuns::new(db.pool.clone());

    let first = log
        .record_run(&priced_run("run-1", 1, 0), &filer())
        .await
        .expect("nothing spent: admitted");
    assert_eq!(
        first.run.budget,
        Some(BudgetDecision::Allow {
            remaining_usd_micros: Some(20_000)
        })
    );
    let second = log
        .record_run(&priced_run("run-2", 1, 10), &filer())
        .await
        .expect("7,000 spent: admitted");
    assert_eq!(
        second.run.budget,
        Some(BudgetDecision::Allow {
            remaining_usd_micros: Some(13_000)
        })
    );

    // The column holds the decision as the wire spells it.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT budget FROM agent_runs WHERE run_id = 'run-2'")
            .fetch_one(&db.pool)
            .await
            .expect("the row is there");
    assert_eq!(
        stored,
        serde_json::json!({"kind": "allow", "remaining_usd_micros": 13_000})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_is_on_the_outbox_and_not_in_the_table() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(20_000), None).await;
    let log = PgAgentRuns::new(db.pool.clone());
    for (id, mm) in [("run-1", 0), ("run-2", 10), ("run-3", 20)] {
        log.record_run(&priced_run(id, 1, mm), &filer())
            .await
            .expect("under the cap");
    }

    // 21,000 spent in the hour before 01:30, cap 20,000.
    let err = log
        .record_run(&priced_run("run-4", 1, 30), &filer())
        .await
        .expect_err("over the cap");
    let AgentRunError::Denied { reason } = &err else {
        panic!("{err:?}");
    };
    assert!(reason.contains("21000 of 20000"), "{reason}");

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs")
        .fetch_one(&db.pool)
        .await
        .expect("counts");
    assert_eq!(rows, 3, "the refused run is not a row");

    let denied: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = 'agents.run.denied'")
            .fetch_one(&db.pool)
            .await
            .expect("the refusal committed to the outbox");
    assert_eq!(denied["run_id"], "run-4");
    assert_eq!(denied["actor_id"], "agent-claude");
    assert_eq!(denied["spent_usd_micros"], 21_000);
    assert_eq!(denied["hourly_budget_usd_micros"], 20_000);
    assert_eq!(denied["window_from"], "2026-09-15T00:30:00Z");
    assert_eq!(denied["usd_micros"], 7_000, "the refused run's own cost");
    assert_eq!(denied["reason"], reason.as_str());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_started_while_another_was_in_flight_is_refused_by_the_concurrency_cap() {
    let db = TestDb::new().await;
    cap_the_agent(&db, None, Some(1)).await;
    let log = PgAgentRuns::new(db.pool.clone());

    // 01:00 -> 01:10.
    let long = NewAgentRun {
        finished_at: Utc.with_ymd_and_hms(2026, 9, 15, 1, 10, 0).unwrap(),
        ..priced_run("run-long", 1, 0)
    };
    log.record_run(&long, &filer()).await.expect("first in");
    let err = log
        .record_run(&priced_run("run-mid", 1, 5), &filer())
        .await
        .expect_err("one of one in flight at 01:05");
    assert!(err.to_string().contains("in flight"), "{err}");
    log.record_run(&priced_run("run-after", 1, 11), &filer())
        .await
        .expect("nothing in flight at 01:11");
}

/// The seeded row has both caps NULL, and that is the live shape: the
/// run is admitted with nothing to count down, never refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_agent_is_unbudgeted_and_admitted() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());
    let out = log
        .record_run(&priced_run("run-free", 1, 0), &filer())
        .await
        .expect("NULL caps admit");
    assert_eq!(
        out.run.budget,
        Some(BudgetDecision::Allow {
            remaining_usd_micros: None
        })
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT budget FROM agent_runs WHERE run_id = 'run-free'")
            .fetch_one(&db.pool)
            .await
            .expect("the row is there");
    assert_eq!(
        stored,
        serde_json::json!({"kind": "allow", "remaining_usd_micros": null})
    );
}

/// Determinism: the decision is replayed from the event, not re-judged.
/// Move the cap after recording; the rebuilt row still says what was
/// decided on the day.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_replays_the_decision_rather_than_rejudging_it() {
    let db = TestDb::new().await;
    cap_the_agent(&db, Some(20_000), None).await;
    let log = PgAgentRuns::new(db.pool.clone());
    log.record_run(&priced_run("run-1", 1, 0), &filer())
        .await
        .expect("admitted with 20,000 left");
    // The outbox is what a live write fills; the rebuild reads
    // audit_log, so move the event across as the relay would.
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         SELECT event_id, kind, source, timestamp, payload FROM event_outbox \
         WHERE kind = 'agents.run.recorded'",
    )
    .execute(&db.pool)
    .await
    .expect("relay the event");
    cap_the_agent(&db, Some(5), None).await;

    let report = rebuild_agent_runs(&db.pool).await.expect("rebuilds");
    assert_eq!(report.runs_inserted, 1);
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT budget FROM agent_runs WHERE run_id = 'run-1'")
            .fetch_one(&db.pool)
            .await
            .expect("the rebuilt row is there");
    assert_eq!(
        stored,
        serde_json::json!({"kind": "allow", "remaining_usd_micros": 20_000}),
        "the cap moved to 5 afterwards; the record keeps the decision made under 20,000"
    );
}

/// An event written before budgets were consulted carries no `budget`
/// key; it rebuilds to NULL — no decision — not to an allow.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_of_a_pre_budget_event_holds_no_decision() {
    let db = TestDb::new().await;
    let payload = serde_json::json!({
        "run_id": "run-old",
        "actor_id": "claude:opus-5[1m]",
        "model": "opus-5[1m]",
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
    .expect("seed the pre-budget event");
    rebuild_agent_runs(&db.pool).await.expect("rebuilds");
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT budget FROM agent_runs WHERE run_id = 'run-old'")
            .fetch_one(&db.pool)
            .await
            .expect("the rebuilt row is there");
    assert_eq!(stored, None, "no decision was made, and the row says so");
}

/// The SEEDED card is the registry, so this is the pin on the migration
/// itself (20260920223003): `opus-5[1m]` — the model every recorded run
/// on this pod has run on — declares an 87.5% input share, and the run
/// shape every coding agent here actually reports is priced by it.
///
/// 200,000 tokens at the blend the two seeded rates weigh to
/// ($7.50/MTok) is $1.50. Before this the same run was one of the 83
/// unpriced rows out of 85 (backlog 6681a803).
#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_card_declares_a_blend_and_prices_a_total_with_it() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());

    let share: Option<i64> =
        sqlx::query_scalar("SELECT blended_input_share_ppm FROM agent_rate_card WHERE model = $1")
            .bind("opus-5[1m]")
            .fetch_one(&db.pool)
            .await
            .expect("the seeded row is there");
    assert_eq!(
        share,
        Some(875_000),
        "the declared ratio, seeded not guessed"
    );

    let mut run = a_run("run-blended", TokenUsage::TotalOnly { total: 200_000 });
    run.actor_id = ActorId::agent("claude", "opus-5[1m]");
    let out = log.record_run(&run, &filer()).await.expect("records");
    assert_eq!(out.run.usd_micros, Some(1_500_000));
    assert_eq!(out.run.priced_by.as_deref(), Some("opus-5[1m]"));
    assert_eq!(
        out.run.pricing_basis(),
        Some(PricingBasis::Blended),
        "the record must say the figure rests on the declared ratio"
    );

    // And the rebuild replays that figure rather than re-pricing, so
    // the basis it derives is the one the run was recorded with. The
    // outbox is what a live write fills; the rebuild reads audit_log,
    // so move the event across as the relay would.
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         SELECT event_id, kind, source, timestamp, payload FROM event_outbox \
         WHERE kind = 'agents.run.recorded'",
    )
    .execute(&db.pool)
    .await
    .expect("relay the event");
    rebuild_agent_runs(&db.pool).await.expect("rebuilds");
    let held = log
        .list_runs(&RunFilter::default())
        .await
        .expect("lists")
        .pop()
        .expect("one run");
    assert_eq!(held.usd_micros, Some(1_500_000));
    assert_eq!(held.pricing_basis(), Some(PricingBasis::Blended));
}
