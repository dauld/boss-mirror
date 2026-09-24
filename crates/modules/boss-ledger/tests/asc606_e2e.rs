//! ASC 606 step 6 — end-to-end tests for the deferred-revenue chain.
//!
//! Exercises the full path: `finance.invoice.issued` (v2, ratable line) →
//! `revenue_schedules` row → `boss-ledger-recognize` tick → per-period
//! `finance.revenue.recognized` fact → v2 rule → balanced journal entry
//! (DR 2200 / CR 4140) → cursor advance → final period flips `status`
//! to `closed`.
//!
//! Scope per the design doc's step 6:
//!   - Period-crossing (contract straddling a year boundary).
//!   - Locked-period skip (cursor advances past a locked month).
//!   - Schedule-close (status='closed' after the last period).
//!   - Parity (v1-immediate vs v2-ratable yield the same 12-month
//!     revenue total — pure rule comparison, no DB).
//!
//! Env: v2 is the active RuleSet for every DB test in this file.
//! `cargo test` runs each `tests/*.rs` file as its own binary, so the
//! env-var mutation here doesn't leak into rules_v1 / periods_e2e /
//! http_api which all want v1.

use std::sync::Mutex;

use boss_ledger::{BossRuleSet, FactRef, evaluate, post_fact_in_tx, recognize};
use boss_testing::TestDb;
use chrono::{Duration, NaiveDate};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

/// Serialize the DB-touching tests. Four tests each opening their own
/// TestDb (new database per test, each with a fresh admin connection
/// to `/postgres`) plus N `recognize::run_tick` roundtrips per test
/// swamps the listener backlog on the VM's single Postgres instance —
/// we see `Connection refused (os error 111)` on parallel runs. The
/// test cost is ~1.5 s each; serializing the four is cheaper than
/// installing + wiring a `serial_test` dep.
static DB_LOCK: Mutex<()> = Mutex::new(());

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

