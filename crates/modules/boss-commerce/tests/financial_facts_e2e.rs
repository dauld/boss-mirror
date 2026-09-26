//! End-to-end tests for commerce → financial_facts — v1a of the GL track.
//!
//! Verifies the fact log invariant: every AR domain operation that happens
//! writes a matching `financial_facts` row in the same transaction. Backed
//! by a real Postgres via `TestDb`.

use boss_commerce::PgCommerce;
use boss_commerce::port::CommerceRepository;
use boss_commerce::types::*;
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::Value;
use sqlx::Row;

fn invoice(id: &str, status: InvoiceStatus, paid_on: Option<NaiveDate>) -> Invoice {
    Invoice {
        id: id.to_string(),
        account_id: "account-ff-1".to_string(),
        issued_on: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
        paid_on,
        status,
        amount_cents: 1_200_000,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 1_200_000,
            currency: "USD".to_string(),
            description: "Device sale".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}

async fn fact_count(db: &TestDb, kind: &str, source_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = $1 AND source_table = 'invoices' AND source_id = $2",
    )
    .bind(kind)
    .bind(source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn entry_count_for(db: &TestDb, source_id: &str, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.source_id = $1 AND f.kind = $2",
    )
    .bind(source_id)
    .bind(kind)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_invoice_emits_issued_fact() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    commerce
        .create_invoice(&invoice(
            "inv-ff-1",
            InvoiceStatus::OUTSTANDING.into(),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-ff-1").await,
        1
    );
    assert_eq!(fact_count(&db, "finance.invoice.paid", "inv-ff-1").await, 0);
    // v1b: the fact also produced a balanced journal entry.
    assert_eq!(
        entry_count_for(&db, "inv-ff-1", "finance.invoice.issued").await,
        1
    );

    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM financial_facts \
         WHERE source_id = 'inv-ff-1' AND kind = 'finance.invoice.issued'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    // The live fact now carries the full Invoice struct (same shape the
    // commerce.invoice.created event emits) so it rebuilds byte-identically:
    // top-level `id` (not `invoice_id`), line items keyed `revenue_category`.
    assert_eq!(payload["id"], "inv-ff-1");
    assert_eq!(payload["account_id"], "account-ff-1");
    assert_eq!(payload["amount_cents"], 1_200_000);
    assert_eq!(payload["currency"], "USD");
    assert_eq!(payload["line_items"][0]["revenue_category"], "wholesale");
    assert_eq!(payload["line_items"][0]["amount_cents"], 1_200_000);
}

/// The commerce-alignment invariant: the live in-tx `finance.invoice.issued`
/// fact must be byte-identical to the fact rebuilt from the
/// `commerce.invoice.created` event (which the publisher stamps with
/// `_actor`/`_simulated`). Drives the REAL create path, emits the event the
/// handler would (same `invoice_created_payload` off the same enriched
/// invoice) plus the envelope, rebuilds, and asserts the payloads match.
#[tokio::test(flavor = "multi_thread")]
async fn issued_fact_is_byte_identical_to_rebuild_from_event() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    let enriched = commerce
        .create_invoice(&invoice(
            "inv-parity-1",
            InvoiceStatus::OUTSTANDING.into(),
            None,
        ))
        .await
        .unwrap();

    // Snapshot the live fact payload before the rebuild's TRUNCATE wipes it.
    let live: Value = sqlx::query_scalar(
        "SELECT payload FROM financial_facts \
         WHERE source_id = 'inv-parity-1' AND kind = 'finance.invoice.issued'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    // The event exactly as the handler emits it, plus the publisher envelope.
    let mut event_payload = boss_commerce::events::invoice_created_payload(&enriched);
    event_payload["_actor"] = serde_json::json!("sim:workforce");
    event_payload["_simulated"] = serde_json::json!(true);
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), now(), 'commerce', 'commerce.invoice.created', $1)",
    )
    .bind(&event_payload)
    .execute(&db.pool)
    .await
    .unwrap();

    boss_ledger::rebuild_facts(&db.pool).await.unwrap();

    let rebuilt: Value = sqlx::query_scalar(
        "SELECT payload FROM financial_facts \
         WHERE source_id = 'inv-parity-1' AND kind = 'finance.invoice.issued'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        live, rebuilt,
        "live invoice.issued fact must equal the envelope-stripped rebuild from the event"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn create_paid_invoice_emits_both_facts() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    let paid_on = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
    commerce
        .create_invoice(&invoice(
            "inv-ff-2",
            InvoiceStatus::PAID.into(),
            Some(paid_on),
        ))
        .await
        .unwrap();

    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-ff-2").await,
        1
    );
    assert_eq!(fact_count(&db, "finance.invoice.paid", "inv-ff-2").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_invoice_paid_emits_no_paid_fact() {
    // 2026-05-06: mark_invoice_paid stopped emitting a
    // `finance.invoice.paid` fact. Tenants that model bank float
    // (the playground brewery + every UDS deploy) drain AR via
    // the two-phase `ledger.payment.received` /
    // `ledger.payment.settled` chain — that chain debits 1010
    // Cash-in-Transit and credits 1100 AR. Emitting
    // finance.invoice.paid here would ALSO credit AR (via
    // BossRuleSet::invoice_paid's DR Cash CR AR shortcut), and the
    // brewery hit exactly that — AR went structurally negative
    // -$6.5M because every payment got AR-credited twice. The
    // status flip + paid_on stamp still happens; the fact does
    // not. Tenants that explicitly want the single-shot path can
    // call `record_fact_in_tx` directly.
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    commerce
        .create_invoice(&invoice(
            "inv-ff-3",
            InvoiceStatus::OUTSTANDING.into(),
            None,
        ))
        .await
        .unwrap();
    commerce.mark_invoice_paid("inv-ff-3").await.unwrap();

    // The issued fact + entry stay (created by create_invoice).
    assert_eq!(
        entry_count_for(&db, "inv-ff-3", "finance.invoice.issued").await,
        1
    );
    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-ff-3").await,
        1
    );

    // mark_invoice_paid emits NO finance.invoice.paid fact
    // (the structural fix for the duplicate AR credit). The
    // status flip itself still happens — the row's status =
    // 'paid' and paid_on is stamped — verified via the
    // invoices table directly.
    assert_eq!(fact_count(&db, "finance.invoice.paid", "inv-ff-3").await, 0);
    assert_eq!(
        entry_count_for(&db, "inv-ff-3", "finance.invoice.paid").await,
        0
    );

    let row: (String, Option<NaiveDate>) =
        sqlx::query_as("SELECT status, paid_on FROM invoices WHERE id = 'inv-ff-3'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(row.0, "paid");
    assert!(row.1.is_some(), "paid_on must be stamped");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_is_idempotent() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    let inv = invoice("inv-ff-4", InvoiceStatus::OUTSTANDING.into(), None);
    commerce.create_invoice(&inv).await.unwrap();
    commerce.create_invoice(&inv).await.unwrap();
    commerce.create_invoice(&inv).await.unwrap();

    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-ff-4").await,
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_paid_then_replayed_create_does_not_duplicate() {
    // Replay of a paid-state create after an earlier mark_paid.
    // mark_invoice_paid no longer emits a fact (see the
    // mark_invoice_paid_emits_no_paid_fact test for the why). The
    // create-already-paid path emits one when the status arrives as
    // 'paid' and payment_method is unset (the legacy single-shot
    // tenant path) — but only on the create that WRITES the invoice.
    // A create under an id that already names the invoice writes
    // nothing (backlog 9d2af748): until then this replay posted a
    // finance.invoice.paid for a payment the verb had deliberately
    // left to the settlement chain — the A/R credit that verb's own
    // test says a two-phase tenant then takes twice. One issued fact,
    // no paid fact.
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    commerce
        .create_invoice(&invoice(
            "inv-ff-5",
            InvoiceStatus::OUTSTANDING.into(),
            None,
        ))
        .await
        .unwrap();
    commerce.mark_invoice_paid("inv-ff-5").await.unwrap();

    let paid_on = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
    commerce
        .create_invoice(&invoice(
            "inv-ff-5",
            InvoiceStatus::PAID.into(),
            Some(paid_on),
        ))
        .await
        .unwrap();

    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-ff-5").await,
        1
    );
    // None: the mark_invoice_paid call between the two creates emits
    // no finance.invoice.paid fact, and the replayed create writes
    // nothing at all.
    assert_eq!(fact_count(&db, "finance.invoice.paid", "inv-ff-5").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_paid_missing_invoice_writes_no_fact() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    let err = commerce.mark_invoice_paid("inv-does-not-exist").await;
    assert!(err.is_err());

    let total: i64 = sqlx::query("SELECT COUNT(*) FROM financial_facts")
        .fetch_one(&db.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(total, 0);
}

/// Seed a product + finished-goods inventory row so an FG invoice line
/// can draw it down.
async fn seed_fg(db: &TestDb, sku: &str, on_hand: i32, cost_cents: i64) {
    sqlx::query(
        "INSERT INTO products (sku, name, product_kind, package_unit) \
         VALUES ($1, $1, 'beer', '1/2-bbl-keg')",
    )
    .bind(sku)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO finished_product_inventory \
            (product_sku, location_id, on_hand, value_cents) \
         VALUES ($1, 'loc-fg-1', $2, $3::bigint * $2)",
    )
    .bind(sku)
    .bind(on_hand)
    .bind(cost_cents)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn fg_on_hand(db: &TestDb, sku: &str) -> i32 {
    sqlx::query_scalar("SELECT on_hand FROM finished_product_inventory WHERE product_sku = $1")
        .bind(sku)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// Q2 (inventory-value-conservation): the consume owns the FG
/// drawdown + COGS. Issuing an invoice — even twice, even for more
/// units than exist — must NOT touch finished_product_inventory and
/// must write exactly one issued fact. The physical drain rides the
/// dispatcher's products-consume-on-invoice-created rule through the
/// products surface, outside this tx.
#[tokio::test(flavor = "multi_thread")]
async fn issuing_never_touches_fg_and_shortage_does_not_block() {
    let db = TestDb::new().await;
    let commerce = PgCommerce::new(db.pool.clone());

    let sku = "BEER-IPA-HALF-BBL";
    seed_fg(&db, sku, 5, 8000).await;

    // FG invoice for MORE units than on hand — under invoice-owned
    // COGS this 409'd ("fictive revenue"); under consume-owned COGS
    // revenue posts and the consume NAKs until stock lands (a visible
    // backorder, not a blocked sale).
    let inv = Invoice {
        id: "inv-step-abc".to_string(),
        account_id: "account-fg-1".to_string(),
        issued_on: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        due_on: NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 500_000,
        tax_cents: 0,
        tax_jurisdiction: None,
        currency: "USD".to_string(),
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: "inv-step-abc-l1".to_string(),
            invoice_id: "inv-step-abc".to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 500_000,
            currency: "USD".to_string(),
            description: "10 × IPA half-bbl".to_string(),
            ref_id: None,
            sku: Some(sku.to_string()),
            qty: Some(10),
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    };

    commerce.create_invoice(&inv).await.unwrap();
    assert_eq!(
        fg_on_hand(&db, sku).await,
        5,
        "issuing must not touch finished_product_inventory"
    );
    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-step-abc").await,
        1
    );

    // Redelivery: still no FG mutation, still exactly one fact.
    commerce.create_invoice(&inv).await.unwrap();
    assert_eq!(fg_on_hand(&db, sku).await, 5);
    assert_eq!(
        fact_count(&db, "finance.invoice.issued", "inv-step-abc").await,
        1,
        "redelivery reuses the issued fact"
    );
}
