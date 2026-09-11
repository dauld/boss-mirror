//! Postgres adapter for `AgentRunLog`.
//!
//! The record is ONE transaction holding three things: the rate-card
//! read that prices the run, the `agent_runs` row, and the
//! `agents.run.recorded` event on the outbox. The log and the
//! projection commit or fail together — the outbox phase-2 shape
//! `PgJobs` already uses — so there is no window where a run was
//! charged to nobody or recorded twice.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{AgentRunError, AgentRunLog, RecordedRun, validate};
use super::types::{
    AgentRun, NewAgentRun, RateCardRow, RunFilter, RunOutcome, TokenUsage, price_run,
};

pub struct PgAgentRuns {
    pool: PgPool,
}

impl PgAgentRuns {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> AgentRunError {
    AgentRunError::Storage(e.to_string())
}

/// Every column `agent_runs` holds, in one place, so the SELECTs here
/// and the rebuilder's INSERT cannot disagree about the row's shape.
pub(super) const RUN_COLUMNS: &str = "run_id, actor_id, started_at, finished_at, outcome, error, \
     total_tokens, input_tokens, output_tokens, tool_calls, usd_micros, priced_by, job_id, branch, \
     detail, recorded_at";

/// `INSERT INTO agent_runs (<cols>) VALUES ($1,…,$n)`, with the
/// placeholder list DERIVED from [`RUN_COLUMNS`] rather than typed out
/// beside it. A hand-written `$1..$15` is the same fact twice in two
/// files (§9a): adding a column would mean editing three places and
/// the compiler would catch none of them — the mismatch surfaces at
/// runtime as a bind-count error. One definition cannot drift from
/// itself.
pub(super) fn insert_run_sql() -> String {
    let placeholders = (1..=RUN_COLUMNS.split(',').count())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "INSERT INTO agent_runs ({RUN_COLUMNS}) VALUES ({placeholders}) \
         ON CONFLICT (run_id) DO NOTHING"
    )
}

pub(super) fn row_to_run(row: &sqlx::postgres::PgRow) -> Result<AgentRun, AgentRunError> {
    let actor: String = row.try_get("actor_id").map_err(storage)?;
    let outcome_str: String = row.try_get("outcome").map_err(storage)?;
    let outcome = RunOutcome::parse(&outcome_str).ok_or_else(|| {
        // A row the code cannot read is a defect in whatever wrote it,
        // and naming the value is what makes it findable.
        AgentRunError::Storage(format!(
            "agent_runs.outcome holds {outcome_str:?}, which is not success|failed|cancelled"
        ))
    })?;
    // The split is optional and the total is not: a reporter with one
    // number is a real reporter (see `TokenUsage`). A row that holds
    // neither shape is a defect in whatever wrote it, and naming the
    // row is what makes it findable.
    let total_tokens: i64 = row.try_get("total_tokens").map_err(storage)?;
    let input_tokens: Option<i64> = row.try_get("input_tokens").map_err(storage)?;
    let output_tokens: Option<i64> = row.try_get("output_tokens").map_err(storage)?;
    let run_id: String = row.try_get("run_id").map_err(storage)?;
    let tokens = TokenUsage::from_parts(
        input_tokens.map(|v| u64::try_from(v).unwrap_or(0)),
        output_tokens.map(|v| u64::try_from(v).unwrap_or(0)),
        Some(u64::try_from(total_tokens).unwrap_or(0)),
    )
    .map_err(|e| {
        AgentRunError::Storage(format!(
            "agent_runs row {run_id:?} has unreadable tokens: {e}"
        ))
    })?;
    let tool_calls: i32 = row.try_get("tool_calls").map_err(storage)?;
    let usd_micros: Option<i64> = row.try_get("usd_micros").map_err(storage)?;
    Ok(AgentRun {
        run: NewAgentRun {
            run_id,
            // Infallible parse: every string is SOME actor class, and
            // an actor that is not an agent reads back as not-an-agent
            // rather than as an error, which is what a rebuild of an
            // older row needs.
            actor_id: actor
                .parse()
                .unwrap_or_else(|_| ActorId::Automation("platform".into())),
            started_at: row.try_get("started_at").map_err(storage)?,
            finished_at: row.try_get("finished_at").map_err(storage)?,
            outcome,
            error: row.try_get("error").map_err(storage)?,
            tokens,
            tool_calls: u32::try_from(tool_calls).unwrap_or(0),
            job_id: row.try_get("job_id").map_err(storage)?,
            branch: row.try_get("branch").map_err(storage)?,
            detail: row.try_get("detail").map_err(storage)?,
        },
        usd_micros: usd_micros.map(|v| u64::try_from(v).unwrap_or(0)),
        priced_by: row.try_get("priced_by").map_err(storage)?,
        recorded_at: row.try_get("recorded_at").map_err(storage)?,
    })
}