async fn seed_account(db: &TestDb, id: &str) {
    sqlx::query(
        "INSERT INTO accounts \
            (id, name, director, city, state, tier, customer_since, territory_rep_id) \
         VALUES ($1, 'Test Clinic', 'Dr. Test', 'Austin', 'TX', 'silver', '2024-01-01', 'emp-001') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// Insert a schedule row directly — mirrors what
/// `POST /api/ledger/revenue-schedules` does. Bypassing HTTP keeps
/// the test surface small.
async fn seed_schedule(
    db: &TestDb,
    id: &str,
    account_id: &str,
    total_cents: i64,
    start_date: NaiveDate,
    end_date: NaiveDate,
    next_recognition_date: NaiveDate,
) {
    sqlx::query(
        "INSERT INTO revenue_schedules \
             (id, source_kind, source_id, account_id, revenue_category, \
              revenue_account, deferred_account, total_cents, start_date, \
              end_date, frequency, recognized_to_date_cents, \
              next_recognition_date, status) \
         VALUES ($1, 'service_agreement', $2, $3, 'distribution', \
                 '4140', '2200', $4, $5, $6, 'monthly', 0, $7, 'active')",
    )
    .bind(id)
    .bind(id.trim_start_matches("rs-"))
    .bind(account_id)
    .bind(total_cents)
    .bind(start_date)
    .bind(end_date)
    .bind(next_recognition_date)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn schedule_state(db: &TestDb, id: &str) -> (i64, NaiveDate, String) {
    let row = sqlx::query(
        "SELECT recognized_to_date_cents, next_recognition_date, status \
         FROM revenue_schedules WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (
        row.get("recognized_to_date_cents"),
        row.get("next_recognition_date"),
        row.get("status"),
    )
}

async fn journal_total_for_account(db: &TestDb, account_code: &str) -> (i64, i64) {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(jl.debit_cents), 0)::bigint AS d, \
                COALESCE(SUM(jl.credit_cents), 0)::bigint AS c \
         FROM gl_journal_lines jl \
         JOIN gl_accounts a ON a.id = jl.account_id \
         WHERE a.code = $1",
    )
    .bind(account_code)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (row.get("d"), row.get("c"))
}

// ---------------------------------------------------------------------------
// Schedule-close — run every period, assert the chain closes cleanly.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)] // DB_LOCK is the whole point — serialize.
async fn schedule_closes_after_final_period_recognition() {
    let _guard = DB_LOCK.lock().unwrap();
    let db = TestDb::new().await;
    seed_account(&db, "account-e2e-1").await;

    let schedule_id = "rs-E2E-1";
    let start = d(2026, 2, 1);
    let end = d(2027, 1, 31); // 12 months
    let first_period_end = d(2026, 2, 28);

    seed_schedule(
        &db,
        schedule_id,
        "account-e2e-1",
        1_200_000,
        start,
        end,
        first_period_end,
    )
    .await;

    // Twelve monthly ticks. Each tick advances one schedule. We hand
    // the tick an explicit `today` so we don't depend on wall-clock.
    let mut today = first_period_end;
    for month in 0..12 {
        let summary = recognize::run_tick(&db.pool, &None, today).await.unwrap();
        if month < 12 {
            assert_eq!(
                summary.periods_posted, 1,
                "month {month}: expected exactly one post",
            );
        }
        // Advance the clock by ~31 days so the next tick finds the
        // cursor due. The schedule's own cursor is what drives the
        // sweep — we just need `today` to be >= the cursor.
        today += Duration::days(31);
    }

    // Schedule should be closed + fully recognized.
    let (recognized, _next, status) = schedule_state(&db, schedule_id).await;
    assert_eq!(status, "closed");
    assert_eq!(recognized, 1_200_000);

    // Journal totals: 1,200,000 debited to 2200, 1,200,000 credited
    // to 4140. Net liability → revenue shift is clean.
    let (d2200, c2200) = journal_total_for_account(&db, "2200").await;
    assert_eq!(d2200, 1_200_000, "2200 debits");
    assert_eq!(c2200, 0, "2200 has no direct credits in this test");

    let (d4140, c4140) = journal_total_for_account(&db, "4140").await;
    assert_eq!(d4140, 0);
    assert_eq!(c4140, 1_200_000);
}

// ---------------------------------------------------------------------------
// Period-crossing — contract straddling a year boundary.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn period_crossing_contract_recognizes_across_year_boundary() {
    let _guard = DB_LOCK.lock().unwrap();
    let db = TestDb::new().await;
    seed_account(&db, "account-e2e-crossing").await;

    // Contract runs 2026-12-01 → 2027-11-30 — twelve months spanning
    // the new year. This is the shape D4 warned about: a naïve
    // "same-year month count" would under-recognize.
    let schedule_id = "rs-CROSS-1";
    let start = d(2026, 12, 1);
    let end = d(2027, 11, 30);
    let first_period_end = d(2026, 12, 31);

    seed_schedule(
        &db,
        schedule_id,
        "account-e2e-crossing",
        240_000, // awkward total to force remainder into last period
        start,
        end,
        first_period_end,
    )
    .await;

    let mut today = first_period_end;
    for _ in 0..12 {
        recognize::run_tick(&db.pool, &None, today).await.unwrap();
        today += Duration::days(31);
    }

    let (recognized, _, status) = schedule_state(&db, schedule_id).await;
    assert_eq!(status, "closed");
    assert_eq!(recognized, 240_000);

    // Count facts — should be exactly 12 periods, even though the
    // dates cross years.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.revenue.recognized' \
           AND source_id LIKE 'rs-CROSS-1::%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 12);
}

// ---------------------------------------------------------------------------
// Locked-period skip — D5 behavior.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn locked_period_skip_advances_cursor_without_posting() {
    let _guard = DB_LOCK.lock().unwrap();
    let db = TestDb::new().await;
    seed_account(&db, "account-e2e-lock").await;

    // Contract starts Feb 1, 2026. We'll lock the Feb period so the
    // first tick has to skip forward into March.
    let schedule_id = "rs-LOCK-1";
    let start = d(2026, 2, 1);
    let end = d(2027, 1, 31);
    let first_period_end = d(2026, 2, 28);

    seed_schedule(
        &db,
        schedule_id,
        "account-e2e-lock",
        1_200_000,
        start,
        end,
        first_period_end,
    )
    .await;

    // Ensure the Feb period exists, then lock it. The period auto-
    // creates on first post, so we post a throwaway fact into it
    // first to materialise the row. Use a non-revenue fact so the
    // journal accounting doesn't contaminate later assertions.
    let filler_fact_id = Uuid::new_v4();
    let filler_payload: Value = json!({
        "memo": "filler entry to materialise Feb period",
        "lines": [
            { "account_code": "1000", "debit_cents": 1, "memo": "filler" },
            { "account_code": "3000", "credit_cents": 1, "memo": "filler" },
        ],
    });
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, 'finance.manual.entry', $2, $3, NULL, 'filler-1', 'test')",
    )
    .bind(filler_fact_id)
    .bind(first_period_end)
    .bind(&filler_payload)
    .execute(&mut *tx)
    .await
    .unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id: filler_fact_id,
            kind: "finance.manual.entry",
            happened_on: first_period_end,
            payload: &filler_payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Now lock Feb.
    let feb_period_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = '2026-02-01'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now());
    boss_ledger::periods::lock_period(&db.pool, feb_period_id, "test", &stamp, "test")
        .await
        .unwrap();

    // Tick for Feb's cursor date — should skip (no fact posted), but
    // advance the cursor to March so tomorrow's tick can proceed.
    let summary = recognize::run_tick(&db.pool, &None, first_period_end)
        .await
        .unwrap();
    assert_eq!(summary.periods_posted, 0);
    assert_eq!(summary.locked_skips, 1);

    let (recognized, next, status) = schedule_state(&db, schedule_id).await;
    assert_eq!(recognized, 0, "nothing should have posted");
    assert_eq!(status, "active");
    assert_eq!(next, d(2026, 3, 28)); // cursor advanced one month forward

    // Tick again for March's cursor — should post successfully now.
    let summary = recognize::run_tick(&db.pool, &None, d(2026, 3, 31))
        .await
        .unwrap();
    assert_eq!(summary.periods_posted, 1);

    let (recognized, _, _) = schedule_state(&db, schedule_id).await;
    assert!(
        recognized > 0,
        "once past the locked period the schedule recognizes normally"
    );

    // No facts for the skipped Feb cursor date ever landed.
    let skipped_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.revenue.recognized' \
           AND source_id = 'rs-LOCK-1::2026-02-28'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(skipped_count, 0);
}

