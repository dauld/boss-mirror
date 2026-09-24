//! End-to-end: a fact inserted directly produces a balanced journal entry
//! with lines resolved against the starter chart. Proves the full
//! projection round-trip without requiring a full commerce/inventory
//! HTTP setup.

use boss_ledger::{FactRef, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

/// Insert a fact row + post it. Combined so each test is one call.
async fn post_raw_fact(
    db: &TestDb,
    kind: &str,
    happened_on: NaiveDate,
    payload: &Value,
    source_id: &str,
) -> Uuid {
    let fact_id = Uuid::new_v4();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'invoices', $5, 'test')",
    )
    .bind(fact_id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let fact = FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    post_fact_in_tx(&mut tx, &fact).await.unwrap();
    tx.commit().await.unwrap();
    fact_id
}

async fn line_totals(db: &TestDb, entry_id: Uuid) -> (i64, i64) {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(debit_cents), 0)::bigint AS dtotal, \
                COALESCE(SUM(credit_cents), 0)::bigint AS ctotal \
         FROM gl_journal_lines WHERE journal_entry_id = $1",
    )
    .bind(entry_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (row.get("dtotal"), row.get("ctotal"))
}

#[tokio::test(flavor = "multi_thread")]
async fn invoice_issued_produces_balanced_entry() {
    let db = TestDb::new().await;
    let payload = json!({
        "invoice_id": "inv-e2e-1",
        "amount_cents": 12_000,
        "line_items": [{"category": "wholesale", "amount_cents": 12_000}],
    });
    let fact_id = post_raw_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &payload,
        "inv-e2e-1",
    )
    .await;

    let entry_id: Uuid = sqlx::query_scalar("SELECT id FROM gl_journal_entries WHERE fact_id = $1")
        .bind(fact_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let (debits, credits) = line_totals(&db, entry_id).await;
    assert_eq!(debits, 12_000i64);
    assert_eq!(credits, 12_000i64);

    // Per-line account check: the debit hits A/R (1100), the credit hits
    // revenue new sales (4100).
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, l.debit_cents, l.credit_cents FROM gl_journal_lines l \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE l.journal_entry_id = $1 ORDER BY l.sort_order",
    )
    .bind(entry_id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "1100");
    assert_eq!(rows[0].1, 12_000i64);
    assert_eq!(rows[1].0, "4100");
    assert_eq!(rows[1].2, 12_000i64);
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_category_invoice_splits_revenue_lines() {
    let db = TestDb::new().await;
    let payload = json!({
        "invoice_id": "inv-e2e-mix",
        "amount_cents": 15_000,
        "line_items": [
            {"category": "wholesale", "amount_cents": 10_000},
            {"category": "taproom", "amount_cents": 5_000},
        ],
    });
    let fact_id = post_raw_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &payload,
        "inv-e2e-mix",
    )
    .await;

    let entry_id: Uuid = sqlx::query_scalar("SELECT id FROM gl_journal_entries WHERE fact_id = $1")
        .bind(fact_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let line_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_lines WHERE journal_entry_id = $1")
            .bind(entry_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(line_count, 3, "one A/R debit + two revenue credits");

    let (debits, credits) = line_totals(&db, entry_id).await;
    assert_eq!(debits, 15_000i64);
    assert_eq!(credits, 15_000i64);
}

#[tokio::test(flavor = "multi_thread")]
async fn posting_the_same_fact_twice_is_idempotent() {
    let db = TestDb::new().await;
    let payload = json!({
        "invoice_id": "inv-e2e-idem",
        "amount_cents": 500,
        "line_items": [{"category": "taproom", "amount_cents": 500}],
    });
    let fact_id = Uuid::new_v4();
    let happened_on = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();

    // First transaction: insert fact + post
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, 'finance.invoice.issued', $2, $3, 'invoices', 'inv-e2e-idem', 'test')",
    )
    .bind(fact_id)
    .bind(happened_on)
    .bind(&payload)
    .execute(&mut *tx)
    .await
    .unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id: fact_id,
            kind: "finance.invoice.issued",
            happened_on,
            payload: &payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Second transaction: post the same fact again — should no-op
    let mut tx = db.pool.begin().await.unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id: fact_id,
            kind: "finance.invoice.issued",
            happened_on,
            payload: &payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let entry_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(entry_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_creates_monthly_period_for_new_month() {
    let db = TestDb::new().await;
    let payload = json!({
        "invoice_id": "inv-e2e-period",
        "amount_cents": 100,
        "line_items": [{"category": "taproom", "amount_cents": 100}],
    });
    post_raw_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(),
        &payload,
        "inv-e2e-period",
    )
    .await;

    let period: (NaiveDate, NaiveDate, String) = sqlx::query_as(
        "SELECT starts_on, ends_on, status FROM gl_periods \
         WHERE kind = 'month' AND starts_on = '2026-07-01'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(period.0, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
    assert_eq!(period.1, NaiveDate::from_ymd_opt(2026, 7, 31).unwrap());
    assert_eq!(period.2, "open");
}
