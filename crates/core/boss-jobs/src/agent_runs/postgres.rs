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
use boss_core::agent::{AgentCaps, BudgetDecision};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{
    AgentRunError, AgentRunLog, RecordedRun, RegisteredAgent, admit, resolve_model, validate,
    validate_profile, validate_window,
};
use super::profile::{RunProfile, WorkProfile};
use super::types::{
    ADMISSION_WINDOW, AgentRun, NewAgentRun, RateCardRow, RunFilter, RunOutcome, TokenUsage,
    measure_load, price_run,
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
pub(super) const RUN_COLUMNS: &str = "run_id, actor_id, model, started_at, finished_at, outcome, \
     error, total_tokens, input_tokens, output_tokens, tool_calls, usd_micros, priced_by, job_id, \
     branch, detail, budget, recorded_at, cache_read_tokens, cache_write_tokens";

/// What a READ selects from. `agent_runs` is what a WRITE inserts
/// into, and the two are deliberately different relations: the view
/// (20260922051115) presents a pre-cutover `total_tokens = 0` carrying
/// `detail.tokens_reported: false` as NULL, because that is what the
/// zero meant before the column could say it. The row itself is
/// untouched — insert-once holds, and a rebuild from the log puts the
/// same zeros back — so the reinterpretation has to live where the
/// reading happens, in ONE relation rather than in every reader's
/// memory of a cutover date (backlog f19589ac).
pub(super) const READ_RELATION: &str = "agent_runs_read";

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
    // Every one of the three columns is optional, and the three
    // legal combinations are `TokenUsage`'s three shapes: a split, a
    // bare total, or nothing at all — the row saying it holds no
    // count, which is what NULL means here and never zero (backlog
    // 65c9c05a). A row in no shape is a defect in whatever wrote it,
    // and naming the row is what makes it findable. The column is
    // wrapped in a second `Some` below because the ROW always states
    // it: `Some(None)` is "this row says NULL", which `from_parts`
    // reads as unreported, where a bare `None` would be a caller that
    // never mentioned tokens and is refused.
    let total_tokens: Option<i64> = row.try_get("total_tokens").map_err(storage)?;
    let input_tokens: Option<i64> = row.try_get("input_tokens").map_err(storage)?;
    let output_tokens: Option<i64> = row.try_get("output_tokens").map_err(storage)?;
    // A metered run's cache counts (backlog e6b2066f), NULL on every
    // other shape; the table's CHECKs hold them to both-or-neither.
    let cache_read: Option<i64> = row.try_get("cache_read_tokens").map_err(storage)?;
    let cache_write: Option<i64> = row.try_get("cache_write_tokens").map_err(storage)?;
    let run_id: String = row.try_get("run_id").map_err(storage)?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    let tokens = TokenUsage::from_wire(
        input_tokens.map(n),
        output_tokens.map(n),
        cache_read.map(n),
        cache_write.map(n),
        Some(total_tokens.map(n)),
    )
    .map_err(|e| {
        AgentRunError::Storage(format!(
            "agent_runs row {run_id:?} has unreadable tokens: {e}"
        ))
    })?;
    let tool_calls: i32 = row.try_get("tool_calls").map_err(storage)?;
    let usd_micros: Option<i64> = row.try_get("usd_micros").map_err(storage)?;
    // NULL on a row recorded before budgets were consulted: "no
    // decision was made", kept distinct from an allow with no cap.
    let budget = row
        .try_get::<Option<serde_json::Value>, _>("budget")
        .map_err(storage)?
        .map(serde_json::from_value::<BudgetDecision>)
        .transpose()
        .map_err(|e| {
            AgentRunError::Storage(format!(
                "agent_runs row {run_id:?} has an unreadable budget decision: {e}"
            ))
        })?;
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
            // The column as stored. NULL only on a row older than the
            // column whose actor id the backfill could not read; the
            // colon-form fallback in `NewAgentRun::model` still answers
            // for those.
            model: row.try_get("model").map_err(storage)?,
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
        budget,
        recorded_at: row.try_get("recorded_at").map_err(storage)?,
    })
}

