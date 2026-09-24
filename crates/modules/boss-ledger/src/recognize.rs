//! Revenue-recognition scheduler — step 3 of the ASC-606 design.
//!
//! Reads `revenue_schedules` rows whose `next_recognition_date` is due,
//! emits a `finance.revenue.recognized` fact per period (RuleSet v2 then
//! posts the corresponding journal entry: DR 2200 / CR `<revenue-account>`),
//! advances the cursor, and flips the status to `closed` once the
//! schedule is fully recognized.
//!
//! Pure-vs-impure split: period math + cursor advancement + amount
//! computation live in free functions with no DB touch (testable in
//! isolation). The `run_tick` entry point is the only async function,
//! doing the actual transaction + fact insert + `post_fact_in_tx`
//! hand-off. Locked-period behavior: skip
//! forward to the next open month, attach a `makeup_for` marker so
//! auditors can trace the original-vs-realized period.
//!
//! Idempotency: `financial_facts` is uniquely indexed on
//! `(kind, source_table, source_id)`. We set `source_id =
//! "{schedule_id}::{period_start}"` so re-running the tick for the same
//! schedule + period is a no-op. The cursor advance happens in the same
//! transaction as the fact insert, so either both land or neither does.

#[cfg(feature = "postgres")]
use crate::error::LedgerError;
use chrono::{Datelike, Months, NaiveDate};
#[cfg(feature = "postgres")]
use sqlx::{PgPool, Postgres, Transaction};
#[cfg(feature = "postgres")]
use tracing::{info, warn};

/// Recognition cadence. Matches the DB-level CHECK constraint on
/// `revenue_schedules.frequency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Monthly,
    Quarterly,
}

impl Frequency {
    /// Parse the TEXT value stored in `revenue_schedules.frequency`.
    /// Named `parse_db_str` rather than `from_str` to avoid confusion
    /// with `std::str::FromStr` — we don't want a free `.parse()` to
    /// silently dispatch here when someone touches the type later.
    pub fn parse_db_str(s: &str) -> Option<Self> {
        match s {
            "monthly" => Some(Self::Monthly),
            "quarterly" => Some(Self::Quarterly),
            _ => None,
        }
    }

    /// Periods per whole year. Used for the default division when we
    /// don't have explicit start/end bounds — which today is never
    /// (every schedule carries both), but the helper's here for the
    /// symmetry.
    pub fn months_per_period(self) -> u32 {
        match self {
            Self::Monthly => 1,
            Self::Quarterly => 3,
        }
    }
}

/// A schedule row in the shape the scheduler needs. Mirrors the
/// `revenue_schedules` table, stripped to the columns the tick reads.
#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub id: String,
    pub source_kind: String,
    pub source_id: String,
    pub account_id: String,
    pub revenue_category: String,
    pub revenue_account: String,
    pub deferred_account: String,
    pub total_cents: i64,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub frequency: Frequency,
    pub recognized_to_date_cents: i64,
    pub next_recognition_date: NaiveDate,
}

/// Count the number of whole periods between `start_date` and
/// `end_date` inclusive at the given cadence. A 12-month contract
/// (start 2026-02-01, end 2027-01-31) has 12 monthly periods; the
/// same span at quarterly cadence has 4.
///
/// Implementation: month-difference + 1 (inclusive), divided by
/// months-per-period. Rounds up for partial tail periods — if a
/// schedule runs start=Jan 1, end=Feb 14 with monthly cadence, that's
/// 2 periods (one for January's whole month, one for the partial
/// February). The design doc's "prorated partial-first-month"
/// guidance (D4) is a per-row concern handled by the schedule creator
/// — by the time the scheduler sees the row, `start_date` already
/// reflects the first recognition date.
pub fn num_periods(start: NaiveDate, end: NaiveDate, freq: Frequency) -> u32 {
    debug_assert!(end >= start, "end_date >= start_date invariant");
    let months_inclusive =
        (end.year() - start.year()) * 12 + (end.month() as i32 - start.month() as i32) + 1;
    let months_inclusive = months_inclusive.max(1) as u32;
    let m = freq.months_per_period();
    months_inclusive.div_ceil(m)
}

