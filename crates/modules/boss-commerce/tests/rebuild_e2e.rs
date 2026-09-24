//! End-to-end: drive commerce writes through PgCommerce (which
//! records each invoice event on the transactional OUTBOX inside the
//! domain tx — phase 2), drain the outbox through the relay pipeline
//! into `audit_log`, snapshot invoices + invoice_line_items, drop,
//! rebuild from `audit_log`, assert match. This is the REAL delivery
//! path — no hand-emitted events.

use std::sync::Arc;

use boss_commerce::PgCommerce;
use boss_commerce::rebuild_commerce;
use boss_commerce::types::*;
use boss_core::port::EventBus;
use boss_core::publisher::EventStamp;
use boss_events::outbox::drain_outbox_once;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use boss_commerce::CommerceRepository;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct InvoiceRow {
    id: String,
    account_id: String,
    issued_on: NaiveDate,
    due_on: NaiveDate,
    paid_on: Option<NaiveDate>,
    status: String,
    amount_cents: i64,
    currency: String,
    tax_cents: i64,
    tax_jurisdiction: Option<String>,
    payment_method: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct LineRow {
    id: String,
    invoice_id: String,
    revenue_category: String,
    amount_cents: i64,
    currency: String,
    description: String,
    ref_id: Option<String>,
    created_at: DateTime<Utc>,
}

async fn snapshot_invoices(pool: &PgPool) -> Vec<InvoiceRow> {
    sqlx::query_as("SELECT id, account_id, issued_on, due_on, paid_on, status, amount_cents, currency, tax_cents, tax_jurisdiction, payment_method, created_at FROM invoices ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_lines(pool: &PgPool) -> Vec<LineRow> {
    sqlx::query_as("SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, created_at FROM invoice_line_items ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

fn fixture(id: &str, account: &str, lines: Vec<(RevenueCategory, i64, &str)>) -> Invoice {
    let total: i64 = lines.iter().map(|(_, c, _)| *c).sum();
    Invoice {
        id: id.into(),
        account_id: account.into(),
        issued_on: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: total,
        currency: "USD".into(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: lines
            .into_iter()
            .enumerate()
            .map(|(i, (cat, cents, desc))| InvoiceLineItem {
                id: format!("{id}-line-{i}"),
                invoice_id: id.into(),
                revenue_category: cat,
                amount_cents: cents,
                currency: "USD".into(),
                description: desc.into(),
                ref_id: None,
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            })
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_invoices_and_line_items() {
    let db = TestDb::new().await;
    let commerce = Arc::new(PgCommerce::new(db.pool.clone()));
    let actor = boss_core::actor::ActorId::Automation("test".into());

    let now = Utc::now();
    let i1 = fixture(
        "INV-001",
        "acc-001",
        vec![
            (RevenueCategory::from("taproom"), 5_000, "tune-up"),
            (
                RevenueCategory::from("event-package"),
                3_000,
                "spare cartridge",
            ),
        ],
    );
    let i2 = fixture(
        "INV-002",
        "acc-002",
        vec![(
            RevenueCategory::from("distribution"),
            12_000,
            "annual support",
        )],
    );

    // Drive both creates through the repo alone — the adapter records
    // each event on the outbox INSIDE its transaction (phase 2); no
    // hand-emitted events anywhere in this test.
    commerce
        .create_invoice_at(
            &i1,
            now,
            &EventStamp::new("commerce", actor.clone()).with_timestamp(now),
        )
        .await
        .unwrap();

    let now2 = Utc::now();
    commerce
        .create_invoice_at(
            &i2,
            now2,
            &EventStamp::new("commerce", actor.clone()).with_timestamp(now2),
        )
        .await
        .unwrap();

    // Mark INV-001 paid — the paid event records in the flip's tx.
    let now3 = Utc::now();
    let paid_on = now3.date_naive();
    commerce
        .mark_invoice_paid_at(
            &i1.id,
            paid_on,
            &EventStamp::new("commerce", actor).with_timestamp(now3),
        )
        .await
        .unwrap();

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered. THIS is what the
    // rebuild below reads.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert_eq!(stats.delivered, 3, "2 created + 1 paid via the outbox");

    // Snapshot.
    let invoices_before = snapshot_invoices(&db.pool).await;
    let lines_before = snapshot_lines(&db.pool).await;
    assert_eq!(invoices_before.len(), 2);
    assert_eq!(lines_before.len(), 3, "INV-001:2 + INV-002:1");
    assert_eq!(
        invoices_before
            .iter()
            .find(|i| i.id == "INV-001")
            .unwrap()
            .status,
        "paid"
    );

    // Wipe + rebuild.
    let report = rebuild_commerce(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.invoices_upserted, 3, "2 created + 1 paid");

    // Reconstructed projection must match exactly.
    let invoices_after = snapshot_invoices(&db.pool).await;
    let lines_after = snapshot_lines(&db.pool).await;
    assert_eq!(invoices_before, invoices_after, "invoices mismatch");
    assert_eq!(lines_before, lines_after, "invoice_line_items mismatch");
}