/// The columns of an `agents` row the recorder reads. Both caps are
/// nullable there (20260915212644: NULL is "no cap declared") and stay
/// `None` here, which `admit` reads as unbudgeted.
fn agent_row(row: &sqlx::postgres::PgRow) -> Result<RegisteredAgent, AgentRunError> {
    let hourly: Option<i64> = row.try_get("hourly_budget_usd_micros").map_err(storage)?;
    let concurrent: Option<i32> = row.try_get("max_concurrent_runs").map_err(storage)?;
    Ok(RegisteredAgent {
        default_model: row.try_get("default_model").map_err(storage)?,
        caps: AgentCaps {
            // The table's CHECK refuses a negative, so the conversion
            // cannot fail on a row it admitted.
            hourly_budget_usd_micros: hourly.map(|v| u64::try_from(v).unwrap_or(0)),
            max_concurrent_runs: concurrent.map(|v| u32::try_from(v).unwrap_or(0)),
        },
    })
}

/// The rate card's columns, spelled once: the recorder reads the card
/// inside its transaction and the read endpoint reads it outside one,
/// and a list that lived twice would drift the next time a column is
/// added (CLAUDE.md §9a — `blended_input_share_ppm` was the next time).
const CARD_COLUMNS: &str = "model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, note, blended_input_share_ppm, \
     cache_read_usd_micros_per_mtok, cache_write_usd_micros_per_mtok";