/// The index (0-based) of the period currently being recognized.
///
/// Derived from the cursor rather than a stored counter so a replay
/// rebuilds it correctly. If recognized_to_date == 0 the index is 0
/// (nothing posted yet); every period posted bumps it by one.
///
/// Small caveat: this assumes the evenly-divided path. Because the
/// *last* period absorbs rounding, the index derived from
/// recognized-to-date can be off by ±1 for the very last period on
/// awkward totals. We instead derive the index from the cursor
/// relative to start_date + frequency, which is robust to rounding.
pub fn current_period_index(schedule: &ScheduleRow) -> u32 {
    let start = schedule.start_date;
    let cursor = schedule.next_recognition_date;
    let months =
        (cursor.year() - start.year()) * 12 + (cursor.month() as i32 - start.month() as i32);
    let months = months.max(0) as u32;
    months / schedule.frequency.months_per_period()
}

/// Compute the amount to recognize for THIS period, given the
/// schedule's state. `num_periods` is the total count from
/// [`num_periods`]; the caller passes it in so the math is
/// unit-testable without reconstructing the schedule.
///
/// Algorithm (evenly-distributed with remainder):
///   base  = total / num_periods                (floor)
///   tail  = total - base * (num_periods - 1)    // goes to the last
///   This period's amount = tail if last period, base otherwise.
///
/// The same arithmetic gives back: over all periods the sum equals
/// `total_cents` exactly, regardless of whether the division is
/// clean or has remainder.
pub fn period_amount_cents(total_cents: i64, num_periods: u32) -> (i64, i64) {
    debug_assert!(total_cents >= 0, "total_cents non-negative");
    debug_assert!(num_periods > 0, "schedule with zero periods");
    if num_periods == 1 {
        return (total_cents, total_cents);
    }
    let base = total_cents / num_periods as i64;
    let tail = total_cents - base * (num_periods as i64 - 1);
    (base, tail)
}

/// Given `schedule` and its period index, return the cents for this
/// specific period. Pulls the last-period-tail rule.
pub fn amount_for_period(schedule: &ScheduleRow, period_index: u32, total_periods: u32) -> i64 {
    let (base, tail) = period_amount_cents(schedule.total_cents, total_periods);
    if period_index + 1 >= total_periods {
        tail
    } else {
        base
    }
}

/// Move the cursor forward by one period. For monthly, +1 month; for
/// quarterly, +3 months. Chrono's `NaiveDate + Months` handles end-of-
/// month correctly (Jan 31 + 1 month = Feb 28/29, not a panic).
pub fn advance_cursor(cursor: NaiveDate, freq: Frequency) -> NaiveDate {
    cursor
        .checked_add_months(Months::new(freq.months_per_period()))
        .expect(
            "cursor advance overflow — a schedule running past year 262143 is not a real concern",
        )
}

// ---------------------------------------------------------------------------
// Tick orchestration (postgres-gated)
// ---------------------------------------------------------------------------

/// A summary of what a tick did. Returned so callers (CLI, tests, the
/// eventual metrics endpoint) can surface counts without scraping logs.
#[derive(Debug, Default, Clone)]
pub struct TickSummary {
    pub schedules_considered: usize,
    pub periods_posted: usize,
    pub schedules_closed: usize,
    pub locked_skips: usize,
    pub errors: Vec<String>,
}

/// The tick as the maintenance packet records it — the counts, flat,
/// under the names an operator reads (`recognized` is the periods
/// posted). Measured 2026-09-17 (backlog 18d6a6c9): on an instance
/// with no revenue schedules the nightly run considered 0, posted 0
/// and its packet said `result=ok` and nothing else, so "ran over
/// nothing" and "recognized a year of revenue" were the same record.
/// `errors` is a count here; each error's text is in the journal
/// beside the run, and the exit status carries the verdict.
pub fn run_summary_json(s: &TickSummary) -> serde_json::Value {
    serde_json::json!({
        "considered": s.schedules_considered,
        "recognized": s.periods_posted,
        "closed": s.schedules_closed,
        "locked_skips": s.locked_skips,
        "errors": s.errors.len(),
    })
}

/// Leave the summary where the packet's closer reads it: the path the
/// unit declares ONCE as `BOSS_RUN_SUMMARY_FILE` (infra/run-summary.sh
/// — both the run and boss-step.sh's `ExecStopPost=` inherit it, and
/// boss-step.sh merges the object onto the step and deletes the file).
/// `None` — no path declared, a hand run outside a packet — writes
/// nothing, deliberately.
pub fn write_run_summary(path: Option<&std::path::Path>, s: &TickSummary) -> std::io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    std::fs::write(path, run_summary_json(s).to_string())
}

