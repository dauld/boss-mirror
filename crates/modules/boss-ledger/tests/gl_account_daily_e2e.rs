//! The `gl_account_daily` convenience projection.
//!
//! Two invariants make it safe for the financial statements to read the
//! rollup instead of GROUP-BY-ing `gl_journal_lines` × `gl_journal_entries`
//! on every request:
//!   1. the live increment (in `post_fact_in_tx` → `insert_entry`) produces
//!      the SAME rollup as the rebuild's full TRUNCATE + re-aggregate, and
//!   2. that rollup equals a direct aggregation of the journal.

use std::collections::BTreeMap;

use boss_ledger::{FactRef, post_fact_in_tx, rebuild};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

type Key = (Uuid, NaiveDate);
type Roll = (i64, i64, i64); // (debit_cents, credit_cents, cash_flow_cents)

async fn insert_fact(
    db: &TestDb,
    kind: &str,
    on: NaiveDate,
    payload: &Value,
    source_id: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'invoices', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(on)
    .bind(payload)
    .bind(source_id)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn snapshot(db: &TestDb) -> BTreeMap<Key, Roll> {
    let rows: Vec<(Uuid, NaiveDate, i64, i64, i64)> = sqlx::query_as(
        "SELECT account_id, posted_on, debit_cents, credit_cents, cash_flow_cents \
         FROM gl_account_daily",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(a, d, deb, cr, cf)| ((a, d), (deb, cr, cf)))
        .collect()
}

/// Mixed facts: two invoices issued the same day (per-day aggregation),
/// an invoice paid (DR 1000 Cash / CR 1100 AR — a 2-line cash JE that
/// exercises the cash attribution), and a vendor bill (AP, no cash).
fn fixture() -> Vec<(&'static str, NaiveDate, Value, &'static str)> {
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    vec![
        (
            "finance.invoice.issued",
            d(2026, 3, 10),
            json!({"invoice_id": "i1", "amount_cents": 10_000, "line_items": [{"category": "taproom", "amount_cents": 10_000}]}),
            "i1",
        ),
        (
            "finance.invoice.issued",
            d(2026, 3, 10),
            json!({"invoice_id": "i2", "amount_cents": 2_500, "line_items": [{"category": "wholesale", "amount_cents": 2_500}]}),
            "i2",
        ),
        (
            "finance.invoice.paid",
            d(2026, 3, 20),
            json!({"invoice_id": "i1", "amount_cents": 10_000}),
            "i1",
        ),
        (
            "finance.bill.approved",
            d(2026, 3, 12),
            json!({"vendor_invoice_id": "v1", "lines": [{"part_sku": "INK-A", "qty": 2, "unit_cost_cents": 200}]}),
            "v1",
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn live_increment_matches_rebuild_reaggregate() {
    let db = TestDb::new().await;
    let facts = fixture();

    // Insert facts, then post each LIVE — post_fact_in_tx increments
    // gl_account_daily in the same tx, simulating the live write path.
    let mut posted: Vec<(Uuid, &'static str, NaiveDate, Value)> = Vec::new();
    for (kind, on, payload, source) in &facts {
        let id = insert_fact(&db, kind, *on, payload, source).await;
        posted.push((id, kind, *on, payload.clone()));
    }
    for (id, kind, on, payload) in &posted {
        let mut tx = db.pool.begin().await.unwrap();
        let fact = FactRef {
            id: *id,
            kind,
            happened_on: *on,
            payload,
        };
        post_fact_in_tx(&mut tx, &fact).await.unwrap();
        tx.commit().await.unwrap();
    }
    let live = snapshot(&db).await;
    assert!(
        !live.is_empty(),
        "live posting should have populated gl_account_daily"
    );

    // Rebuild re-derives gl_account_daily via TRUNCATE + full re-aggregate.
    rebuild(&db.pool).await.unwrap();
    let rebuilt = snapshot(&db).await;

    assert_eq!(
        live, rebuilt,
        "live increment diverged from the rebuild re-aggregate"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_rollup_matches_direct_aggregation() {
    let db = TestDb::new().await;
    for (kind, on, payload, source) in &fixture() {
        insert_fact(&db, kind, *on, payload, source).await;
    }
    rebuild(&db.pool).await.unwrap();

    // Debit/credit in the rollup must equal a direct GROUP BY of the lines.
    let direct: Vec<(Uuid, NaiveDate, i64, i64)> = sqlx::query_as(
        "SELECT l.account_id, e.posted_on, \
                SUM(l.debit_cents)::bigint, SUM(l.credit_cents)::bigint \
         FROM gl_journal_lines l JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         GROUP BY l.account_id, e.posted_on",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let roll = snapshot(&db).await;
    assert!(!direct.is_empty(), "fixture should produce journal lines");
    for (acc, day, deb, cr) in direct {
        let got = roll
            .get(&(acc, day))
            .unwrap_or_else(|| panic!("account-day {acc} {day} missing from rollup"));
        assert_eq!(
            (got.0, got.1),
            (deb, cr),
            "debit/credit mismatch {acc} {day}"
        );
    }

    // The invoice-paid entry (DR 1000 Cash / CR 1100 AR — a 2-line cash
    // JE) attributes the full $100.00 to its single offset, 1100 AR, on
    // the paid date. 2-line → exact, no rounding.
    let ar_cash: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(d.cash_flow_cents), 0)::bigint FROM gl_account_daily d \
         JOIN gl_accounts a ON a.id = d.account_id \
         WHERE a.code = '1100' AND d.posted_on = '2026-03-20'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        ar_cash, 10_000,
        "AR offset should carry the full cash received on the paid date"
    );
}

/// The whole point of the projection: a statement query reading
/// gl_account_daily must return byte-identical totals to the old query
/// that GROUP-BY'd gl_journal_lines × gl_journal_entries. Checks the
/// cumulative `as_of` shape (trial balance / balance sheet /
/// account-balances) at a covering date and a mid-window date.
#[tokio::test(flavor = "multi_thread")]
async fn statement_query_matches_direct_journal() {
    let db = TestDb::new().await;
    for (kind, on, payload, source) in &fixture() {
        insert_fact(&db, kind, *on, payload, source).await;
    }
    rebuild(&db.pool).await.unwrap();

    for as_of in ["2026-12-31", "2026-03-15"] {
        let direct: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT a.code, COALESCE(SUM(l.debit_cents),0)::bigint, \
                    COALESCE(SUM(l.credit_cents),0)::bigint \
             FROM gl_accounts a \
             LEFT JOIN gl_journal_lines l ON l.account_id = a.id \
             LEFT JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
             WHERE e.posted_on <= $1::date \
             GROUP BY a.code ORDER BY a.code",
        )
        .bind(as_of)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let projected: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT a.code, COALESCE(SUM(d.debit_cents),0)::bigint, \
                    COALESCE(SUM(d.credit_cents),0)::bigint \
             FROM gl_accounts a \
             LEFT JOIN gl_account_daily d ON d.account_id = a.id \
             WHERE d.posted_on <= $1::date \
             GROUP BY a.code ORDER BY a.code",
        )
        .bind(as_of)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            direct, projected,
            "projection-backed statement diverged from direct journal at as_of={as_of}"
        );
    }
}