fn card_row(row: &sqlx::postgres::PgRow) -> Result<RateCardRow, AgentRunError> {
    let input: i64 = row.try_get("input_usd_micros_per_mtok").map_err(storage)?;
    let output: i64 = row.try_get("output_usd_micros_per_mtok").map_err(storage)?;
    // NULL is "this model declares no ratio", which leaves a total-only
    // run unpriced — not a zero share, which would price every token at
    // the output rate.
    let share: Option<i64> = row.try_get("blended_input_share_ppm").map_err(storage)?;
    // NULL is "no cache rate declared", which leaves a metered run on
    // this model unpriced — never a zero rate, which would price the
    // cache reads that are most of the bill as free.
    let cache_read: Option<i64> = row
        .try_get("cache_read_usd_micros_per_mtok")
        .map_err(storage)?;
    let cache_write: Option<i64> = row
        .try_get("cache_write_usd_micros_per_mtok")
        .map_err(storage)?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(RateCardRow {
        model: row.try_get("model").map_err(storage)?,
        input_usd_micros_per_mtok: n(input),
        output_usd_micros_per_mtok: n(output),
        note: row.try_get("note").map_err(storage)?,
        blended_input_share_ppm: share.map(n),
        cache_read_usd_micros_per_mtok: cache_read.map(n),
        cache_write_usd_micros_per_mtok: cache_write.map(n),
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
        let card_rows = sqlx::query(&format!("SELECT {CARD_COLUMNS} FROM agent_rate_card"))
            .fetch_all(&mut *tx)
            .await
            .map_err(storage)?;
        let card = card_rows
            .iter()
            .map(card_row)
            .collect::<Result<Vec<_>, _>>()?;

        // The agent row — its default model and its caps — read in the
        // same transaction as the price so the row names the registry
        // it was resolved and admitted against. Only a registered id
        // has a row to read; the colon form says its own model, has no
        // caps, and the lookup is skipped.
        let agent: Option<RegisteredAgent> = match &run.actor_id {
            ActorId::RegisteredAgent(id) => sqlx::query(
                "SELECT default_model, hourly_budget_usd_micros, max_concurrent_runs \
                 FROM agents WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .map(|r| agent_row(&r))
            .transpose()?,
            _ => None,
        };
        // Resolved ONCE, and the run is recorded with it set: the row,
        // the event and the price all read the same word.
        let run = &NewAgentRun {
            model: Some(resolve_model(
                run,
                agent.as_ref().map(|a| a.default_model.as_str()),
            )?),
            ..run.clone()
        };
        let priced = price_run(&card, run);

        // Judge the run against its actor's budget and RECORD the
        // judgement (backlog e6b2066f): an over-cap run is a row whose
        // `budget` reads `deny`, never a run missing from the record.
        // The actor's rows since the window opened are the whole input
        // to the ONE load measure (`measure_load`): every row that
        // counts toward spend finished inside the window, and every row
        // in flight at the run's start finished after it, so one
        // bounded read holds both.
        let prior_rows = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM {READ_RELATION} WHERE actor_id = $1 AND finished_at >= $2"
        ))
        .bind(run.actor_id.to_string())
        .bind(ADMISSION_WINDOW.cutoff(run.started_at))
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let prior = prior_rows
            .iter()
            .map(row_to_run)
            .collect::<Result<Vec<_>, _>>()?;
        let budget = admit(agent.as_ref(), measure_load(&prior, run));

        let event = super::events::run_recorded_event(recorded_by, run, &priced, &budget);

        let inserted = sqlx::query(&insert_run_sql())
            .bind(&run.run_id)
            .bind(run.actor_id.to_string())
            .bind(run.model.as_deref())
            .bind(run.started_at)
            .bind(run.finished_at)
            .bind(run.outcome.as_str())
            .bind(run.error.as_deref())
            // The total when the run has one, NULL when it reported
            // none; the split goes in beside it only when it was
            // measured. The table's CHECKs refuse a pair that
            // disagrees and a split with no total, and the derivation
            // here is why neither can happen.
            .bind(
                run.tokens
                    .total()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
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
            .bind(serde_json::to_value(&budget).unwrap_or_default())
            // The event's instant, so the row and the record agree.
            .bind(event.timestamp)
            .bind(
                run.tokens
                    .cache_read()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
            .bind(
                run.tokens
                    .cache_write()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
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
            "SELECT {RUN_COLUMNS} FROM {READ_RELATION} WHERE run_id = $1"
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
            "SELECT {RUN_COLUMNS} FROM {READ_RELATION} \
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
        let rows = sqlx::query(&format!(
            "SELECT {CARD_COLUMNS} FROM agent_rate_card ORDER BY model"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(card_row).collect()
    }

    // The profile pair: `agent_run_profiles` (20260926050506), the row
    // its own record — no outbox, no event (see `super::profile`).

    async fn record_profile(
        &self,
        run_id: &str,
        profile: &WorkProfile,
        at: DateTime<Utc>,
    ) -> Result<RunProfile, AgentRunError> {
        validate_profile(run_id)?;
        let body = serde_json::to_value(profile)
            .map_err(|e| AgentRunError::BadRequest(format!("profile: {e}")))?;
        sqlx::query(
            "INSERT INTO agent_run_profiles (run_id, profile, recorded_at) VALUES ($1, $2, $3) \
             ON CONFLICT (run_id) DO UPDATE \
             SET profile = EXCLUDED.profile, recorded_at = EXCLUDED.recorded_at",
        )
        .bind(run_id)
        .bind(&body)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(RunProfile {
            run_id: run_id.to_string(),
            recorded_at: at,
            profile: profile.clone(),
        })
    }

    async fn list_profiles(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<RunProfile>, AgentRunError> {
        validate_window(since, until)?;
        let rows = sqlx::query(
            "SELECT run_id, profile, recorded_at FROM agent_run_profiles \
             WHERE recorded_at >= $1 AND recorded_at < $2 \
             ORDER BY recorded_at, run_id",
        )
        .bind(since)
        .bind(until)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|r| {
                let body: serde_json::Value = r.try_get("profile").map_err(storage)?;
                Ok(RunProfile {
                    run_id: r.try_get("run_id").map_err(storage)?,
                    recorded_at: r.try_get("recorded_at").map_err(storage)?,
                    // A row the type cannot read is a storage fault,
                    // named — never a default profile of zeroes.
                    profile: serde_json::from_value(body)
                        .map_err(|e| AgentRunError::Storage(format!("profile: {e}")))?,
                })
            })
            .collect()
    }
}