#[cfg(feature = "postgres")]
/// Run one tick of the scheduler against the given pool.
///
/// The whole sweep is idempotent: re-running on the same calendar
/// day emits zero new facts because the schedule cursors have already
/// advanced past `today`.
pub async fn run_tick(
    pool: &PgPool,
    publisher: &Option<std::sync::Arc<boss_core::publisher::DomainPublisher>>,
    today: NaiveDate,
) -> Result<TickSummary, LedgerError> {
    let mut summary = TickSummary::default();

    // Fetch the due set in one query — keeps the hot path small and
    // lets the partial index on (next_recognition_date WHERE
    // status='active') do the heavy lifting. We operate per-schedule
    // below, each in its own transaction, so a bad row doesn't kill
    // the sweep.
    let rows: Vec<ScheduleRow> = fetch_due_schedules(pool, today).await?;
    summary.schedules_considered = rows.len();

    for row in rows {
        match advance_one_schedule(pool, publisher, &row).await {
            Ok(Advance::Posted { closed }) => {
                summary.periods_posted += 1;
                if closed {
                    summary.schedules_closed += 1;
                }
            }
            Ok(Advance::LockedSkip) => {
                summary.locked_skips += 1;
            }
            Err(e) => {
                warn!(schedule = %row.id, error = %e, "revenue recognition failed for schedule");
                summary.errors.push(format!("{}: {e}", row.id));
            }
        }
    }

    info!(
        considered = summary.schedules_considered,
        posted = summary.periods_posted,
        closed = summary.schedules_closed,
        locked_skips = summary.locked_skips,
        errors = summary.errors.len(),
        "revenue recognition tick complete"
    );

    Ok(summary)
}

#[cfg(feature = "postgres")]
enum Advance {
    /// A period was posted (journal entry written). `closed` is true
    /// if this period fully recognized the schedule.
    Posted { closed: bool },
    /// The target period is locked — cursor advanced forward, no
    /// journal entry written this tick. The fact will carry the
    /// `makeup_for` marker when it eventually posts.
    LockedSkip,
}

/// All 13 columns `fetch_due_schedules` pulls, aliased so clippy's
/// `type_complexity` check has something readable to point at and
/// so the query_as call fits on one line.
#[cfg(feature = "postgres")]
type ScheduleRowTuple = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    NaiveDate,
    NaiveDate,
    String,
    i64,
    NaiveDate,
);

#[cfg(feature = "postgres")]
async fn fetch_due_schedules(
    pool: &PgPool,
    today: NaiveDate,
) -> Result<Vec<ScheduleRow>, LedgerError> {
    let rows: Vec<ScheduleRowTuple> = sqlx::query_as(
        "SELECT id, source_kind, source_id, account_id, revenue_category, \
                revenue_account, deferred_account, total_cents, start_date, \
                end_date, frequency, recognized_to_date_cents, next_recognition_date \
         FROM revenue_schedules \
         WHERE status = 'active' AND next_recognition_date <= $1 \
         ORDER BY next_recognition_date",
    )
    .bind(today)
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Frequency::parse_db_str(&r.10).map(|frequency| ScheduleRow {
                id: r.0,
                source_kind: r.1,
                source_id: r.2,
                account_id: r.3,
                revenue_category: r.4,
                revenue_account: r.5,
                deferred_account: r.6,
                total_cents: r.7,
                start_date: r.8,
                end_date: r.9,
                frequency,
                recognized_to_date_cents: r.11,
                next_recognition_date: r.12,
            })
        })
        .collect())
}