fn card_row(row: &sqlx::postgres::PgRow) -> Result<RateCardRow, AgentRunError> {
    let input: i64 = row.try_get("input_usd_micros_per_mtok").map_err(storage)?;
    let output: i64 = row.try_get("output_usd_micros_per_mtok").map_err(storage)?;
    Ok(RateCardRow {
        model: row.try_get("model").map_err(storage)?,
        input_usd_micros_per_mtok: u64::try_from(input).unwrap_or(0),
        output_usd_micros_per_mtok: u64::try_from(output).unwrap_or(0),
        note: row.try_get("note").map_err(storage)?,
    })
}

#[async_trait]
impl AgentRunLog for PgAgentRuns {
    async fn record_run(
        &self,
        run: &NewAgentRun,
        recorded_by: &ActorId,
    ) -> Result<RecordedRun, AgentRunError> {
        validate(run)?;

        let mut tx = self.pool.begin().await.map_err(storage)?;

        // Price inside the transaction so the row names the card it was
        // actually priced against.
        let card_rows = sqlx::query(
            "SELECT model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, note \
             FROM agent_rate_card",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let card = card_rows
            .iter()
            .map(card_row)
            .collect::<Result<Vec<_>, _>>()?;
        let priced = price_run(&card, run);

        let event = super::events::run_recorded_event(recorded_by, run, &priced);

        let inserted = sqlx::query(&insert_run_sql())
            .bind(&run.run_id)
            .bind(run.actor_id.to_string())
            .bind(run.started_at)
            .bind(run.finished_at)
            .bind(run.outcome.as_str())
            .bind(run.error.as_deref())
            // The total goes in as the ONE figure every run has; the
            // split goes in beside it only when it was measured. The
            // table's CHECK refuses a pair that disagrees, and the
            // derivation here is why it never can.
            .bind(i64::try_from(run.tokens.total()).unwrap_or(i64::MAX))
            .bind(
                run.tokens
                    .input()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
            .bind(
                run.tokens
                    .output()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
            .bind(i32::try_from(run.tool_calls).unwrap_or(i32::MAX))
            .bind(
                priced
                    .as_ref()
                    .map(|(m, _)| i64::try_from(*m).unwrap_or(i64::MAX)),
            )
            .bind(priced.as_ref().map(|(_, model)| model.as_str()))
            .bind(run.job_id)
            .bind(run.branch.as_deref())
            .bind(&run.detail)
            // The event's instant, so the row and the record agree.
            .bind(event.timestamp)
            .execute(&mut *tx)
            .await
            .map_err(storage)?
            .rows_affected()
            == 1;

        if inserted {
            // Only a real insert records a fact. A collapsed retry
            // publishes nothing — the same replay guard `PgJobs` uses,
            // for the same reason: a re-reported run must not put a
            // second event on the log.
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(AgentRunError::Storage)?;
        }

        let row = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs WHERE run_id = $1"
        ))
        .bind(&run.run_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let recorded = row_to_run(&row)?;

        tx.commit().await.map_err(storage)?;

        Ok(RecordedRun {
            recorded: inserted,
            run: recorded,
        })
    }

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<AgentRun>, AgentRunError> {
        // Bound parameters only — every filter is a placeholder, never
        // interpolated text. The NULL-or-equal idiom keeps one
        // statement instead of four assembled variants.
        let since: Option<DateTime<Utc>> = filter.since;
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs \
             WHERE ($1::uuid IS NULL OR job_id = $1) \
               AND ($2::text IS NULL OR branch = $2) \
               AND ($3::text IS NULL OR actor_id = $3) \
               AND ($4::timestamptz IS NULL OR finished_at >= $4) \
             ORDER BY finished_at DESC, run_id ASC LIMIT $5"
        ))
        .bind(filter.job_id)
        .bind(filter.branch.as_deref())
        .bind(filter.actor_id.as_deref())
        .bind(since)
        .bind(filter.limit.unwrap_or(200).clamp(1, 1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(row_to_run).collect()
    }

    async fn rate_card(&self) -> Result<Vec<RateCardRow>, AgentRunError> {
        let rows = sqlx::query(
            "SELECT model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, note \
             FROM agent_rate_card ORDER BY model",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(card_row).collect()
    }
}
