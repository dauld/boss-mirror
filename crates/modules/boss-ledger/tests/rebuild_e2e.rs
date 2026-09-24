//! Rebuild tests. Confirms the core flexibility claim of the design:
//! the projection can be dropped and recomputed from facts, deterministically,
//! without touching the underlying facts.

use boss_ledger::rebuild;
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

async fn insert_fact(
    db: &TestDb,
    kind: &str,
    happened_on: NaiveDate,
    payload: &Value,
    source_id: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'invoices', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_id)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn entry_count(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_projects_all_facts_from_empty_ledger() {
    let db = TestDb::new().await;

    // Seed 3 facts directly, no ledger posting yet.
    let p1 = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    let p2 = json!({"invoice_id": "i2", "amount_cents": 2_500, "line_items": [{"category": "wholesale", "amount_cents": 2_500}]});
    let p3 = json!({"invoice_id": "i1", "amount_cents": 1_000});
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p1,
        "i1",
    )
    .await;
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p2,
        "i2",
    )
    .await;
    insert_fact(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        &p3,
        "i1",
    )
    .await;

    assert_eq!(entry_count(&db).await, 0, "ledger starts empty");

    let report = rebuild(&db.pool).await.unwrap();
    assert_eq!(report.facts_processed, 3);
    assert_eq!(report.entries_created, 3);
    assert_eq!(report.entries_dropped, 0);
    assert!(report.is_balanced(), "{:?}", report);
    assert_eq!(entry_count(&db).await, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_is_idempotent() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 500, "line_items": [{"category": "taproom", "amount_cents": 500}]});
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p,
        "i1",
    )
    .await;

    // First rebuild: empty → 1 entry
    let r1 = rebuild(&db.pool).await.unwrap();
    assert_eq!(r1.facts_processed, 1);
    assert_eq!(r1.entries_created, 1);
    assert_eq!(r1.entries_dropped, 0);

    // Second rebuild: drops + recreates — byte-identical projection
    let r2 = rebuild(&db.pool).await.unwrap();
    assert_eq!(r2.facts_processed, 1);
    assert_eq!(r2.entries_created, 1);
    assert_eq!(r2.entries_dropped, 1);
    assert_eq!(entry_count(&db).await, 1, "still exactly one entry");
    assert_eq!(r2.total_debits, r1.total_debits);
    assert_eq!(r2.total_credits, r1.total_credits);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_preserves_trial_balance() {
    let db = TestDb::new().await;
    let p_issued = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    let p_paid = json!({"invoice_id": "i1", "amount_cents": 1_000});
    // bill_approved requires `lines` (no more lump-only fallback).
    let p_bill = json!({
        "vendor_invoice_id": "v1",
        "lines": [{ "part_sku": "INK-A", "qty": 2, "unit_cost_cents": 200 }],
    });

    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p_issued,
        "i1",
    )
    .await;
    insert_fact(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(),
        &p_paid,
        "i1",
    )
    .await;
    insert_fact(
        &db,
        "finance.bill.approved",
        NaiveDate::from_ymd_opt(2026, 3, 12).unwrap(),
        &p_bill,
        "v1",
    )
    .await;

    let report = rebuild(&db.pool).await.unwrap();
    assert!(report.is_balanced(), "totals: {:?}", report);
    assert_eq!(report.total_debits, 2_400i64);
    assert_eq!(report.total_credits, 2_400i64);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_skips_facts_in_locked_periods() {
    let db = TestDb::new().await;

    // Two facts: one in March (which we'll lock), one in April (open).
    let p_mar = json!({"invoice_id": "i-mar", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    let p_apr = json!({"invoice_id": "i-apr", "amount_cents": 2_000, "line_items": [{"category": "taproom", "amount_cents": 2_000}]});
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p_mar,
        "i-mar",
    )
    .await;
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
        &p_apr,
        "i-apr",
    )
    .await;

    // First rebuild projects both entries; March period auto-created.
    let r1 = rebuild(&db.pool).await.unwrap();
    assert_eq!(r1.entries_created, 2);

    // Lock the March period. Its entries must survive a subsequent rebuild.
    sqlx::query(
        "UPDATE gl_periods SET status = 'locked', locked_at = NOW(), \
         locked_rule_version_id = (SELECT id FROM gl_rule_versions WHERE version = 1) \
         WHERE starts_on = '2026-03-01'",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // Add a new April fact, rebuild. March should be untouched; only April
    // re-projects.
    let p_apr2 = json!({"invoice_id": "i-apr2", "amount_cents": 500, "line_items": [{"category": "event-package", "amount_cents": 500}]});
    insert_fact(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
        &p_apr2,
        "i-apr2",
    )
    .await;

    let r2 = rebuild(&db.pool).await.unwrap();
    // March fact should NOT have been re-projected (its period is locked).
    // April facts = 2 (original + new).
    assert_eq!(r2.facts_processed, 2);
    assert_eq!(r2.periods_rebuilt, 1, "only April's period is open");

    // Total entries = 1 (March, preserved) + 2 (April, rebuilt) = 3.
    assert_eq!(entry_count(&db).await, 3);
    assert!(r2.is_balanced());
}