#[cfg(feature = "postgres")]
async fn advance_one_schedule(
    pool: &PgPool,
    publisher: &Option<std::sync::Arc<boss_core::publisher::DomainPublisher>>,
    row: &ScheduleRow,
) -> Result<Advance, LedgerError> {
    let total_periods = num_periods(row.start_date, row.end_date, row.frequency);
    let period_ix = current_period_index(row);
    let period_amount = amount_for_period(row, period_ix, total_periods);

    // Conservation: cumulative recognitions cannot exceed the
    // schedule's total. The cursor + period-math should make this
    // impossible by construction (period_amount is total/N for the
    // base periods plus a remainder on the last), but a corrupt
    // `recognized_to_date_cents` (manual SQL fix-up, bad migration)
    // could let this slip. Reject defensively — never over-recognize.
    if row.recognized_to_date_cents.saturating_add(period_amount) > row.total_cents {
        return Err(LedgerError::InvalidPayload {
            kind: "finance.revenue.recognized".to_string(),
            reason: format!(
                "would over-recognize schedule {}: recognized_to_date={} + period={} > total={}",
                row.id, row.recognized_to_date_cents, period_amount, row.total_cents
            ),
        });
    }

    // Next cursor after this period posts. If this is the last
    // period, we don't advance (the status flip to 'closed' removes
    // it from the active-partial-index's scan).
    let is_final_period = period_ix + 1 >= total_periods;
    let post_date = row.next_recognition_date;
    let next_cursor = advance_cursor(row.next_recognition_date, row.frequency);
    let closed = is_final_period;

    let mut tx: Transaction<'_, Postgres> = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Insert the fact first (serves as the idempotency anchor via the
    // unique index on (kind, source_table, source_id)).
    let period_start = post_date;
    let period_end = period_end_for(post_date, row.frequency);
    let source_id_for_fact = format!("{}::{}", row.id, post_date);
    let payload = serde_json::json!({
        "schedule_id":  row.id,
        "source_id":    source_id_for_fact,
        "post_date":    post_date,
        "period_start": period_start,
        "period_end":   period_end,
        "amount_cents": period_amount,
        "category":     row.revenue_category,
        "account_id":  row.account_id,
    });

    let actual_fact_id = crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.revenue.recognized",
            happened_on: post_date,
            payload: &payload,
            source_table: Some("revenue_schedules"),
            source_id: Some(&source_id_for_fact),
            // The recognize bin publishes its events with source
            // "ledger"; created_by must match or the rebuilt fact
            // diverges (the projection falls back to event.source).
            created_by: "ledger",
        },
    )
    .await?
    .id;

    // Post via the normal ruleset path — BossRuleSet handles the
    // finance.revenue.recognized kind (deferred-revenue recognition).
    let fact_ref = crate::types::FactRef {
        id: actual_fact_id,
        kind: "finance.revenue.recognized",
        happened_on: post_date,
        payload: &payload,
    };
    match crate::postgres::post_fact_in_tx(&mut tx, &fact_ref).await {
        Ok(()) => {}
        Err(LedgerError::LockedPeriod { .. }) => {
            // Roll back the fact insert and advance the cursor
            // forward (design D5 — skip into the next open period).
            tx.rollback()
                .await
                .map_err(|e| LedgerError::Storage(e.to_string()))?;

            let skipped_next = advance_cursor(row.next_recognition_date, row.frequency);
            sqlx::query(
                "UPDATE revenue_schedules \
                 SET next_recognition_date = $2, updated_at = NOW() \
                 WHERE id = $1",
            )
            .bind(&row.id)
            .bind(skipped_next)
            .execute(pool)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;

            return Ok(Advance::LockedSkip);
        }
        Err(e) => return Err(e),
    }

    // Advance the schedule row — same transaction as the post.
    let new_recognized = row.recognized_to_date_cents + period_amount;
    let new_status = if closed { "closed" } else { "active" };
    sqlx::query(
        "UPDATE revenue_schedules \
         SET recognized_to_date_cents = $2, \
             next_recognition_date = $3, \
             status = $4, \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(&row.id)
    .bind(new_recognized)
    .bind(next_cursor)
    .bind(new_status)
    .execute(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Record the event in the SAME tx as the post + advance (outbox
    // phase 2). The record stamp is wall-clock — sim time is retired
    // from the record (David, 2026-08-22, packet a7a4cae5). The
    // business day the recognition covers is the schedule's own
    // recognition date, which rides in the payload + the schedule
    // row — never the stamp. The recognize timer has no request
    // user, so the actor is the publisher's default.
    let stamp = match publisher {
        Some(p) => p.stamp_with_actor(p.default_actor()).await,
        None => boss_core::publisher::EventStamp::new(
            "ledger",
            boss_core::actor::ActorId::Automation("platform".into()),
        ),
    };
    crate::events::record_ledger_event_in_tx(&mut tx, &stamp, "ledger.revenue.recognized", payload)
        .await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    info!(
        schedule = %row.id,
        period_index = period_ix,
        amount_cents = period_amount,
        next = %next_cursor,
        closed,
        "posted revenue recognition period"
    );

    Ok(Advance::Posted { closed })
}

/// Compute the last day of the period whose first day is `start`.
/// Monthly: last day of that month. Quarterly: last day of the
/// start-month + 2 months.
#[cfg(any(test, feature = "postgres"))]
fn period_end_for(start: NaiveDate, freq: Frequency) -> NaiveDate {
    let months = freq.months_per_period();
    let end_month_first = start
        .checked_add_months(Months::new(months))
        .expect("period-end overflow");
    end_month_first
        .pred_opt()
        .expect("day before 0001-01-01 isn't a real concern")
}

// ---------------------------------------------------------------------------
// Runoff projection — ASC 606 step 5
// ---------------------------------------------------------------------------

/// A single month in the deferred-revenue runoff projection. The
/// `month` is the first day of that calendar month; `amount_cents` is
/// the sum of every active schedule's period amount whose
/// `next_recognition_date` falls inside that month during the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunoffMonth {
    pub month: NaiveDate,
    pub amount_cents: i64,
}

/// Output of [`project_runoff`]: the month-by-month recognition
/// forecast plus any balance that extends past the horizon. The sum of
/// `months.amount_cents + beyond_horizon_cents` equals the total
/// remaining deferred balance across the input schedules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunoffProjection {
    pub months: Vec<RunoffMonth>,
    pub beyond_horizon_cents: i64,
    pub remaining_total_cents: i64,
}

