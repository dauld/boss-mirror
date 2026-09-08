//! Rebuild the inventory projections from `audit_log`.
//!
//! Third projection rebuilder in the event-canonical arc (after
//! `boss-messages` and `boss-jobs`). See
//! `docs/design/projection-rebuilders.md`.
//!
//! Four projection tables in scope: `vendors`, `purchase_orders` +
//! `purchase_order_lines` (parent + child), `vendor_invoices`,
//! `inventory_items`.
//!
//! State events consumed (one event = one full row state):
//! - `inventory.vendor.created` / `.updated`         → upsert vendors
//! - `inventory.vendor.deleted`                       → delete vendor
//! - `inventory.purchase_order.upserted`              → upsert PO + lines
//! - `inventory.vendor_invoice.upserted`              → upsert invoice
//! - `inventory.item.upserted` / `inventory.item.consumed`
//!   → upsert inventory_item
//!
//! Marker events skipped: `inventory.po.status_changed`,
//! `inventory.vendor_invoice.approved`,
//! `inventory.vendor_invoice.paid` — sibling state events already
//! carry full row state; the vendor_invoice transitions drive the
//! ledger projection, not this one.

use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::warn;

use crate::postgres::po_status_str;
use crate::types::{InventoryItem, PurchaseOrder, Vendor, VendorInvoice};

/// Stable advisory-lock key. Distinct from boss-ledger (…_001),
/// boss-messages (…_002), boss-jobs (…_003).
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("inventory");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub vendors_upserted: u64,
    pub vendors_deleted: u64,
    pub purchase_orders_upserted: u64,
    pub vendor_invoices_upserted: u64,
    pub items_upserted: u64,
    pub vendor_contacts_upserted: u64,
    pub vendor_contacts_deleted: u64,
    pub vendor_interactions_recorded: u64,
    pub vendor_interactions_deleted: u64,
    pub vendor_team_assigned: u64,
    pub vendor_team_unassigned: u64,
    pub vendor_contracts_upserted: u64,
}

