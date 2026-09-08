//! Postgres adapter for `ShippingRepository`.
//!
//! Queries `shipments` table and `shipment_assets` satellite table,
//! assembling asset_ids from the satellite rows.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::port::{ShippingError, ShippingRepository};
use crate::types::*;

pub struct PgShipping {
    pool: PgPool,
}

impl PgShipping {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ShippingRepository for PgShipping {
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError> {
        let rows: Vec<ShipmentRow> =
            sqlx::query_as("SELECT * FROM shipments ORDER BY created_on DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| ShippingError::Storage(e.to_string()))?;

        let mut shipments = Vec::with_capacity(rows.len());
        for row in rows {
            let serials = self.fetch_asset_ids(&row.id).await?;
            let lines = self.fetch_line_items(&row.id).await?;
            shipments.push(row.into_shipment(serials, lines));
        }
        Ok(shipments)
    }

    async fn list_shipments(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Shipment>, i64), ShippingError> {
        let (total,): (i64,) = match account_id {
            Some(cid) => {
                sqlx::query_as("SELECT count(*) FROM shipments WHERE account_id = $1")
                    .bind(cid)
                    .fetch_one(&self.pool)
                    .await
            }
            None => {
                sqlx::query_as("SELECT count(*) FROM shipments")
                    .fetch_one(&self.pool)
                    .await
            }
        }
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        let rows: Vec<ShipmentRow> = match account_id {
            Some(cid) => {
                sqlx::query_as(
                    "SELECT * FROM shipments WHERE account_id = $1 \
                 ORDER BY created_on DESC LIMIT $2 OFFSET $3",
                )
                .bind(cid)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as(
                    "SELECT * FROM shipments ORDER BY created_on DESC LIMIT $1 OFFSET $2",
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        let mut shipments = Vec::with_capacity(rows.len());
        for row in rows {
            let serials = self.fetch_asset_ids(&row.id).await?;
            let lines = self.fetch_line_items(&row.id).await?;
            shipments.push(row.into_shipment(serials, lines));
        }
        Ok((shipments, total))
    }

    async fn shipment_by_id(&self, id: &str) -> Result<Option<Shipment>, ShippingError> {
        let row: Option<ShipmentRow> = sqlx::query_as("SELECT * FROM shipments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;

        match row {
            Some(r) => {
                let serials = self.fetch_asset_ids(&r.id).await?;
                let lines = self.fetch_line_items(&r.id).await?;
                Ok(Some(r.into_shipment(serials, lines)))
            }
            None => Ok(None),
        }
    }

    async fn create_shipment_at(
        &self,
        shipment: &Shipment,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, ShippingError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        // Upsert: if the shipment already exists, update status +
        // delivery dates. This supports both initial creation and
        // lifecycle advancement from the shipping generator via the
        // same batch endpoint. Device junction rows use ON CONFLICT
        // DO NOTHING since they don't change across status updates.
        // Identity write-through (subject-model R1, Q1).
        boss_subject_kinds::subjects::record_subject_in_tx(&mut tx, "shipment", &shipment.id, None)
            .await
            .map_err(ShippingError::Storage)?;
        insert_shipment_row(&mut tx, shipment, now).await?;
        insert_shipment_assets(&mut tx, shipment).await?;
        replace_shipment_line_items(&mut tx, shipment).await?;
        // OUTBOX (phase 2): the created event (full row state, matching
        // the upsert semantics above) records with the rows.
        let event = stamp.event(
            crate::events::SHIPMENT_CREATED,
            serde_json::to_value(shipment).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ShippingError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        Ok(shipment.id.clone())
    }

    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM shipments WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| ShippingError::Storage(e.to_string()))?;
        if !exists {
            return Err(ShippingError::NotFound(id.to_string()));
        }
        // UPSERT preserves the original `created_at`; it's
        // load-bearing for the audit_log → projection rebuild path
        // to reproduce timestamps exactly (a DELETE-then-INSERT would
        // reset created_at on every update).
        insert_shipment_row(&mut tx, shipment, now).await?;
        // shipment_assets still needs full replacement because the
        // junction has no per-row "id" — asset_ids could be added
        // OR removed in one update.
        sqlx::query("DELETE FROM shipment_assets WHERE shipment_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        insert_shipment_assets(&mut tx, shipment).await?;
        replace_shipment_line_items(&mut tx, shipment).await?;
        // OUTBOX (phase 2): the updated event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::SHIPMENT_UPDATED,
            serde_json::to_value(shipment).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ShippingError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError> {
        let dir_str = direction_str(direction);
        let week_ago = today - chrono::Duration::days(7);

        // Counts per in-flight status (excludes Delivered).
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, COUNT(*)::bigint \
             FROM shipments \
             WHERE direction = $1 AND status <> 'delivered' \
             GROUP BY status",
        )
        .bind(dir_str)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        let mut label_created = 0i64;
        let mut picked_up = 0i64;
        let mut in_transit = 0i64;
        let mut exception = 0i64;
        for (status, cnt) in rows {
            match status.as_str() {
                "label-created" => label_created = cnt,
                "picked-up" => picked_up = cnt,
                "in-transit" => in_transit = cnt,
                "exception" => exception = cnt,
                _ => {}
            }
        }

        let (delivered_7d,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM shipments \
             WHERE direction = $1 AND status = 'delivered' AND delivered_on >= $2",
        )
        .bind(dir_str)
        .bind(week_ago)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        // Top-N recent: in-flight first sorted by most-recent shipped_on,
        // then delivered rows sorted by delivered_on. One query via a
        // computed bucket + sort_date column; shipment_assets count
        // joined once as a subquery aggregate.
        let preview_rows: Vec<RecentRow> = sqlx::query_as(
            "SELECT s.id, s.status, s.carrier, s.destination, s.account_id, \
                    s.shipped_on, s.estimated_delivery, s.delivered_on, s.created_on, \
                    COALESCE(sys.n, 0)::bigint AS asset_id_count \
             FROM shipments s \
             LEFT JOIN ( \
                 SELECT shipment_id, COUNT(*) AS n FROM shipment_assets GROUP BY shipment_id \
             ) sys ON sys.shipment_id = s.id \
             WHERE s.direction = $1 \
             ORDER BY \
               CASE WHEN s.status = 'delivered' THEN 1 ELSE 0 END ASC, \
               COALESCE( \
                 CASE WHEN s.status = 'delivered' THEN s.delivered_on ELSE s.shipped_on END, \
                 s.created_on \
               ) DESC \
             LIMIT $2",
        )
        .bind(dir_str)
        .bind(recent_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        let recent: Vec<boss_shipping_client::OutboundShipmentRow> = preview_rows
            .into_iter()
            .map(|r| boss_shipping_client::OutboundShipmentRow {
                id: r.id,
                status: r.status,
                // Preview DTO carries a display string; an
                // un-enriched (NULL) carrier renders as empty.
                carrier: r.carrier.unwrap_or_default(),
                destination: r.destination,
                account_id: r.account_id,
                shipped_on: r.shipped_on,
                estimated_delivery: r.estimated_delivery,
                asset_id_count: r.asset_id_count as usize,
            })
            .collect();

        Ok(boss_shipping_client::OutboundShipmentSummary {
            label_created,
            picked_up,
            in_transit,
            exception,
            delivered_7d,
            recent,
        })
    }

    async fn delete_shipment_at(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM shipment_assets WHERE shipment_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        let result = sqlx::query("DELETE FROM shipments WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(ShippingError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): the deleted event records only after the
        // row actually deleted (NotFound above returns pre-recording,
        // so a phantom delete records nothing).
        let event = stamp.event(
            crate::events::SHIPMENT_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(ShippingError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn record_tracking_scan(
        &self,
        shipment_id: &str,
        status: &str,
        occurred_on: chrono::NaiveDate,
        stage_index: Option<i16>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), ShippingError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;

        // Insert the tracking event row. ON CONFLICT DO NOTHING
        // gives us idempotency on (shipment_id, status,
        // occurred_on) for free; the FK to shipments(id) is what
        // surfaces NotFound on out-of-order scans.
        // created_at binds the stamp's instant (= the event's
        // audit_log.timestamp) instead of falling to the column
        // DEFAULT NOW(), so the replayed row matches byte-for-byte.
        let result = sqlx::query(
            "INSERT INTO shipment_tracking_events \
                (shipment_id, status, occurred_on, stage_index, created_at) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (shipment_id, status, occurred_on) DO NOTHING",
        )
        .bind(shipment_id)
        .bind(status)
        .bind(occurred_on)
        .bind(stage_index)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            // PG foreign_key_violation = 23503; surface as NotFound
            // so the HTTP layer skips cleanly.
            if let sqlx::Error::Database(ref dbe) = e
                && dbe.code().as_deref() == Some("23503")
            {
                return ShippingError::NotFound(shipment_id.to_string());
            }
            ShippingError::Storage(e.to_string())
        })?;
        // The ON CONFLICT DO NOTHING idempotency guard doubles as the
        // event gate: a replayed scan inserts nothing and records
        // nothing (structural emit-once).
        let inserted = result.rows_affected() > 0;

        // Roll up the shipment row's `status` column when the
        // scan corresponds to one of the row-state values.
        // out-for-delivery is a tracking-only sub-state and
        // doesn't change the rollup; in-transit and delivered
        // do. Mapping kept narrow on purpose so unknown statuses
        // (carrier-specific edges) don't silently mutate the row.
        //
        // Gated on `inserted`: a replayed scan (conflict-skip) already
        // rolled up when it first landed. Re-running the rollup would
        // let a stale redelivered scan REGRESS status (in-transit
        // after delivered) with no event behind it — a live-vs-rebuilt
        // divergence. The scan is a full no-op or a full fact.
        let row_status = match status {
            "in-transit" => Some("in-transit"),
            "delivered" => Some("delivered"),
            _ => None,
        };
        if inserted && let Some(s) = row_status {
            // shipped_on COALESCEs on EVERY row-status transition so
            // a delivered shipment with no preceding in-transit scan
            // (carrier consolidator that only emits a final delivery
            // scan) still ends with shipped_on populated. Without
            // this, in-transit / out-for-delivery / delivered rows
            // showed NULL shipped_on in the Exec dashboard's recent
            // shipments preview, which was the user-flagged bug.
            sqlx::query(
                "UPDATE shipments \
                 SET status = $1, \
                     shipped_on = COALESCE(shipped_on, $2::date), \
                     delivered_on = COALESCE(delivered_on, \
                         CASE WHEN $1 = 'delivered' THEN $2::date ELSE NULL END), \
                     updated_at = $4 \
                 WHERE id = $3",
            )
            .bind(s)
            .bind(occurred_on)
            .bind(shipment_id)
            .bind(stamp.timestamp)
            .execute(&mut *tx)
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        }

        if inserted {
            // OUTBOX (phase 2): the scan event records with the row.
            let event = stamp.event(
                crate::events::TRACKING_RECORDED,
                serde_json::json!({
                    "shipment_id": shipment_id,
                    "status": status,
                    "occurred_on": occurred_on,
                    "stage_index": stage_index,
                }),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(ShippingError::Storage)?;
        }

        tx.commit()
            .await
            .map_err(|e| ShippingError::Storage(e.to_string()))?;
        Ok(())
    }
}

impl PgShipping {
    async fn fetch_asset_ids(&self, shipment_id: &str) -> Result<Vec<String>, ShippingError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT asset_id FROM shipment_assets WHERE shipment_id = $1 ORDER BY asset_id",
        )
        .bind(shipment_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    async fn fetch_line_items(
        &self,
        shipment_id: &str,
    ) -> Result<Vec<crate::types::ShipmentLineItem>, ShippingError> {
        let rows: Vec<(String, i32, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT sku, qty, unit_price_cents, description \
             FROM shipment_line_items WHERE shipment_id = $1 ORDER BY idx",
        )
        .bind(shipment_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(
                |(sku, qty, unit_price_cents, description)| crate::types::ShipmentLineItem {
                    sku,
                    qty,
                    unit_price_cents,
                    description,
                },
            )
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Write helpers
// ---------------------------------------------------------------------------

fn to_kebab<T: serde::Serialize>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

pub(crate) async fn insert_shipment_row(
    tx: &mut sqlx::PgConnection,
    s: &Shipment,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ShippingError> {
    sqlx::query(
        "INSERT INTO shipments (id, direction, status, carrier, tracking_number, origin, \
         destination, po_id, order_id, account_id, created_on, shipped_on, estimated_delivery, \
         delivered_on, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$15) \
         ON CONFLICT (id) DO UPDATE SET \
            direction = EXCLUDED.direction, \
            status = EXCLUDED.status, \
            carrier = EXCLUDED.carrier, \
            tracking_number = EXCLUDED.tracking_number, \
            origin = EXCLUDED.origin, \
            destination = EXCLUDED.destination, \
            po_id = EXCLUDED.po_id, \
            order_id = EXCLUDED.order_id, \
            account_id = EXCLUDED.account_id, \
            created_on = EXCLUDED.created_on, \
            shipped_on = EXCLUDED.shipped_on, \
            estimated_delivery = EXCLUDED.estimated_delivery, \
            delivered_on = EXCLUDED.delivered_on, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&s.id)
    .bind(to_kebab(&s.direction))
    // Transparent `ShipmentStatus` wrapper — `as_str()` is the bare
    // kebab code the column stores, no serde round-trip needed.
    .bind(s.status.as_str())
    // Identity-first nullable carrier: bind the bare code when
    // present, SQL NULL when absent. The transparent `Carrier`
    // wrapper means `as_str()` already yields the plain string, so no
    // `to_kebab` round-trip is needed here.
    .bind(s.carrier.as_ref().map(|c| c.as_str()))
    .bind(&s.tracking_number)
    .bind(&s.origin)
    .bind(&s.destination)
    .bind(&s.po_id)
    .bind(&s.order_id)
    .bind(&s.account_id)
    .bind(s.created_on)
    .bind(s.shipped_on)
    .bind(s.estimated_delivery)
    .bind(s.delivered_on)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| ShippingError::Storage(e.to_string()))?;
    Ok(())
}

pub(crate) async fn insert_shipment_assets(
    tx: &mut sqlx::PgConnection,
    s: &Shipment,
) -> Result<(), ShippingError> {
    for serial in &s.asset_ids {
        sqlx::query(
            "INSERT INTO shipment_assets (shipment_id, asset_id) \
             VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(&s.id)
        .bind(serial)
        .execute(&mut *tx)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;
    }
    Ok(())
}

/// Full replace of `shipment_line_items` rows for a shipment.
/// Matches the `shipment_assets` pattern — the join carries no
/// stable per-row id, so an add OR remove on update has to wipe +
/// rewrite. Idempotent: re-running with the same `Shipment` value
/// produces an identical row set.
pub(crate) async fn replace_shipment_line_items(
    tx: &mut sqlx::PgConnection,
    s: &Shipment,
) -> Result<(), ShippingError> {
    sqlx::query("DELETE FROM shipment_line_items WHERE shipment_id = $1")
        .bind(&s.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;
    for (idx, line) in s.line_items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO shipment_line_items \
                (shipment_id, idx, sku, qty, unit_price_cents, description) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&s.id)
        .bind(idx as i32)
        .bind(&line.sku)
        .bind(line.qty)
        .bind(line.unit_price_cents)
        .bind(&line.description)
        .execute(&mut *tx)
        .await
        .map_err(|e| ShippingError::Storage(e.to_string()))?;
    }
    Ok(())
}

fn direction_str(d: ShipmentDirection) -> &'static str {
    match d {
        ShipmentDirection::Inbound => "inbound",
        ShipmentDirection::Outbound => "outbound",
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct RecentRow {
    id: String,
    status: String,
    carrier: Option<String>,
    destination: String,
    account_id: Option<String>,
    shipped_on: Option<chrono::NaiveDate>,
    estimated_delivery: Option<chrono::NaiveDate>,
    #[allow(dead_code)]
    delivered_on: Option<chrono::NaiveDate>,
    #[allow(dead_code)]
    created_on: chrono::NaiveDate,
    asset_id_count: i64,
}

#[derive(sqlx::FromRow)]
struct ShipmentRow {
    id: String,
    direction: String,
    status: String,
    carrier: Option<String>,
    tracking_number: Option<String>,
    origin: String,
    destination: String,
    po_id: Option<String>,
    order_id: Option<String>,
    account_id: Option<String>,
    created_on: chrono::NaiveDate,
    shipped_on: Option<chrono::NaiveDate>,
    estimated_delivery: Option<chrono::NaiveDate>,
    delivered_on: Option<chrono::NaiveDate>,
}

impl ShipmentRow {
    fn into_shipment(
        self,
        asset_ids: Vec<String>,
        line_items: Vec<crate::types::ShipmentLineItem>,
    ) -> Shipment {
        Shipment {
            id: self.id,
            direction: parse_direction(&self.direction).unwrap_or(ShipmentDirection::Outbound),
            // Free-text Class code; the column holds the kebab string,
            // so the newtype wraps it as-is.
            status: ShipmentStatus::new(self.status),
            carrier: self.carrier.map(Carrier),
            tracking_number: self.tracking_number,
            origin: self.origin,
            destination: self.destination,
            asset_ids,
            line_items,
            po_id: self.po_id,
            order_id: self.order_id,
            account_id: self.account_id,
            created_on: self.created_on,
            shipped_on: self.shipped_on,
            estimated_delivery: self.estimated_delivery,
            delivered_on: self.delivered_on,
        }
    }
}

// ---------------------------------------------------------------------------
// Enum parsing
// ---------------------------------------------------------------------------

fn parse_direction(s: &str) -> Result<ShipmentDirection, ShippingError> {
    match s {
        "inbound" => Ok(ShipmentDirection::Inbound),
        "outbound" => Ok(ShipmentDirection::Outbound),
        other => Err(ShippingError::Storage(format!(
            "unknown shipment direction: {other}"
        ))),
    }
}

// Carrier is now a free-text wrapper, lifted to the Class registry
// under (subject_kind='shipment'). Validation against the active
// Class set lives at the API boundary, not the storage adapter, so
// this layer trusts whatever the DB carries (no parse_carrier).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_directions() {
        let cases = ["inbound", "outbound"];
        for s in cases {
            assert!(parse_direction(s).is_ok(), "failed to parse direction: {s}");
        }
        assert!(parse_direction("sideways").is_err());
    }

    #[test]
    fn status_round_trips_through_serde() {
        // Free-text wrapper serializes transparently to the bare kebab
        // code the column stores, and round-trips back. Validation
        // against the active Class set lives at the API boundary, not
        // this adapter — so the storage layer accepts any string.
        for code in [
            ShipmentStatus::LABEL_CREATED,
            ShipmentStatus::PICKED_UP,
            ShipmentStatus::IN_TRANSIT,
            ShipmentStatus::DELIVERED,
            ShipmentStatus::EXCEPTION,
        ] {
            let st = ShipmentStatus::new(code);
            assert_eq!(st.as_str(), code);
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{code}\""));
            let back: ShipmentStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, st);
        }
    }

    #[test]
    fn carrier_round_trips_through_serde() {
        // Free-text wrapper serializes transparently to a bare string
        // (so `to_kebab` on the INSERT path produces the plain code)
        // and round-trips back. Validation against the active Class set
        // lives at the API boundary, not here.
        let c = Carrier::new("fedex");
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "\"fedex\"");
        let back: Carrier = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}