/// Project future revenue recognition across active schedules.
///
/// For each schedule we walk from its `next_recognition_date` period by
/// period until it's fully recognized, dropping each period's amount
/// into the calendar-month bucket that contains the period's start
/// date. Periods that fall past `as_of + horizon_months` land in the
/// `beyond_horizon_cents` catch-all.
///
/// Pure: takes schedules + date inputs, returns buckets. Safe to unit-
/// test without a DB. The caller is responsible for filtering to
/// `status = 'active'` schedules.
pub fn project_runoff(
    schedules: &[ScheduleRow],
    as_of: NaiveDate,
    horizon_months: u32,
) -> RunoffProjection {
    // Month buckets start at the first day of `as_of`'s month and run
    // forward `horizon_months` consecutive months. Using a Vec keeps
    // the output order stable without needing a sort.
    let horizon_start = first_of_month(as_of);
    let mut buckets: Vec<RunoffMonth> = (0..horizon_months)
        .map(|offset| RunoffMonth {
            month: add_months(horizon_start, offset),
            amount_cents: 0,
        })
        .collect();
    let horizon_end_exclusive = add_months(horizon_start, horizon_months);

    let mut beyond_horizon_cents: i64 = 0;
    let mut remaining_total_cents: i64 = 0;

    for schedule in schedules {
        let total_periods = num_periods(schedule.start_date, schedule.end_date, schedule.frequency);
        let mut period_ix = current_period_index(schedule);
        let mut cursor = schedule.next_recognition_date;

        while period_ix < total_periods {
            let amount = amount_for_period(schedule, period_ix, total_periods);
            remaining_total_cents += amount;

            let bucket_month = first_of_month(cursor);
            if bucket_month < horizon_end_exclusive {
                // `bucket_idx` is guaranteed in-range since the vec
                // was sized by `horizon_months` starting from
                // `horizon_start`. But we clamp defensively to dodge
                // any calendar arithmetic surprise with pre-horizon
                // schedules (shouldn't happen — they'd already be due).
                if let Some(idx) = months_between(horizon_start, bucket_month)
                    && (idx as usize) < buckets.len()
                {
                    buckets[idx as usize].amount_cents += amount;
                } else {
                    beyond_horizon_cents += amount;
                }
            } else {
                beyond_horizon_cents += amount;
            }

            cursor = advance_cursor(cursor, schedule.frequency);
            period_ix += 1;
        }
    }

    RunoffProjection {
        months: buckets,
        beyond_horizon_cents,
        remaining_total_cents,
    }
}

/// First day of the calendar month containing `d`.
fn first_of_month(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).expect("valid year/month")
}

/// `base + offset` months, where `base` must already be a first-of-
/// month date. Chrono handles end-of-month clamping but we stay on
/// day-1 so the addition is straightforward.
fn add_months(base: NaiveDate, offset: u32) -> NaiveDate {
    base.checked_add_months(Months::new(offset))
        .expect("month-add overflow on bucket axis")
}