/// Drop every row in the four inventory projections and replay every
/// `inventory.*` event from `audit_log` in id order. Wrapped in one
/// advisory-locked transaction.
pub async fn rebuild_inventory(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // Order matters for FK constraints:
    // - vendor_invoices and purchase_order_lines reference purchase_orders
    // - purchase_orders.vendor is edge-enforced against vendor subjects
    // The `ON DELETE CASCADE` on the children means we only have to
    // explicitly delete from the leaf tables that reference vendors,
    // but doing all five in dependency order is more robust.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &[
            "DELETE FROM vendor_invoices",
            "DELETE FROM purchase_order_lines",
            "DELETE FROM purchase_orders",
            "DELETE FROM inventory_items",
            // Procurement (vendor CRM) projections. All four FK to
            // vendors so they must come before the vendors wipe.
            "DELETE FROM vendor_contracts",
            "DELETE FROM vendor_account_team",
            "DELETE FROM vendor_interactions",
            "DELETE FROM vendor_contacts",
            "DELETE FROM vendors",
        ],
        "kind LIKE 'inventory.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "inventory.vendor.created" | "inventory.vendor.updated" => {
                    let vendor: Vendor = match serde_json::from_value(ev.payload.clone()) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping vendor event with non-Vendor payload (likely pre-enrichment id-only)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_vendor(&mut *conn, &vendor, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendors_upserted += 1;
                    Ok(Applied::Yes)
                }
                "inventory.vendor.deleted" => {
                    let id: Option<String> =
                        ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    if let Some(id) = id {
                        let n = sqlx::query("DELETE FROM vendors WHERE id = $1")
                            .bind(&id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.vendors_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            // DELETE for a vendor we never CREATED — projection
                            // already in the right "absent" state.
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "inventory.purchase_order.upserted" => {
                    let po: PurchaseOrder = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad PO payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_purchase_order(&mut *conn, &po, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.purchase_orders_upserted += 1;
                    Ok(Applied::Yes)
                }
                "inventory.vendor_invoice.upserted" => {
                    let inv: VendorInvoice = match serde_json::from_value(ev.payload.clone()) {
                        Ok(i) => i,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad invoice payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_vendor_invoice_row(&mut *conn, &inv, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendor_invoices_upserted += 1;
                    Ok(Applied::Yes)
                }
                "inventory.item.upserted" | "inventory.item.consumed" => {
                    let item: InventoryItem = match serde_json::from_value(ev.payload.clone()) {
                        Ok(i) => i,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad item payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_inventory_item(&mut *conn, &item, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.items_upserted += 1;
                    Ok(Applied::Yes)
                }
                // Markers — sibling state events already carried full row.
                // The vendor_invoice.{approved,paid} markers are consumed by
                // the ledger (posting rules → financial_facts); the inventory
                // projection's full row already arrived via the upserted event.
                // Marker event kinds the current emitters don't fire;
                // their sibling state events carry the full row, so the
                // inventory projection skips them.
                "inventory.po.created"
                | "inventory.po.status_changed"
                | "inventory.part.consumed"
                | "inventory.vendor_invoice.approved"
                | "inventory.vendor_invoice.paid" => Ok(Applied::Skipped),
                // Model B ledger marker. `inventory.transferred`
                // is emitted alongside parts.consume so the audit_log
                // carries the raw→WIP transfer next to the inventory_items
                // mutation. The financial side rides on financial_facts
                // (and projects to gl_journal_entries); the inventory
                // projection has nothing to update — the on_hand decrement
                // already arrived via `inventory.item.consumed`.
                "inventory.transferred" => Ok(Applied::Skipped),
                // Procurement (vendor CRM) — six event kinds. Each
                // replays via the canonical adapter helper so the SQL
                // stays in one place.
                "inventory.vendor_contact.upserted" => {
                    let c: crate::procurement::types::VendorContact = match serde_json::from_value(
                        ev.payload.clone(),
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed vendor_contact payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    crate::procurement::postgres::replay_upsert_contact(&mut *conn, &c)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendor_contacts_upserted += 1;
                    Ok(Applied::Yes)
                }
                "inventory.vendor_contact.deleted" => {
                    let id: Option<String> =
                        ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    if let Some(id) = id {
                        let n = crate::procurement::postgres::replay_delete_contact(&mut *conn, &id)
                            .await
                            .map_err(|e| e.to_string())?;
                        if n > 0 {
                            report.vendor_contacts_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "inventory.vendor_interaction.recorded" => {
                    let i: crate::procurement::types::VendorInteraction = match serde_json::from_value(
                        ev.payload.clone(),
                    ) {
                        Ok(i) => i,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed vendor_interaction payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    crate::procurement::postgres::replay_upsert_interaction(&mut *conn, &i)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendor_interactions_recorded += 1;
                    Ok(Applied::Yes)
                }
                "inventory.vendor_interaction.deleted" => {
                    let id = ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    let by = ev
                        .payload
                        .get("deleted_by")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let at = ev
                        .payload
                        .get("deleted_at")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                        .unwrap_or(ev.ts);
                    if let (Some(id), Some(by)) = (id, by) {
                        let n = crate::procurement::postgres::replay_soft_delete_interaction(
                            &mut *conn, &id, &by, at,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        if n > 0 {
                            report.vendor_interactions_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "inventory.vendor_team.assigned" => {
                    let m: crate::procurement::types::VendorAccountTeamMember =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(event_id = ev.audit_id, error = %e, "skipping malformed vendor_team_assigned payload");
                                return Ok(Applied::Skipped);
                            }
                        };
                    crate::procurement::postgres::replay_upsert_team_member(&mut *conn, &m)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendor_team_assigned += 1;
                    Ok(Applied::Yes)
                }
                "inventory.vendor_team.unassigned" => {
                    let vendor_id = ev
                        .payload
                        .get("vendor_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let role = ev
                        .payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let (Some(vendor_id), Some(role)) = (vendor_id, role) {
                        let n = crate::procurement::postgres::replay_remove_team_member(
                            &mut *conn, &vendor_id, &role,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        if n > 0 {
                            report.vendor_team_unassigned += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "inventory.vendor_contract.upserted" => {
                    let c: crate::procurement::types::VendorContract = match serde_json::from_value(
                        ev.payload.clone(),
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed vendor_contract payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    crate::procurement::postgres::replay_upsert_contract(&mut *conn, &c)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.vendor_contracts_upserted += 1;
                    Ok(Applied::Yes)
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown inventory.* event kind");
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

async fn upsert_vendor(
    tx: &mut sqlx::PgConnection,
    vendor: &Vendor,
    ts: DateTime<Utc>,
) -> Result<(), RebuildError> {
    // `behavior` rides the vendor.created/.updated payloads but was
    // never reproduced here, so a rebuild silently wiped every
    // vendor's behavior profile (packet d7b8158e).
    sqlx::query(
        "INSERT INTO vendors (id, name, contact_name, contact_email, city, state, \
                              lead_time_days, payment_terms, category, behavior, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (id) DO UPDATE SET \
             name = EXCLUDED.name, \
             contact_name = EXCLUDED.contact_name, \
             contact_email = EXCLUDED.contact_email, \
             city = EXCLUDED.city, \
             state = EXCLUDED.state, \
             lead_time_days = EXCLUDED.lead_time_days, \
             payment_terms = EXCLUDED.payment_terms, \
             category = EXCLUDED.category, \
             behavior = EXCLUDED.behavior",
    )
    .bind(&vendor.id)
    .bind(&vendor.name)
    .bind(&vendor.contact_name)
    .bind(&vendor.contact_email)
    .bind(&vendor.city)
    .bind(&vendor.state)
    .bind(vendor.lead_time_days as i16)
    .bind(&vendor.payment_terms)
    .bind(&vendor.category)
    .bind(
        vendor
            .behavior
            .as_ref()
            .map(|b| serde_json::to_value(b).unwrap_or_default()),
    )
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

async fn upsert_purchase_order(
    tx: &mut sqlx::PgConnection,
    po: &PurchaseOrder,
    ts: DateTime<Utc>,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO purchase_orders (id, vendor, status, placed_on, expected_on, received_on, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (id) DO UPDATE SET \
             vendor = EXCLUDED.vendor, \
             status = EXCLUDED.status, \
             placed_on = EXCLUDED.placed_on, \
             expected_on = EXCLUDED.expected_on, \
             received_on = EXCLUDED.received_on",
    )
    .bind(&po.id)
    .bind(&po.vendor)
    .bind(po_status_str(&po.status))
    .bind(po.placed_on)
    .bind(po.expected_on)
    .bind(po.received_on)
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;

    // Replace the line set on every upsert — the event payload is
    // canonical, so any line not present in this event no longer
    // exists.
    sqlx::query("DELETE FROM purchase_order_lines WHERE po_id = $1")
        .bind(&po.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    for line in &po.lines {
        sqlx::query(
            "INSERT INTO purchase_order_lines (po_id, part_sku, qty, unit_cost_cents, currency) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&po.id)
        .bind(&line.part_sku)
        .bind(line.qty as i32)
        .bind(line.unit_cost_cents)
        .bind(&line.currency)
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    }
    Ok(())
}

async fn upsert_vendor_invoice_row(
    tx: &mut sqlx::PgConnection,
    inv: &VendorInvoice,
    ts: DateTime<Utc>,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO vendor_invoices (id, po_id, vendor, vendor_invoice_no, amount_cents, \
                                       currency, received_on, matched_on, approved_on, paid_on, \
                                       status, discrepancy_cents, discrepancy_kind, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT (id) DO UPDATE SET \
             po_id             = EXCLUDED.po_id, \
             vendor            = EXCLUDED.vendor, \
             vendor_invoice_no = EXCLUDED.vendor_invoice_no, \
             amount_cents      = EXCLUDED.amount_cents, \
             currency          = EXCLUDED.currency, \
             received_on       = EXCLUDED.received_on, \
             matched_on        = EXCLUDED.matched_on, \
             approved_on       = EXCLUDED.approved_on, \
             paid_on           = EXCLUDED.paid_on, \
             status            = EXCLUDED.status, \
             discrepancy_cents = EXCLUDED.discrepancy_cents, \
             discrepancy_kind  = EXCLUDED.discrepancy_kind",
    )
    .bind(&inv.id)
    .bind(&inv.po_id)
    .bind(&inv.vendor)
    .bind(&inv.vendor_invoice_no)
    .bind(inv.amount_cents)
    .bind(&inv.currency)
    .bind(inv.received_on)
    .bind(inv.matched_on)
    .bind(inv.approved_on)
    .bind(inv.paid_on)
    .bind(inv.status.as_str())
    .bind(inv.discrepancy_cents)
    .bind(inv.discrepancy_kind.as_ref().map(|k| k.as_str()))
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

async fn upsert_inventory_item(
    tx: &mut sqlx::PgConnection,
    item: &InventoryItem,
    ts: DateTime<Utc>,
) -> Result<(), RebuildError> {
    // Reconstruct the FULL InventoryItem from the event. Every column the
    // live batch-upsert writes (boss-inventory/postgres.rs) must be replayed
    // here, or the rebuild silently drops it — which is exactly how
    // vendor_category + vendor_price_cents got nulled on every reset,
    // breaking the cold-start auto-restock vendor resolution. These two
    // hand-written column lists (live batch-upsert vs this rebuild) drifting
    // apart is the real bug; a shared row-upsert helper would prevent it
    // recurring.
    sqlx::query(
        "INSERT INTO inventory_items (part_sku, bin, on_hand, allocated, reorder_point, \
                                       reorder_qty, trailing_90d_usage, value_cents, \
                                       vendor_price_cents, vendor_category, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (part_sku) DO UPDATE SET \
             bin = EXCLUDED.bin, \
             on_hand = EXCLUDED.on_hand, \
             allocated = EXCLUDED.allocated, \
             reorder_point = EXCLUDED.reorder_point, \
             reorder_qty = EXCLUDED.reorder_qty, \
             trailing_90d_usage = EXCLUDED.trailing_90d_usage, \
             value_cents = EXCLUDED.value_cents, \
             vendor_price_cents = EXCLUDED.vendor_price_cents, \
             vendor_category = EXCLUDED.vendor_category, \
             updated_at = EXCLUDED.updated_at",
    )
    .bind(&item.part_sku)
    .bind(&item.bin)
    .bind(item.on_hand as i32)
    .bind(item.allocated as i32)
    .bind(item.reorder_point as i32)
    .bind(item.reorder_qty as i32)
    .bind(item.trailing_90d_usage as i32)
    .bind(item.value_cents)
    .bind(item.vendor_price_cents)
    .bind(&item.vendor_category)
    .bind(ts)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}
