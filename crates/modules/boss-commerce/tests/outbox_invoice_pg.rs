//! Outbox phase 2, the whole point in two tests:
//!
//! 1. An invoice create records its `commerce.invoice.created` event
//!    on the transactional outbox INSIDE the domain transaction —
//!    with the stamp's `_actor` enrichment — so state + provenance
//!    commit together and the relay (not the handler) delivers.
//! 2. An invoice referencing a GHOST account ABORTS the whole create:
//!    the subject_edges trigger (R2) fires on the outbox INSERT, in
//!    the domain transaction. No invoice row, no line items, no
//!    event. This is the 2026-07-13 phantom-account incident class
//!    closed at the source — the post-commit audit trigger could only
//!    punch provenance holes; this can refuse the state itself.

use boss_commerce::PgCommerce;
use boss_commerce::port::CommerceRepository;
use boss_commerce::types::*;
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};

fn fixture(id: &str, account: &str) -> Invoice {
    Invoice {
        id: id.into(),
        account_id: account.into(),
        issued_on: NaiveDate::from_ymd_opt(2026, 7, 21).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2026, 8, 20).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 5_000,
        currency: "USD".into(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.into(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 5_000,
            currency: "USD".into(),
            description: "keg".into(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}

fn stamp() -> EventStamp {
    EventStamp::new(
        "commerce",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(Utc::now())
}

/// TestDb disables ref-checks database-wide; this suite is ABOUT the
/// in-tx guard, so flip the database setting back on and open a
/// FRESH pool (ALTER DATABASE only reaches new sessions — the
/// original pool's connections predate it).
async fn guarded_pool(db: &TestDb) -> sqlx::PgPool {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        r#"ALTER DATABASE "{name}" SET audit_log.ref_check = 'on'"#
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_records_the_event_in_tx_and_ghost_account_aborts_everything() {
    let db = TestDb::new().await;
    let pool = guarded_pool(&db).await;
    let repo = PgCommerce::new(pool.clone());

    // A real account subject — the invoice→account edge resolves.
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ('account', 'acc-real')")
        .execute(&pool)
        .await
        .unwrap();

    repo.create_invoice_at(&fixture("INV-OK", "acc-real"), Utc::now(), &stamp())
        .await
        .expect("resolvable account must create");

    let (outbox, actor): (i64, Option<String>) = (
        sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE kind = 'commerce.invoice.created' \
             AND payload->>'id' = 'INV-OK'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        sqlx::query_scalar(
            "SELECT payload->>'_actor' FROM event_outbox \
             WHERE payload->>'id' = 'INV-OK' LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
    );
    assert_eq!(outbox, 1, "the event records on the outbox in-tx");
    assert_eq!(
        actor.as_deref(),
        Some("automation:test"),
        "the stamp's _actor enrichment rides the outbox payload"
    );

    // The ghost: subject_edges rejects the outbox INSERT inside the
    // domain tx → the WHOLE create aborts.
    let err = repo
        .create_invoice_at(&fixture("INV-GHOST", "acc-ghost"), Utc::now(), &stamp())
        .await;
    assert!(err.is_err(), "ghost account must abort the create");

    let (rows, lines, events): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM invoices WHERE id = 'INV-GHOST'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        sqlx::query_scalar(
            "SELECT count(*) FROM invoice_line_items WHERE invoice_id = 'INV-GHOST'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE payload->>'id' = 'INV-GHOST'")
            .fetch_one(&pool)
            .await
            .unwrap(),
    );
    assert_eq!(
        (rows, lines, events),
        (0, 0, 0),
        "no state, no lines, no event — the abort is total"
    );
}