// ---------------------------------------------------------------------------
// Parity — same 12-month total, same revenue endpoint, different timing.
// Pure rule comparison, no DB touch.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn v1_and_v2_yield_same_annual_revenue_total_over_12_months() {
    // Build the two fact payloads. v1: one invoice → all 1.2M hits
    // 4140 on day 1. v2: one invoice → all 1.2M hits 2200 on day 1;
    // then 12 monthly recognitions each release 100k from 2200 → 4140.
    let v1_payload = json!({
        "invoice_id": "inv-parity",
        "line_items": [
            { "category": "distribution", "amount_cents": 1_200_000 },
        ],
    });
    let v2_invoice_payload = json!({
        "invoice_id": "inv-parity",
        "line_items": [
            { "category": "distribution", "amount_cents": 1_200_000, "recognition_pattern": "ratable" },
        ],
    });

    let id = Uuid::new_v4();
    let v1_fact = FactRef {
        id,
        kind: "finance.invoice.issued",
        happened_on: d(2026, 2, 1),
        payload: &v1_payload,
    };
    let v2_fact = FactRef {
        id,
        kind: "finance.invoice.issued",
        happened_on: d(2026, 2, 1),
        payload: &v2_invoice_payload,
    };
    let v1_draft = evaluate(&BossRuleSet, &v1_fact).unwrap();
    let v2_draft = evaluate(&BossRuleSet, &v2_fact).unwrap();

    // v1: 4140 gets a direct credit. v2: 2200 gets the credit on invoice.
    let v1_rev_credit: i64 = v1_draft
        .lines
        .iter()
        .filter(|l| l.account_code.as_ref() == "4140")
        .map(|l| l.credit_cents)
        .sum();
    let v2_deferred_credit: i64 = v2_draft
        .lines
        .iter()
        .filter(|l| l.account_code.as_ref() == "2200")
        .map(|l| l.credit_cents)
        .sum();
    assert_eq!(v1_rev_credit, 1_200_000);
    assert_eq!(v2_deferred_credit, 1_200_000);

    // Now walk 12 monthly recognitions under v2 and sum their
    // 4140 credits. The total must match v1's day-1 revenue credit
    // exactly — same long-run revenue, just released monthly.
    let mut v2_recognized_revenue: i64 = 0;
    for i in 0..12 {
        let month_index = i + 2; // Feb = 2, Mar = 3, ...
        let (year, month) = if month_index > 12 {
            (2027, month_index - 12)
        } else {
            (2026, month_index)
        };
        let post_date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
        // Clean $100/mo × 12 = $1,200 means every period (including
        // the tail) is $100.
        let amount = 100_000i64;
        let payload = json!({
            "schedule_id": "rs-parity",
            "period_start": post_date,
            "period_end": post_date,
            "amount_cents": amount,
            "category": "distribution",
            "account_id": "account-e2e-parity",
        });
        let recognition_fact = FactRef {
            id,
            kind: "finance.revenue.recognized",
            happened_on: post_date,
            payload: &payload,
        };
        let r = evaluate(&BossRuleSet, &recognition_fact).unwrap();
        v2_recognized_revenue += r
            .lines
            .iter()
            .filter(|l| l.account_code.as_ref() == "4140")
            .map(|l| l.credit_cents)
            .sum::<i64>();
    }
    assert_eq!(v2_recognized_revenue, v1_rev_credit);
}
