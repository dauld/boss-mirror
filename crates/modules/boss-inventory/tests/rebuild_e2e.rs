//! End-to-end: drive inventory writes through the API on the REAL
//! pipeline (PgInventory records each event on the transactional
//! outbox in the write's tx; the relay drain moves it to audit_log),
//! snapshot the four projection tables, drop them, rebuild from
//! `audit_log`, assert exact match.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_inventory::PgInventory;
use boss_inventory::http::{InventoryApiState, router};
use boss_inventory::rebuild_inventory;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

// Not Eq: `behavior` is JSONB (serde_json::Value is PartialEq only).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
struct VendorRow {
    id: String,
    name: Option<String>,
    contact_name: Option<String>,
    contact_email: Option<String>,
    city: Option<String>,
    state: Option<String>,
    lead_time_days: i16,
    payment_terms: Option<String>,
    category: Option<String>,
    // Reproduced from the event payload — the rebuilder used to drop
    // this column entirely, so a replay silently wiped every vendor's
    // behavior profile (packet d7b8158e).
    behavior: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct PoRow {
    id: String,
    vendor: Option<String>,
    status: String,
    placed_on: Option<chrono::NaiveDate>,
    expected_on: Option<chrono::NaiveDate>,
    received_on: Option<chrono::NaiveDate>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct PoLineRow {
    po_id: String,
    part_sku: String,
    qty: i32,
    unit_cost_cents: i64,
    currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct InvoiceRow {
    id: String,
    po_id: String,
    vendor: String,
    vendor_invoice_no: String,
    amount_cents: i64,
    currency: String,
    received_on: chrono::NaiveDate,
    matched_on: Option<chrono::NaiveDate>,
    approved_on: Option<chrono::NaiveDate>,
    paid_on: Option<chrono::NaiveDate>,
    status: String,
    discrepancy_cents: Option<i64>,
    discrepancy_kind: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ItemRow {
    part_sku: String,
    bin: String,
    on_hand: i32,
    allocated: i32,
    reorder_point: i32,
    reorder_qty: i32,
    trailing_90d_usage: i32,
    updated_at: DateTime<Utc>,
}

async fn snapshot_vendors(pool: &PgPool) -> Vec<VendorRow> {
    sqlx::query_as("SELECT id, name, contact_name, contact_email, city, state, lead_time_days, payment_terms, category, behavior, created_at FROM vendors ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_pos(pool: &PgPool) -> Vec<PoRow> {
    sqlx::query_as("SELECT id, vendor, status, placed_on, expected_on, received_on, created_at FROM purchase_orders ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_po_lines(pool: &PgPool) -> Vec<PoLineRow> {
    sqlx::query_as("SELECT po_id, part_sku, qty, unit_cost_cents, currency FROM purchase_order_lines ORDER BY po_id, part_sku")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_invoices(pool: &PgPool) -> Vec<InvoiceRow> {
    sqlx::query_as("SELECT id, po_id, vendor, vendor_invoice_no, amount_cents, currency, received_on, matched_on, approved_on, paid_on, status, discrepancy_cents, discrepancy_kind, created_at FROM vendor_invoices ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_items(pool: &PgPool) -> Vec<ItemRow> {
    sqlx::query_as("SELECT part_sku, bin, on_hand, allocated, reorder_point, reorder_qty, trailing_90d_usage, updated_at FROM inventory_items ORDER BY part_sku")
        .fetch_all(pool).await.unwrap()
}

fn build_app(pool: PgPool) -> Router {
    let inventory = Arc::new(PgInventory::new(pool.clone()));
    // No publisher: stamps fall back to source="inventory" and the
    // adapters record on the outbox — deliberately NO direct audit
    // writer, so these tests only pass through the real
    // outbox → relay → audit_log path.
    let state = InventoryApiState {
        inventory,
        publisher: None,
        clients: None,
        classes_client: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

/// Drain the outbox through the relay pipeline: outbox → audit_log
/// (chained) → bus → delivered. Rebuild reads audit_log.
async fn drain(pool: &PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 1000)
        .await
        .expect("relay drain")
        .delivered
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_all_four_inventory_projections() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Create two vendors — one carrying a behavior profile, so
    //    the rebuild-omission of `vendors.behavior` (packet
    //    d7b8158e) is exercised, not just NULL-vs-NULL.
    for (id, name, cat, behavior) in [
        (
            "VND-001",
            "Hopswell",
            "hops-supplier",
            Some(serde_json::json!({
                "lead_time_days": 12.0,
                "lead_spread_days": 2.0,
                "fulfilment_rate": 0.97,
                "ap_payment_days": 28.0,
                "ap_spread_days": 3.0,
                "provenance": {"source": "hand_set", "template": "hops-supplier"},
            })),
        ),
        ("VND-002", "Maltworks", "malt-supplier", None),
    ] {
        TestRequest::post("/api/inventory/vendors")
            .json(&serde_json::json!({
                "id": id,
                "name": name,
                "contact_name": "Pat Buyer",
                "contact_email": format!("pat@{name}.example").to_lowercase(),
                "city": "Austin",
                "state": "TX",
                "lead_time_days": 14,
                "payment_terms": "net-30",
                "category": cat,
                "behavior": behavior,
            }))
            .send(&app)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // 2. Update one of the vendors (behavior included — the update
    //    event must carry it through the replay too).
    TestRequest::put("/api/inventory/vendors/VND-001")
        .json(&serde_json::json!({
            "name": "Hopswell (Renamed)",
            "contact_name": "Pat Buyer",
            "contact_email": "pat@hopswell.example",
            "city": "Austin",
            "state": "TX",
            "lead_time_days": 21,
            "payment_terms": "net-45",
            "category": "hops-supplier",
            "behavior": {
                "lead_time_days": 15.0,
                "lead_spread_days": 2.0,
                "fulfilment_rate": 0.95,
                "ap_payment_days": 30.0,
                "ap_spread_days": 3.0,
                "provenance": {"source": "hand_set", "template": "hops-supplier"},
            },
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Upsert two inventory items.
    TestRequest::post("/api/inventory/items/batch")
        .json(&serde_json::json!([
            {"part_sku": "HOPS-CASCADE", "bin": "B-01", "on_hand": 100, "allocated": 0, "reorder_point": 20, "reorder_qty": 50, "trailing_90d_usage": 60},
            {"part_sku": "MALT-PILSNER", "bin": "B-02", "on_hand": 200, "allocated": 0, "reorder_point": 50, "reorder_qty": 100, "trailing_90d_usage": 150},
        ]))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // 4. Create a purchase order with two lines.
    TestRequest::post("/api/inventory/orders/create")
        .json(&serde_json::json!({
            "vendor": "VND-001",
            "lines": [
                {"part_sku": "HOPS-CASCADE", "qty": 50, "unit_cost_cents": 1500, "currency": "USD"},
                {"part_sku": "HOPS-CITRA",   "qty": 30, "unit_cost_cents": 1800, "currency": "USD"},
            ],
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    let po_id: String = {
        let row: (String,) = sqlx::query_as("SELECT id FROM purchase_orders LIMIT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        row.0
    };

    // 5. Update PO status (Submitted → InTransit).
    TestRequest::put(format!("/api/inventory/orders/{po_id}/status"))
        .json(&serde_json::json!({"status": "in-transit"}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // 6. Upsert a vendor invoice for the PO.
    TestRequest::post("/api/inventory/vendor-invoices")
        .json(&serde_json::json!({
            "id": "VINV-0001",
            "po_id": po_id,
            "vendor": "VND-001",
            "vendor_invoice_no": "INV-9001",
            "amount_cents": 129000,
            "currency": "USD",
            "received_on": "2026-04-10",
            "matched_on": "2026-04-11",
            "approved_on": null,
            "paid_on": null,
            "status": "matched",
            "discrepancy_cents": null,
            "discrepancy_kind": null,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 7. Consume some of HOPS-CASCADE.
    TestRequest::post("/api/inventory/items/HOPS-CASCADE/consume")
        .json(&serde_json::json!({"qty": 12}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // 8. Snapshot all four projections.
    let vendors_before = snapshot_vendors(&db.pool).await;
    let pos_before = snapshot_pos(&db.pool).await;
    let lines_before = snapshot_po_lines(&db.pool).await;
    let invoices_before = snapshot_invoices(&db.pool).await;
    let items_before = snapshot_items(&db.pool).await;

    assert_eq!(vendors_before.len(), 2, "two vendors");
    assert_eq!(pos_before.len(), 1, "one PO");
    assert_eq!(lines_before.len(), 2, "two PO lines");
    assert_eq!(invoices_before.len(), 1, "one vendor invoice");
    assert_eq!(items_before.len(), 2, "two inventory items");
    let cascade = items_before
        .iter()
        .find(|i| i.part_sku == "HOPS-CASCADE")
        .unwrap();
    assert_eq!(cascade.on_hand, 88, "post-consume on_hand");

    // 9. Relay the outbox to audit_log, then sanity-check the events.
    drain(&db.pool).await;
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'inventory.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    // 2 vendor.created + 1 vendor.updated + 1 item batch → 2 item.upserted
    // + 1 po.upserted
    // + 1 po.upserted + 1 po.status_changed marker (status update reads back)
    // + 1 vendor_invoice.upserted
    // + 1 item.consumed (+ cogs.recognized when avg_cost_cents > 0)
    // = at least 8 events.
    assert!(event_count.0 >= 8, "got only {} events", event_count.0);

    // 10. Wipe + rebuild.
    for table in [
        "vendor_invoices",
        "purchase_order_lines",
        "purchase_orders",
        "inventory_items",
        "vendors",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&db.pool)
            .await
            .unwrap();
    }

    let report = rebuild_inventory(&db.pool).await.expect("rebuild succeeds");
    assert!(report.vendors_upserted >= 3, "create+create+update = 3");
    assert!(
        report.purchase_orders_upserted >= 2,
        "create + status-update = 2"
    );
    assert!(report.vendor_invoices_upserted == 1);
    assert!(
        report.items_upserted >= 3,
        "2 batch upserts + 1 consume = 3"
    );

    // 11. Reconstructed projections must match originals exactly.
    let vendors_after = snapshot_vendors(&db.pool).await;
    let pos_after = snapshot_pos(&db.pool).await;
    let lines_after = snapshot_po_lines(&db.pool).await;
    let invoices_after = snapshot_invoices(&db.pool).await;
    let items_after = snapshot_items(&db.pool).await;

    assert_eq!(vendors_before, vendors_after, "vendors mismatch");
    assert_eq!(pos_before, pos_after, "purchase_orders mismatch");
    assert_eq!(lines_before, lines_after, "purchase_order_lines mismatch");
    assert_eq!(invoices_before, invoices_after, "vendor_invoices mismatch");
    assert_eq!(items_before, items_after, "inventory_items mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_vendor_delete() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // Create + delete a vendor.
    TestRequest::post("/api/inventory/vendors")
        .json(&serde_json::json!({
            "id": "VND-DEL",
            "name": "Doomed Vendor",
            "contact_name": "Gone",
            "contact_email": "gone@doomed.example",
            "city": "Austin",
            "state": "TX",
            "lead_time_days": 7,
            "payment_terms": "net-30",
            "category": "general",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete("/api/inventory/vendors/VND-DEL")
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vendors")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    // Relay the create + delete events to audit_log for the rebuild.
    assert_eq!(drain(&db.pool).await, 2, "create + delete via the outbox");

    let report = rebuild_inventory(&db.pool).await.unwrap();
    // Create + delete events both processed; final state has zero
    // vendor rows (the delete event removes the row that the create
    // event inserted).
    assert!(report.vendors_upserted >= 1);
    assert!(report.vendors_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vendors")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}
