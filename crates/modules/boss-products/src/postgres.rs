//! Postgres adapter for `ProductsRepository`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::port::{GlMove, InventoryDeltaResult, ProductsError, ProductsRepository};
use crate::types::{JeRecorded, Product, ProductInventory};

pub struct PgProducts {
    pool: PgPool,
}

impl PgProducts {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProductsRepository for PgProducts {
    async fn list_products(&self, active_only: bool) -> Result<Vec<Product>, ProductsError> {
        let rows: Vec<ProductRow> = if active_only {
            sqlx::query_as(
                "SELECT sku, name, product_kind, package_unit, description, metadata, active \
                 FROM products WHERE active = TRUE ORDER BY sku",
            )
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT sku, name, product_kind, package_unit, description, metadata, active \
                 FROM products ORDER BY sku",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get_product(&self, sku: &str) -> Result<Option<Product>, ProductsError> {
        let row: Option<ProductRow> = sqlx::query_as(
            "SELECT sku, name, product_kind, package_unit, description, metadata, active \
             FROM products WHERE sku = $1",
        )
        .bind(sku)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(row.map(Into::into))
    }

    async fn upsert_product(
        &self,
        product: &Product,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ProductsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        // Identity write-through (subject-model R1, Q1).
        boss_subject_kinds::subjects::record_subject_in_tx(
            &mut tx,
            "product",
            &product.sku,
            Some(&product.name),
        )
        .await
        .map_err(ProductsError::Storage)?;
        // Timestamps bind the stamp's instant (= the event's
        // audit_log.timestamp) instead of falling to the column
        // DEFAULT NOW() — the rebuilder binds ev.ts for the same
        // columns, so live and replayed rows agree byte-for-byte.
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
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the upsert event records with the row.
        let event = stamp.event(
            crate::events::PRODUCT_UPSERTED,
            serde_json::to_value(product).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ProductsError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn inventory_for(&self, sku: &str) -> Result<Vec<ProductInventory>, ProductsError> {
        let rows: Vec<InventoryRow> = sqlx::query_as(
            "SELECT product_sku, location_id, on_hand, reserved, \
                    value_cents, production_cost_cents, updated_at \
             FROM finished_product_inventory WHERE product_sku = $1 \
             ORDER BY location_id",
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn upsert_inventory(
        &self,
        row: &ProductInventory,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ProductsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        // updated_at = stamp.timestamp (the event's recorded instant),
        // not NOW() — the rebuilder binds ev.ts for the same column.
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
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the inventory event records with the row.
        let event = stamp.event(
            crate::events::PRODUCT_INVENTORY_UPSERTED,
            serde_json::to_value(row).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ProductsError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn record_inventory_je(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_table: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<JeRecorded, ProductsError> {
        if total_cost_cents <= 0 {
            return Err(ProductsError::Invalid(
                "total_cost_cents must be positive".to_string(),
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;

        // source_table folded in like the ledger movement endpoints do:
        // the emitted event must let rebuild reproduce the original
        // provenance tag (payload-authoritative source_table).
        let payload = serde_json::json!({
            "total_cost_cents": total_cost_cents,
            "debit_account": debit_account,
            "credit_account": credit_account,
            "memo": memo,
            "happened_on": happened_on.to_string(),
            "source_table": source_table,
            "source_id": source_id,
        });

        let inserted = insert_fact(
            &mut tx,
            "finance.inventory.transferred",
            happened_on,
            &payload,
            source_table,
            source_id,
        )
        .await?;

        // Read back the canonical fact_id so the journal entry posts
        // against the right row even if the INSERT was a no-op on the
        // ON CONFLICT path (rebuild replay / brewery_data_seed's
        // external opening-FG-JE call colliding with this atomic
        // post).
        let (fact_id,): (uuid::Uuid,) = sqlx::query_as(
            "SELECT id FROM financial_facts \
             WHERE kind = $1 AND source_table = $2 AND source_id = $3",
        )
        .bind("finance.inventory.transferred")
        .bind(source_table)
        .bind(source_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;

        // OUTBOX (phase 2): the event records ONLY when THIS call
        // inserted the fact — emit-once-on-`inserted`, structural in
        // the fact's own transaction (the HTTP handler used to gate
        // this on the returned bool).
        if inserted {
            let event = stamp.event(crate::events::LEDGER_INVENTORY_TRANSFERRED, payload.clone());
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(ProductsError::Storage)?;
        }

        tx.commit()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(JeRecorded {
            fact_id,
            inserted,
            payload,
        })
    }

    async fn produce(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        total_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "produce qty must be positive, got {qty}"
            )));
        }
        // One tx wraps: (1) on_hand increment + the EXACT line total
        // landing on value_cents, (2) the matching
        // `finance.inventory.transferred` financial_fact sized at the
        // same total (DR 1320 FG / CR 1310 WIP) when the caller
        // supplies a cost. The caller passes a line TOTAL, not a unit
        // cost — the WIP drain allocated exact largest-remainder
        // shares, and posting them un-rounded is what retires the
        // ~qty/2-cents-per-line residual (#73). Model B's WIP→FG
        // half-step lands atomically with the physical move.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;

        // Idempotency guard — `on_hand += qty` is relative, so a
        // redelivered produce (at-least-once delivery) would double-count.
        // The WIP→FG fact is the proof-of-application; if it already
        // exists, return the current row unchanged. Sound for cost-bearing
        // produces (the only ones that write a fact); the brewery always
        // derives a cost, so this covers it.
        if fact_exists(
            &mut tx,
            "finance.inventory.transferred",
            "products_produce",
            &source_id,
        )
        .await?
            && let Some(row) = current_inventory_row(&mut tx, sku, location_id).await?
        {
            return Ok(InventoryDeltaResult {
                inventory: row.into(),
                gl_move: None,
            });
        }

        // updated_at binds the stamp's instant on both arms — the
        // `products.inventory.upserted` event this write emits is
        // replayed with ev.ts (= stamp.timestamp) in the same column,
        // so live and rebuilt rows agree byte-for-byte.
        let row: InventoryRow = match total_cost_cents {
            Some(total) if total > 0 => sqlx::query_as(
                "INSERT INTO finished_product_inventory \
                    (product_sku, location_id, on_hand, reserved, value_cents, updated_at) \
                 VALUES ($1, $2, $3, 0, $4, $5) \
                 ON CONFLICT (product_sku, location_id) DO UPDATE SET \
                    on_hand = finished_product_inventory.on_hand + EXCLUDED.on_hand, \
                    value_cents = finished_product_inventory.value_cents \
                                  + EXCLUDED.value_cents, \
                    updated_at = EXCLUDED.updated_at \
                 RETURNING product_sku, location_id, on_hand, reserved, \
                           value_cents, production_cost_cents, updated_at",
            )
            .bind(sku)
            .bind(location_id)
            .bind(qty)
            .bind(total)
            .bind(stamp.timestamp),
            _ => sqlx::query_as(
                "INSERT INTO finished_product_inventory \
                    (product_sku, location_id, on_hand, reserved, updated_at) \
                 VALUES ($1, $2, $3, 0, $4) \
                 ON CONFLICT (product_sku, location_id) DO UPDATE SET \
                    on_hand = finished_product_inventory.on_hand + EXCLUDED.on_hand, \
                    updated_at = EXCLUDED.updated_at \
                 RETURNING product_sku, location_id, on_hand, reserved, \
                           value_cents, production_cost_cents, updated_at",
            )
            .bind(sku)
            .bind(location_id)
            .bind(qty)
            .bind(stamp.timestamp),
        }
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;

        // WIP→FG cost transfer, only when the caller knew the
        // standard cost per unit. When unit_cost_cents is None, FG
        // gets the units but the GL doesn't move — same shape as
        // a manual on_hand correction without a cost basis.
        let gl_move = if let Some(total) = total_cost_cents
            && total > 0
        {
            let happened_on = now.date_naive();
            // `source_id` + `happened_on` go INTO the payload so
            // the projection rule's `/source_id` + `/happened_on`
            // pointers find them on rebuild. Bundle export +
            // re-import lands an identical `financial_facts` row.
            let payload = serde_json::json!({
                "total_cost_cents": total,
                "debit_account": "1320",
                "credit_account": "1310",
                "memo": format!(
                    "Production — produced {qty} × {sku} (WIP → FG, exact line total)"
                ),
                "sku": sku,
                "location_id": location_id,
                "qty": qty,
                "source_id": source_id,
                "happened_on": happened_on.to_string(),
            });
            insert_fact(
                &mut tx,
                "finance.inventory.transferred",
                happened_on,
                &payload,
                "products_produce",
                &source_id,
            )
            .await?;
            Some(GlMove {
                source_id,
                happened_on,
                payload,
            })
        } else {
            None
        };

        // OUTBOX (phase 2): the post-delta inventory row + (when the
        // GL moved) the produced fact payload record in THIS tx. The
        // idempotency guard above means a redelivered produce never
        // reaches here — no duplicate events, same as no duplicate
        // on_hand.
        let inventory: ProductInventory = row.into();
        let event = stamp.event(
            crate::events::PRODUCT_INVENTORY_UPSERTED,
            serde_json::to_value(&inventory).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ProductsError::Storage)?;
        if let Some(gl) = &gl_move {
            let event = stamp.event(crate::events::PRODUCT_PRODUCED, gl.payload.clone());
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(ProductsError::Storage)?;
        }

        tx.commit()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;
        Ok(InventoryDeltaResult { inventory, gl_move })
    }

    async fn consume(
        &self,
        sku: &str,
        location_id: &str,
        qty: i32,
        revenue_category: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: String,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<InventoryDeltaResult, ProductsError> {
        if qty <= 0 {
            return Err(ProductsError::Invalid(format!(
                "consume qty must be positive, got {qty}"
            )));
        }
        // One tx wraps: (1) the proportional value drain + on_hand
        // decrement, (2) the matching `finance.cogs.recognized` JE
        // sized at exactly the drained value (DR 5100 COGS / CR 1320
        // FG). Every finished keg leaving inventory traces back to a
        // real COGS recognition at the cost it was produced at — now
        // to the cent: the drain is round(value × qty / on_hand) with
        // the final unit taking the remainder, so zero on_hand forces
        // zero value and balance(1320) == Σ value_cents holds by
        // construction.
        // Tag the payload with `revenue_category` when supplied so
        // the finance margin rollups can sum COGS by category
        // directly instead of pro-rating against the period revenue
        // mix.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProductsError::Storage(e.to_string()))?;

        // Idempotency guard — `on_hand -= qty` is relative, so a
        // redelivered consume would double-decrement (and could spuriously
        // fail the `on_hand >= qty` check once stock has fallen). The COGS
        // fact is the proof-of-application; if it exists, return the
        // current row unchanged. Sound for cost-bearing consumes (those
        // that write a fact); a zero-production-cost FG row (early starter
        // inventory with no basis) writes none and isn't guarded — no GL
        // impact, and the seeded brewery costs its opening FG.
        if fact_exists(
            &mut tx,
            "finance.cogs.recognized",
            "products_consume",
            &source_id,
        )
        .await?
            && let Some(row) = current_inventory_row(&mut tx, sku, location_id).await?
        {
            return Ok(InventoryDeltaResult {
                inventory: row.into(),
                gl_move: None,
            });
        }

        // Read under lock, compute the exact drain once, apply it.
        let before: Option<(i32, i64)> = sqlx::query_as(
            "SELECT on_hand, value_cents FROM finished_product_inventory \
             WHERE product_sku = $1 AND location_id = $2 FOR UPDATE",
        )
        .bind(sku)
        .bind(location_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        let drained_cents = match before {
            Some((on_hand_before, value_before)) if on_hand_before >= qty => {
                if on_hand_before == qty {
                    value_before
                } else {
                    (((value_before as i128) * (qty as i128) + (on_hand_before as i128) / 2)
                        / (on_hand_before as i128)) as i64
                }
            }
            _ => {
                drop(tx);
                return Err(ProductsError::Invalid(format!(
                    "consume failed: row missing or on_hand < {qty} for {sku} @ {location_id}"
                )));
            }
        };
        // updated_at = stamp.timestamp: same instant the emitted
        // `products.inventory.upserted` event records, same instant
        // the rebuilder binds at replay.
        let updated: Option<InventoryRow> = sqlx::query_as(
            "UPDATE finished_product_inventory \
                SET on_hand = on_hand - $3, \
                    value_cents = value_cents - $4, \
                    updated_at = $5 \
              WHERE product_sku = $1 AND location_id = $2 \
              RETURNING product_sku, location_id, on_hand, reserved, \
                        value_cents, production_cost_cents, updated_at",
        )
        .bind(sku)
        .bind(location_id)
        .bind(qty)
        .bind(drained_cents)
        .bind(stamp.timestamp)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ProductsError::Storage(e.to_string()))?;
        match updated {
            Some(row) => {
                // COGS recognition. Skip when the drain is zero — the
                // FG row was seeded without a cost basis (the
                // early-day starter inventory case). The on_hand still
                // moved so the physical-side story is intact; books
                // absorb the gap until the next produce lands value.
                let total_cost = drained_cents;
                let gl_move = if total_cost > 0 {
                    let happened_on = now.date_naive();
                    let mut payload = serde_json::json!({
                        "total_cost_cents": total_cost,
                        "cogs_account": "5100",
                        "inventory_account": "1320",
                        "memo": format!(
                            "COGS — sold {qty} × {sku} (value drain)"
                        ),
                        "sku": sku,
                        "location_id": location_id,
                        "qty": qty,
                        "source_id": source_id,
                        "happened_on": happened_on.to_string(),
                    });
                    if let Some(cat) = revenue_category {
                        payload["revenue_category"] = serde_json::Value::String(cat.to_string());
                    }
                    insert_fact(
                        &mut tx,
                        "finance.cogs.recognized",
                        happened_on,
                        &payload,
                        "products_consume",
                        &source_id,
                    )
                    .await?;
                    Some(GlMove {
                        source_id,
                        happened_on,
                        payload,
                    })
                } else {
                    None
                };
                // OUTBOX (phase 2): post-delta row + (on a GL move)
                // the consumed fact payload, in THIS tx. The
                // idempotency guard above keeps redeliveries from
                // reaching here — no duplicate events.
                let inventory: ProductInventory = row.into();
                let event = stamp.event(
                    crate::events::PRODUCT_INVENTORY_UPSERTED,
                    serde_json::to_value(&inventory).unwrap_or_default(),
                );
                boss_events::outbox::record_event_in_tx(&mut tx, &event)
                    .await
                    .map_err(ProductsError::Storage)?;
                if let Some(gl) = &gl_move {
                    let event = stamp.event(crate::events::PRODUCT_CONSUMED, gl.payload.clone());
                    boss_events::outbox::record_event_in_tx(&mut tx, &event)
                        .await
                        .map_err(ProductsError::Storage)?;
                }
                tx.commit()
                    .await
                    .map_err(|e| ProductsError::Storage(e.to_string()))?;
                Ok(InventoryDeltaResult { inventory, gl_move })
            }
            None => Err(ProductsError::Invalid(format!(
                "consume failed: row missing or on_hand < {qty} for {sku} @ {location_id}"
            ))),
        }
    }
}

/// Does a financial_fact with this natural key already exist? The
/// produce/consume idempotency guard: the fact a mutation writes is its
/// proof-of-application, so an existing fact means a redelivered
/// step-effect already applied the relative `on_hand ± qty`.
async fn fact_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    source_table: &str,
    source_id: &str,
) -> Result<bool, ProductsError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| ProductsError::Storage(e.to_string()))?;
    Ok(row.is_some())
}

/// Current FG inventory row for `(sku, location_id)` within a tx — used to
/// return an unchanged result when the idempotency guard skips a mutation.
async fn current_inventory_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sku: &str,
    location_id: &str,
) -> Result<Option<InventoryRow>, ProductsError> {
    sqlx::query_as(
        "SELECT product_sku, location_id, on_hand, reserved, \
                value_cents, production_cost_cents, updated_at \
         FROM finished_product_inventory \
         WHERE product_sku = $1 AND location_id = $2",
    )
    .bind(sku)
    .bind(location_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| ProductsError::Storage(e.to_string()))
}

/// Insert a `financial_facts` row in the given tx + project it
/// via the ledger's posting rules. Mirrors
/// `boss-inventory::postgres::insert_fact` — same idempotency
/// shape (unique on `(kind, source_table, source_id)`), same
/// post-fact-in-tx hand-off.
async fn insert_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &'static str,
    happened_on: chrono::NaiveDate,
    payload: &serde_json::Value,
    source_table: &str,
    source_id: &str,
) -> Result<bool, ProductsError> {
    let result = sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'products') \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING",
    )
    .bind(boss_ledger::deterministic_fact_id(
        kind,
        source_table,
        source_id,
    ))
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_table)
    .bind(source_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| ProductsError::Storage(e.to_string()))?;

    let (fact_id,): (uuid::Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| ProductsError::Storage(e.to_string()))?;

    let fact_ref = boss_ledger::FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    boss_ledger::post_fact_in_tx(tx, &fact_ref)
        .await
        .map_err(|e| ProductsError::Storage(format!("ledger post failed: {e}")))?;
    Ok(result.rows_affected() > 0)
}

#[derive(sqlx::FromRow)]
struct ProductRow {
    sku: String,
    name: String,
    product_kind: String,
    package_unit: String,
    description: Option<String>,
    metadata: serde_json::Value,
    active: bool,
}

impl From<ProductRow> for Product {
    fn from(r: ProductRow) -> Self {
        Self {
            sku: r.sku,
            name: r.name,
            product_kind: r.product_kind,
            package_unit: r.package_unit,
            description: r.description,
            metadata: r.metadata,
            active: r.active,
        }
    }
}

#[derive(sqlx::FromRow)]
struct InventoryRow {
    product_sku: String,
    location_id: String,
    on_hand: i32,
    reserved: i32,
    value_cents: i64,
    production_cost_cents: i64,
    updated_at: DateTime<Utc>,
}

impl From<InventoryRow> for ProductInventory {
    fn from(r: InventoryRow) -> Self {
        Self {
            product_sku: r.product_sku,
            location_id: r.location_id,
            on_hand: r.on_hand,
            reserved: r.reserved,
            value_cents: r.value_cents,
            production_cost_cents: r.production_cost_cents,
            updated_at: Some(r.updated_at),
        }
    }
}
