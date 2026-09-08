//! Rebuild the `shipments` + `shipment_assets` projections from
//! `audit_log`.
//!
//! Fifth projection rebuilder in the event-canonical arc (after
//! messages / jobs / inventory / accounts). See
//! `docs/design/projection-rebuilders.md`.
//!
//! State events consumed:
//! - `shipping.shipment.created` / `.updated` — full `Shipment`
//!   payload (parent row + `asset_ids` list driving the child
//!   `shipment_assets` table).
//! - `shipping.shipment.deleted` — `{id, deleted_at}`.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

use crate::postgres::{insert_shipment_assets, insert_shipment_row, replace_shipment_line_items};
use crate::types::Shipment;

/// Stable advisory-lock key. Distinct from boss-ledger (…_001),
/// boss-messages (…_002), boss-jobs (…_003), boss-inventory (…_004),
/// boss-accounts (…_005).
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("shipping");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub shipments_upserted: u64,
    pub shipments_deleted: u64,
    pub tracking_events_upserted: u64,
}

pub async fn rebuild_shipping(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // TRUNCATE CASCADE wipes shipments + shipment_assets +
    // shipment_tracking_events in one shot (FKs cascade). Plain
    // DELETE wouldn't reach tracking events, leaving them
    // orphaned across replays.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["TRUNCATE shipments, shipment_assets, shipment_line_items, shipment_tracking_events CASCADE"],
        "kind LIKE 'shipping.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "shipping.shipment.created" | "shipping.shipment.updated" => {
                    let shipment: Shipment = match serde_json::from_value(ev.payload.clone()) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping shipment event with non-Shipment payload (likely pre-enrichment id-only)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    // Replace the asset_ids + line_items sets wholesale
                    // per upsert event — payload is canonical.
                    sqlx::query("DELETE FROM shipment_assets WHERE shipment_id = $1")
                        .bind(&shipment.id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    insert_shipment_row(&mut *conn, &shipment, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    insert_shipment_assets(&mut *conn, &shipment)
                        .await
                        .map_err(|e| e.to_string())?;
                    replace_shipment_line_items(&mut *conn, &shipment)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.shipments_upserted += 1;
                    Ok(Applied::Yes)
                }
                "shipping.tracking.recorded" => {
                    let shipment_id = ev
                        .payload
                        .get("shipment_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let status = ev
                        .payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let occurred_on = ev
                        .payload
                        .get("occurred_on")
                        .and_then(|v| v.as_str())
                        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                    let stage_index = ev
                        .payload
                        .get("stage_index")
                        .and_then(|v| v.as_i64())
                        .map(|n| n as i16);
                    let (Some(shipment_id), Some(status), Some(occurred_on)) =
                        (shipment_id, status, occurred_on)
                    else {
                        return Ok(Applied::Skipped);
                    };
                    // Mirror the live record_tracking_scan flow:
                    // upsert the event row + roll up the shipment's
                    // status column when applicable. Skip cleanly if
                    // the shipment isn't yet projected (out-of-order
                    // event delivery).
                    //
                    // FK-violation recovery requires a SAVEPOINT — a
                    // failed statement poisons the outer tx, and every
                    // subsequent statement returns "current transaction
                    // is aborted" until ROLLBACK TO SAVEPOINT clears it.
                    // Without this savepoint wrapper, the "skip rather
                    // than fail" branch below never actually skips: the
                    // outer tx aborts on the very first orphaned
                    // tracking event and the entire shipping rebuild
                    // bails. Found on the live brewery DB.
                    sqlx::query("SAVEPOINT tracking_insert")
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    // created_at = ev.ts mirrors the live scan insert
                    // (which binds the stamp's instant) — without it
                    // the column fell to DEFAULT NOW() at replay time.
                    let inserted = sqlx::query(
                        "INSERT INTO shipment_tracking_events \
                            (shipment_id, status, occurred_on, stage_index, created_at) \
                         VALUES ($1, $2, $3, $4, $5) \
                         ON CONFLICT (shipment_id, status, occurred_on) DO NOTHING",
                    )
                    .bind(&shipment_id)
                    .bind(&status)
                    .bind(occurred_on)
                    .bind(stage_index)
                    .bind(ev.ts)
                    .execute(&mut *conn)
                    .await;
                    match inserted {
                        Ok(_) => {
                            sqlx::query("RELEASE SAVEPOINT tracking_insert")
                                .execute(&mut *conn)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        Err(sqlx::Error::Database(ref dbe))
                            if dbe.code().as_deref() == Some("23503") =>
                        {
                            // Shipment row missing — rollback to the
                            // savepoint to clear the poison, then skip.
                            sqlx::query("ROLLBACK TO SAVEPOINT tracking_insert")
                                .execute(&mut *conn)
                                .await
                                .map_err(|e| e.to_string())?;
                            return Ok(Applied::Skipped);
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                    let row_status = match status.as_str() {
                        "in-transit" => Some("in-transit"),
                        "delivered" => Some("delivered"),
                        _ => None,
                    };
                    if let Some(s) = row_status {
                        // shipped_on COALESCEs on EVERY row-status
                        // transition — mirrors the live path's fix so
                        // a rebuild reproduces shipped_on for every
                        // in-transit / delivered shipment. Without
                        // this, replaying audit_log leaves 955 of
                        // ~16k shipments with NULL shipped_on (the
                        // ones whose tracking sequence skipped the
                        // in-transit scan entirely — keg-courier
                        // shipments that delivered same-day).
                        sqlx::query(
                            "UPDATE shipments \
                             SET status = $1, \
                                 shipped_on = COALESCE(shipped_on, $2::date), \
                                 delivered_on = COALESCE(delivered_on, \
                                     CASE WHEN $1 = 'delivered' THEN $2::date ELSE NULL END), \
                                 updated_at = $3 \
                             WHERE id = $4",
                        )
                        .bind(s)
                        .bind(occurred_on)
                        .bind(ev.ts)
                        .bind(&shipment_id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                    report.tracking_events_upserted += 1;
                    Ok(Applied::Yes)
                }
                "shipping.shipment.deleted" => {
                    let id: Option<String> =
                        ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    if let Some(id) = id {
                        let n = sqlx::query("DELETE FROM shipments WHERE id = $1")
                            .bind(&id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.shipments_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown shipping.* event kind");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}