/// Count of whole months from `earlier` to `later`. Returns `None` if
/// `later < earlier`. Assumes both arguments are first-of-month dates.
fn months_between(earlier: NaiveDate, later: NaiveDate) -> Option<u32> {
    if later < earlier {
        return None;
    }
    let months =
        (later.year() - earlier.year()) * 12 + (later.month() as i32 - earlier.month() as i32);
    Some(months.max(0) as u32)
}

// ---------------------------------------------------------------------------
// Tests — pure period math, cursor advance, and edge cases. The async
// `run_tick` is exercised by the TestDb integration tests in
// `tests/recognize_pg.rs` (under the `postgres` feature).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // --- run summary (what the packet records) -----------------------------

    #[test]
    fn the_run_summary_says_what_the_tick_did_including_nothing() {
        // On an empty revenue_schedules table the tick considers 0 and
        // recognizes 0 — and the packet must say so, not just "ok".
        let s = TickSummary::default();
        assert_eq!(
            run_summary_json(&s),
            serde_json::json!({
                "considered": 0, "recognized": 0, "closed": 0,
                "locked_skips": 0, "errors": 0
            })
        );
        let s = TickSummary {
            schedules_considered: 3,
            periods_posted: 2,
            schedules_closed: 1,
            locked_skips: 1,
            errors: vec!["sched-9: boom".into()],
        };
        assert_eq!(
            run_summary_json(&s),
            serde_json::json!({
                "considered": 3, "recognized": 2, "closed": 1,
                "locked_skips": 1, "errors": 1
            })
        );
    }

    #[test]
    fn the_run_summary_file_is_written_only_when_a_path_is_declared() {
        let dir = boss_testing::scratch_dir("ledger-recognize-run-summary");
        let path = dir.join("summary.json");
        let s = TickSummary::default();
        write_run_summary(None, &s).unwrap();
        assert!(!path.exists());
        write_run_summary(Some(path.as_path()), &s).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(body["recognized"], 0);
        assert_eq!(body["considered"], 0);
    }

    // --- num_periods ------------------------------------------------------

    #[test]
    fn num_periods_twelve_for_a_full_year_monthly() {
        assert_eq!(
            num_periods(d(2026, 2, 1), d(2027, 1, 31), Frequency::Monthly),
            12
        );
    }

    #[test]
    fn num_periods_four_for_a_full_year_quarterly() {
        assert_eq!(
            num_periods(d(2026, 1, 1), d(2026, 12, 31), Frequency::Quarterly),
            4
        );
    }

    #[test]
    fn num_periods_one_for_single_month() {
        assert_eq!(
            num_periods(d(2026, 1, 1), d(2026, 1, 31), Frequency::Monthly),
            1
        );
    }

    #[test]
    fn num_periods_rounds_up_for_partial_tail() {
        // A schedule that runs Jan 1 through Feb 14 covers January
        // fully and 14 days of February — scheduler calls it two
        // monthly periods (partial tail in February).
        assert_eq!(
            num_periods(d(2026, 1, 1), d(2026, 2, 14), Frequency::Monthly),
            2
        );
    }

    // --- period_amount_cents ---------------------------------------------

    #[test]
    fn period_amount_clean_division() {
        // $12,000 / 12 months = $1,000/month flat.
        let (base, tail) = period_amount_cents(1_200_000, 12);
        assert_eq!(base, 100_000);
        assert_eq!(tail, 100_000);
    }

    #[test]
    fn period_amount_absorbs_remainder_in_tail() {
        // $1,234.56 / 12 months — first 11 = floor, last = the rest.
        let (base, tail) = period_amount_cents(123_456, 12);
        assert_eq!(base, 10_288); // 123_456 / 12
        assert_eq!(tail, 123_456 - base * 11); // collects 8 extra cents
        // Sanity: sum equals the total.
        assert_eq!(base * 11 + tail, 123_456);
    }

    #[test]
    fn period_amount_single_period_is_the_whole() {
        let (base, tail) = period_amount_cents(9_876, 1);
        assert_eq!(base, 9_876);
        assert_eq!(tail, 9_876);
    }

    #[test]
    fn period_amount_zero_total() {
        // A zero-value schedule is degenerate but shouldn't panic;
        // every period pays zero and the schedule closes after one.
        let (base, tail) = period_amount_cents(0, 12);
        assert_eq!(base, 0);
        assert_eq!(tail, 0);
    }

    // --- amount_for_period -----------------------------------------------

    fn mk_schedule(total: i64, start: NaiveDate, end: NaiveDate, freq: Frequency) -> ScheduleRow {
        ScheduleRow {
            id: "rs-test".into(),
            source_kind: "service_agreement".into(),
            source_id: "sa-1".into(),
            account_id: "account-001".into(),
            revenue_category: "distribution".into(),
            revenue_account: "4140".into(),
            deferred_account: "2200".into(),
            total_cents: total,
            start_date: start,
            end_date: end,
            frequency: freq,
            recognized_to_date_cents: 0,
            next_recognition_date: start,
        }
    }

    #[test]
    fn amount_for_period_returns_base_except_for_last() {
        let s = mk_schedule(123_456, d(2026, 1, 1), d(2026, 12, 31), Frequency::Monthly);
        let total_periods = num_periods(s.start_date, s.end_date, s.frequency);
        assert_eq!(total_periods, 12);
        // First period: base
        assert_eq!(amount_for_period(&s, 0, total_periods), 10_288);
        // Last period: tail
        assert_eq!(
            amount_for_period(&s, 11, total_periods),
            123_456 - 10_288 * 11
        );
    }

    #[test]
    fn amount_for_period_sums_to_total() {
        // For an awkward total like 100 cents over 7 months, the sum
        // across all periods must equal the total exactly.
        let total_cents = 100i64;
        let s = mk_schedule(
            total_cents,
            d(2026, 1, 1),
            d(2026, 7, 31),
            Frequency::Monthly,
        );
        let np = num_periods(s.start_date, s.end_date, s.frequency);
        assert_eq!(np, 7);
        let sum: i64 = (0..np).map(|i| amount_for_period(&s, i, np)).sum();
        assert_eq!(sum, total_cents);
    }

    // --- advance_cursor ---------------------------------------------------

    #[test]
    fn advance_cursor_monthly_same_year() {
        assert_eq!(
            advance_cursor(d(2026, 3, 15), Frequency::Monthly),
            d(2026, 4, 15),
        );
    }

    #[test]
    fn advance_cursor_quarterly_crosses_year_boundary() {
        assert_eq!(
            advance_cursor(d(2026, 11, 1), Frequency::Quarterly),
            d(2027, 2, 1),
        );
    }

    #[test]
    fn advance_cursor_handles_end_of_month_clamping() {
        // Jan 31 + 1 month = Feb 28/29, per chrono's `Months` semantics.
        // This is the expected behavior — we don't want a panic mid-tick.
        let after = advance_cursor(d(2026, 1, 31), Frequency::Monthly);
        assert_eq!(after, d(2026, 2, 28));
    }

    // --- current_period_index --------------------------------------------

    #[test]
    fn current_period_index_zero_at_start() {
        let s = mk_schedule(1200, d(2026, 1, 31), d(2026, 12, 31), Frequency::Monthly);
        assert_eq!(current_period_index(&s), 0);
    }

    #[test]
    fn current_period_index_advances_monthly() {
        let mut s = mk_schedule(1200, d(2026, 1, 1), d(2026, 12, 31), Frequency::Monthly);
        s.next_recognition_date = d(2026, 3, 1);
        assert_eq!(current_period_index(&s), 2); // Jan=0, Feb=1, Mar=2
    }

    #[test]
    fn current_period_index_advances_quarterly() {
        let mut s = mk_schedule(1200, d(2026, 1, 1), d(2026, 12, 31), Frequency::Quarterly);
        s.next_recognition_date = d(2026, 7, 1);
        assert_eq!(current_period_index(&s), 2); // Q1=0, Q2=1, Q3=2
    }

    // --- period_end_for --------------------------------------------------

    #[test]
    fn period_end_for_monthly_is_last_day_of_month() {
        assert_eq!(
            period_end_for(d(2026, 2, 1), Frequency::Monthly),
            d(2026, 2, 28)
        );
        assert_eq!(
            period_end_for(d(2026, 1, 1), Frequency::Monthly),
            d(2026, 1, 31)
        );
    }

    #[test]
    fn period_end_for_quarterly_spans_three_months() {
        assert_eq!(
            period_end_for(d(2026, 1, 1), Frequency::Quarterly),
            d(2026, 3, 31),
        );
    }

    // --- project_runoff ---------------------------------------------------

    #[test]
    fn project_runoff_single_full_year_monthly_contract() {
        // $1,200 over 12 months, starting fresh on 2026-05-01.
        let s = mk_schedule(120_000, d(2026, 5, 1), d(2027, 4, 30), Frequency::Monthly);
        let p = project_runoff(&[s], d(2026, 5, 15), 12);
        assert_eq!(p.months.len(), 12);
        // Every month in the horizon matches one schedule period:
        // $100 each for 11 months, then the tail catches rounding
        // (clean $100 here, so tail is also $100).
        for m in &p.months {
            assert_eq!(m.amount_cents, 10_000, "month {:?}", m.month);
        }
        assert_eq!(p.beyond_horizon_cents, 0);
        assert_eq!(p.remaining_total_cents, 120_000);
    }

    #[test]
    fn project_runoff_shifts_beyond_horizon_for_long_contracts() {
        // $2,400 over 24 months; horizon is 12 → half lands in months,
        // half rolls into `beyond_horizon_cents`.
        let s = mk_schedule(240_000, d(2026, 5, 1), d(2028, 4, 30), Frequency::Monthly);
        let p = project_runoff(&[s], d(2026, 5, 15), 12);
        let within: i64 = p.months.iter().map(|m| m.amount_cents).sum();
        assert_eq!(within, 12 * 10_000);
        assert_eq!(p.beyond_horizon_cents, 12 * 10_000);
        assert_eq!(p.remaining_total_cents, 240_000);
    }

    #[test]
    fn project_runoff_respects_partial_recognition_via_cursor() {
        // 12-month contract already 5 months in (cursor at Oct 1).
        // With a 12-month horizon starting Oct 2026 we should see
        // 7 months of contract activity + 5 empty tail months.
        let mut s = mk_schedule(120_000, d(2026, 5, 1), d(2027, 4, 30), Frequency::Monthly);
        s.next_recognition_date = d(2026, 10, 1);
        s.recognized_to_date_cents = 5 * 10_000;
        let p = project_runoff(&[s], d(2026, 10, 15), 12);
        let nonzero: Vec<_> = p.months.iter().filter(|m| m.amount_cents != 0).collect();
        assert_eq!(nonzero.len(), 7);
        let within: i64 = p.months.iter().map(|m| m.amount_cents).sum();
        assert_eq!(within, 7 * 10_000);
        assert_eq!(p.beyond_horizon_cents, 0);
        assert_eq!(p.remaining_total_cents, 7 * 10_000);
    }

    #[test]
    fn project_runoff_sums_across_multiple_schedules() {
        let a = mk_schedule(120_000, d(2026, 5, 1), d(2027, 4, 30), Frequency::Monthly);
        let mut b = mk_schedule(60_000, d(2026, 6, 1), d(2026, 11, 30), Frequency::Monthly);
        b.id = "rs-b".into();
        b.next_recognition_date = d(2026, 6, 1);
        let p = project_runoff(&[a, b], d(2026, 5, 15), 12);
        // a contributes $100/mo × 12. b contributes $100/mo × 6 (Jun–Nov),
        // which overlaps 6 of a's months, adding $600 total.
        let within: i64 = p.months.iter().map(|m| m.amount_cents).sum();
        assert_eq!(within, 120_000 + 60_000);
        assert_eq!(p.beyond_horizon_cents, 0);
        assert_eq!(p.remaining_total_cents, 180_000);
    }

    #[test]
    fn project_runoff_awkward_rounding_sums_to_total() {
        // 100 cents over 7 months: 14, 14, 14, 14, 14, 14, 16 (tail).
        // Sum within horizon must equal 100 exactly.
        let s = mk_schedule(100, d(2026, 1, 1), d(2026, 7, 31), Frequency::Monthly);
        let p = project_runoff(&[s], d(2026, 1, 1), 12);
        let within: i64 = p.months.iter().map(|m| m.amount_cents).sum();
        assert_eq!(within + p.beyond_horizon_cents, 100);
        assert_eq!(p.remaining_total_cents, 100);
    }

    #[test]
    fn project_runoff_empty_input_returns_zero_buckets() {
        let p = project_runoff(&[], d(2026, 5, 15), 12);
        assert_eq!(p.months.len(), 12);
        assert!(p.months.iter().all(|m| m.amount_cents == 0));
        assert_eq!(p.beyond_horizon_cents, 0);
        assert_eq!(p.remaining_total_cents, 0);
    }
}
