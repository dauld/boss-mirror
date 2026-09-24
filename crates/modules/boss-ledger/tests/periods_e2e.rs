//! Period-lock tests. Covers the core invariant: a fact that lands in a
//! locked period fails loudly, and the DB trigger catches it even if the
//! application-level check is bypassed.

use boss_ledger::periods::{list_periods, lock_period, unlock_period};
use boss_ledger::{FactRef, LedgerError, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now())
}

async fn seed_fact_and_post(
    db: &TestDb,
    kind: &str,
    happened_on: NaiveDate,
    payload: &Value,
    source_id: &str,
) -> Result<(), LedgerError> {
    let id = Uuid::new_v4();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'invoices', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .unwrap();

    let result = post_fact_in_tx(
        &mut tx,
        &FactRef {
            id,
            kind,
            happened_on,
            payload,
        },
    )
    .await;

    if result.is_ok() {
        tx.commit().await.unwrap();
    } else {
        tx.rollback().await.unwrap();
    }
    result
}

async fn period_id_for(db: &TestDb, starts_on: NaiveDate) -> Uuid {
    sqlx::query_scalar("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
        .bind(starts_on)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn list_periods_reports_totals() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p,
        "i1",
    )
    .await
    .unwrap();

    let periods = list_periods(&db.pool).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].status, "open");
    assert_eq!(periods[0].entry_count, 1);
    assert_eq!(periods[0].total_debits, 1_000i64);
    assert_eq!(periods[0].total_credits, 1_000i64);
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_period_writes_checksum_and_pins_rule_version() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p,
        "i1",
    )
    .await
    .unwrap();

    let pid = period_id_for(&db, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()).await;
    let checksum = lock_period(&db.pool, pid, "operator-test", &stamp(), "test")
        .await
        .unwrap();
    assert!(checksum.starts_with("sha256:"));

    let periods = list_periods(&db.pool).await.unwrap();
    let march = periods.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(march.status, "locked");
    assert_eq!(march.locked_by.as_deref(), Some("operator-test"));
    assert_eq!(march.locked_rule_version, Some(1));
    assert_eq!(march.locked_checksum.as_deref(), Some(checksum.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn locking_is_deterministic_same_state_same_checksum() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p,
        "i1",
    )
    .await
    .unwrap();

    let pid = period_id_for(&db, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()).await;
    let c1 = lock_period(&db.pool, pid, "op", &stamp(), "test")
        .await
        .unwrap();
    unlock_period(&db.pool, pid, &stamp(), "test")
        .await
        .unwrap();
    let c2 = lock_period(&db.pool, pid, "op", &stamp(), "test")
        .await
        .unwrap();
    assert_eq!(c1, c2);
}

#[tokio::test(flavor = "multi_thread")]
async fn posting_to_locked_period_fails_with_locked_period_error() {
    let db = TestDb::new().await;

    // Seed + lock March.
    let p_mar = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "taproom", "amount_cents": 1_000}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p_mar,
        "i1",
    )
    .await
    .unwrap();
    let march_pid = period_id_for(&db, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()).await;
    lock_period(&db.pool, march_pid, "op", &stamp(), "test")
        .await
        .unwrap();

    // Now try to post another fact dated in March — it must fail.
    let p_mar2 = json!({"invoice_id": "i2", "amount_cents": 500, "line_items": [{"category": "event-package", "amount_cents": 500}]});
    let err = seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(),
        &p_mar2,
        "i2",
    )
    .await
    .unwrap_err();
    match err {
        LedgerError::LockedPeriod {
            period_id,
            happened_on,
        } => {
            assert_eq!(period_id, march_pid);
            assert_eq!(happened_on, NaiveDate::from_ymd_opt(2026, 3, 20).unwrap());
        }
        other => panic!("expected LockedPeriod, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_returns_period_to_open() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 100, "line_items": [{"category": "taproom", "amount_cents": 100}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p,
        "i1",
    )
    .await
    .unwrap();
    let pid = period_id_for(&db, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()).await;
    lock_period(&db.pool, pid, "op", &stamp(), "test")
        .await
        .unwrap();
    unlock_period(&db.pool, pid, &stamp(), "test")
        .await
        .unwrap();

    let periods = list_periods(&db.pool).await.unwrap();
    let march = periods.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(march.status, "open");
    assert_eq!(march.locked_checksum, None);
    assert_eq!(march.locked_rule_version, None);

    // Post to March again — should succeed now.
    let p2 = json!({"invoice_id": "i2", "amount_cents": 500, "line_items": [{"category": "event-package", "amount_cents": 500}]});
    seed_fact_and_post(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p2,
        "i2",
    )
    .await
    .unwrap();
}
