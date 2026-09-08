//! End-to-end: drive products writes through the HTTP API on the
//! real pipeline (outbox phase 2), snapshot `products` +
//! `finished_product_inventory`, rebuild from `audit_log`, and
//! assert byte-identical rows — timestamps included.
//!
//! Before packet d7b8158e both the live writes and the rebuilder
//! left `created_at` / `updated_at` to the column DEFAULT NOW() (or
//! stamped NOW() in the conflict arm), so a replay reproduced the
//! catalog with rebuild-time timestamps instead of event-time.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use boss_products::rebuild_products;
use boss_products::types::{Product, ProductInventory};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ProductRow {
    sku: String,
    name: String,
    product_kind: String,
    package_unit: String,
    description: Option<String>,
    metadata: serde_json::Value,
    active: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct InventoryRow {
    product_sku: String,
    location_id: String,
    on_hand: i32,
    reserved: i32,
    value_cents: i64,
    updated_at: DateTime<Utc>,
}

async fn snapshot_products(pool: &PgPool) -> Vec<ProductRow> {
    sqlx::query_as(
        "SELECT sku, name, product_kind, package_unit, description, metadata, active, \
                created_at, updated_at \
         FROM products ORDER BY sku",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn snapshot_inventory(pool: &PgPool) -> Vec<InventoryRow> {
    sqlx::query_as(
        "SELECT product_sku, location_id, on_hand, reserved, value_cents, updated_at \
         FROM finished_product_inventory ORDER BY product_sku, location_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    router(ProductsApiState {
        products: Arc::new(PgProducts::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    })
}

async fn drain_outbox(pool: &PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain")
        .delivered
}

fn product(sku: &str, name: &str) -> Product {
    Product {
        sku: sku.into(),
        name: name.into(),
        product_kind: "beer".into(),
        package_unit: "1/2-bbl-keg".into(),
        description: Some("Test brew".into()),
        metadata: serde_json::json!({"abv_pct": 5.0}),
        active: true,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_products_and_inventory_byte_identical() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Two products; re-upsert one so the ON CONFLICT arm (the
    //    `updated_at` stamp) is exercised, not just the insert arm.
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-PALE", "Test Pale"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-STOUT", "Test Stout"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post("/api/products")
        .json(&product("FP-TEST-PALE", "Test Pale (renamed)"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. An inventory row via PUT, then a produce and a consume so
    //    every write arm that stamps `updated_at` runs.
    TestRequest::put("/api/products/FP-TEST-PALE/inventory")
        .json(&ProductInventory {
            product_sku: "FP-TEST-PALE".into(),
            location_id: "wh-1".into(),
            on_hand: 10,
            reserved: 0,
            value_cents: 10_000,
            production_cost_cents: 0,
            updated_at: None,
        })
        .send(&app)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/products/FP-TEST-PALE/inventory/produce")
        .json(&serde_json::json!({
            "location_id": "wh-1",
            "qty": 5,
            "total_cost_cents": 5_000,
            "idempotency_key": "rebuild-e2e-produce-1",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/products/FP-TEST-PALE/inventory/consume")
        .json(&serde_json::json!({
            "location_id": "wh-1",
            "qty": 3,
            "idempotency_key": "rebuild-e2e-consume-1",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    drain_outbox(&db.pool).await;

    let products_before = snapshot_products(&db.pool).await;
    let inventory_before = snapshot_inventory(&db.pool).await;
    assert_eq!(products_before.len(), 2);
    assert_eq!(inventory_before.len(), 1);
    assert_eq!(inventory_before[0].on_hand, 12, "10 + 5 - 3");

    // 3. Rebuild wipes both tables and replays audit_log.
    let report = rebuild_products(&db.pool).await.expect("rebuild");
    assert_eq!(report.products_upserted, 3, "2 creates + 1 re-upsert");

    let products_after = snapshot_products(&db.pool).await;
    let inventory_after = snapshot_inventory(&db.pool).await;
    assert_eq!(
        products_before, products_after,
        "products must replay byte-identical (created_at/updated_at included)"
    );
    assert_eq!(
        inventory_before, inventory_after,
        "finished_product_inventory must replay byte-identical (updated_at included)"
    );
}
