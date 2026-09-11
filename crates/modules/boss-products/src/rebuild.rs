//! Rebuild the products projections from `audit_log`.
//!
//! Two tables in scope: `products` (catalog) and
//! `finished_product_inventory` (per-location on-hand).
//!
//! State events consumed (one event = one full row state):
//! - `products.product.upserted`              → upsert products
//! - `products.inventory.upserted`            → upsert
//!   finished_product_inventory
//!
//! Marker events skipped: `products.produced`,
//! `products.consumed` — sibling state event
//! (`products.inventory.upserted`) already carries the full
//! post-delta on_hand + cost basis, and the GL side rides on
//! the `gl_fact_projection_rules` registry rather than this
//! projection. Replaying the deltas here would double-count.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

use crate::types::{Product, ProductInventory};

/// Stable advisory-lock key. Distinct from boss-ledger (…_001),
/// boss-messages (…_002), boss-jobs (…_003),
/// boss-inventory (…_004).
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("products");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub products_upserted: u64,
    pub inventory_rows_upserted: u64,
}

/// Drop every row in `products` + `finished_product_inventory` and
/// replay every `products.*` event from `audit_log` in id order.
/// Wrapped in one advisory-locked transaction.
pub async fn rebuild_products(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // FG inventory FKs to products via product_sku, so drop the
    // child first.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &[
            "DELETE FROM finished_product_inventory",
            "DELETE FROM products",
        ],
        "kind LIKE 'products.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "products.product.upserted" => {
                    let product: Product = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad product payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_product(&mut *conn, &product, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.products_upserted += 1;
                    Ok(Applied::Yes)
                }
                "products.inventory.upserted" => {
                    let row: ProductInventory = match serde_json::from_value(ev.payload.clone()) {
                        Ok(r) => r,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad inventory payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_inventory_row(&mut *conn, &row, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.inventory_rows_upserted += 1;
                    Ok(Applied::Yes)
                }
                // GL-only markers — the sibling `products.inventory.upserted`
                // already carries the post-delta on_hand + cost basis, and
                // the `gl_fact_projection_rules` row reproduces the
                // financial_facts row from these events independently.
                // Replaying them here would double-count physical inventory.
                "products.produced" | "products.consumed" => Ok(Applied::Skipped),
                _ => Ok(Applied::Skipped),
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

async fn upsert_product(
    tx: &mut sqlx::PgConnection,
    product: &Product,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), RebuildError> {
    // ts = the event's audit_log.timestamp — the instant the live
    // write bound into created_at/updated_at. Stamping NOW() here
    // re-dated the whole catalog to rebuild time (packet d7b8158e).
    // On conflict created_at keeps the first event's instant.
    sqlx::query(
        "INSERT INTO products (sku, name, product_kind, package_unit, description, metadata, active, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
         ON CONFLICT (sku) DO UPDATE SET \
            name = EXCLUDED.name, \
            product_kind = EXCLUDED.product_kind, \
            package_unit = EXCLUDED.package_unit, \
            description = EXCLUDED.description, \
            metadata = EXCLUDED.metadata, \
            active = EXCLUDED.active, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&product.sku)
    .bind(&product.name)
    .bind(&product.product_kind)
    .bind(&product.package_unit)
    .bind(&product.description)
    .bind(&product.metadata)
    .bind(product.active)
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

async fn upsert_inventory_row(
    tx: &mut sqlx::PgConnection,
    row: &ProductInventory,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO finished_product_inventory \
            (product_sku, location_id, on_hand, reserved, value_cents, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (product_sku, location_id) DO UPDATE SET \
            on_hand = EXCLUDED.on_hand, \
            reserved = EXCLUDED.reserved, \
            value_cents = EXCLUDED.value_cents, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&row.product_sku)
    .bind(&row.location_id)
    .bind(row.on_hand)
    .bind(row.reserved)
    .bind(row.value_cents)
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}
