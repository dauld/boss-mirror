//! Domain event subjects for inventory operations.
//!
//! Two-layer split (same shape as boss-jobs):
//!
//! - **State events** carry full row state and are what the
//!   audit_log → projection rebuild path consumes.
//! - **Marker events** are informational signals for downstream
//!   consumers (low-stock alerts, status badges). They duplicate
//!   state already in the sibling state event but give consumers
//!   a topic to filter on without payload matching. Rebuild
//!   ignores them.

// Vendors — state events carry full Vendor row state.
// vendor.deleted carries `{id, deleted_at}` (the rebuild's signal
// to omit the row).
pub const VENDOR_CREATED: &str = "inventory.vendor.created";
pub const VENDOR_UPDATED: &str = "inventory.vendor.updated";
pub const VENDOR_DELETED: &str = "inventory.vendor.deleted";

// Purchase orders — full PurchaseOrder row state (includes lines).
// `upserted` covers both create and update because the Pg adapter
// uses INSERT … ON CONFLICT DO UPDATE for both. Rebuild treats
// the latest event for each PO id as the canonical state.
pub const PO_UPSERTED: &str = "inventory.purchase_order.upserted";

/// Marker — fires on PO status transitions. Payload `{id,
/// old_status, new_status}`. Rebuild ignores; the sibling
/// PO_UPSERTED already carries the new status.
pub const PO_STATUS_CHANGED: &str = "inventory.po.status_changed";

// Vendor invoices — full VendorInvoice row state.
pub const VENDOR_INVOICE_UPSERTED: &str = "inventory.vendor_invoice.upserted";

/// Transition event — fires the first time `approved_on` lands on a
/// vendor invoice (idempotent on the upsert path; only emitted when
/// the upsert flips approved_on from NULL to a date). Drives the
/// `finance.bill.approved` projection in the gl_fact_projection_rules
/// registry. The state event VENDOR_INVOICE_UPSERTED stays for the
/// inventory rebuilder.
pub const VENDOR_INVOICE_APPROVED: &str = "inventory.vendor_invoice.approved";

/// Transition event — same shape as VENDOR_INVOICE_APPROVED but for
/// `paid_on`. Drives the `finance.bill.paid` projection.
pub const VENDOR_INVOICE_PAID: &str = "inventory.vendor_invoice.paid";

// Inventory items (parts on hand). Two state events:
// - ITEM_UPSERTED  — full InventoryItem absolute state (bin, on_hand, …)
// - ITEM_CONSUMED  — full post-consume row state (qty subtracted),
//   so rebuild treats it as "last write wins" per part_sku and never
//   has to do delta arithmetic.
pub const ITEM_UPSERTED: &str = "inventory.item.upserted";
pub const ITEM_CONSUMED: &str = "inventory.item.consumed";

/// Goods-receipt log marker, fires alongside ITEM_UPSERTED on every
/// `receive_part_at`. Carries the receive's deterministic `source_id`
/// (`{step_id}:{part_sku}`) so the ledger-facts rebuilder can
/// reconstruct the `finance.inventory.received` dedup-fact from
/// audit_log alone — exactly as INVENTORY_TRANSFERRED reconstructs the
/// raw→WIP fact, EXCEPT this one is GL-INERT: there is deliberately NO
/// `gl_fact_projection_rules` row for it and the ledger RuleSet has no
/// arm for `finance.inventory.received`, so the rebuilt fact drives
/// zero journal lines. The matching DR-1300 rides the idempotent
/// bill-approval path; posting from here would double-post it. Payload:
/// ```text
///   {
///     "source_id":        "{step_id}:{part_sku}",  // dedup-fact source_id
///     "part_sku":         "<sku>",
///     "qty":              <qty>,
///     "unit_cost_cents":  <cost or null>,
///     "received_on":      "YYYY-MM-DD"              // dedup-fact happened_on
///   }
/// ```
pub const ITEM_RECEIVED: &str = "inventory.item.received";

/// Inventory cost-transfer event, fires alongside ITEM_CONSUMED
/// whenever the consumed SKU has a non-zero `avg_cost_cents`.
/// Carries the raw → WIP cost-transfer payload so the ledger-
/// facts rebuilder can reproject `finance.inventory.transferred`
/// financial_facts from audit_log alone (matching the in-tx
/// insert_fact path `consume_part_at` uses on the live path).
/// Payload:
/// ```text
///   {
///     "total_cost_cents":  qty * avg_cost,
///     "debit_account":     "1310",
///     "credit_account":    "1300",
///     "memo":              "Production — consumed N × <sku> @ Mc/unit (raw → WIP)",
///     "part_sku":          "<sku>",
///     "qty":               <qty>,
///     "unit_cost_cents":   <avg_cost>
///   }
/// ```
pub const INVENTORY_TRANSFERRED: &str = "inventory.transferred";

/// Burden absorption marker — a production-overhead driver capitalized
/// into WIP at production-consume time. Fires alongside the raw → WIP
/// `INVENTORY_TRANSFERRED` event once per `inventory.overhead.absorb`
/// rule do (direct labor, process utilities, production
/// depreciation, …), sized rate × batch bbl. Payload mirrors
/// the financial-fact shape (total_cost_cents, debit_account,
/// credit_account, memo) so the projection rule can map it 1:1 into
/// a `finance.inventory.transferred` fact on rebuild without a
/// payload transform — same pattern as ITEM_CONSUMED → cogs +
/// transferred. Closes the WIP-balance gap: without this event,
/// burden absorption lives only in financial_facts and vanishes when
/// the audit_log-only seed bundle is exported + reimported.
pub const INVENTORY_OVERHEAD_ABSORBED: &str = "inventory.overhead.absorbed";

// Vendor CRM (procurement). Four entity families, all upsert-shaped
// (handlers use ON CONFLICT DO UPDATE so create + update collapse
// onto one event kind). Soft-delete + hard-delete each get their
// own kind because the projection-side write differs.
//
// Every procurement write emits one of these so rebuild_inventory can
// replay it; otherwise the rows would be wiped on every CASCADE
// replay with nothing to repopulate them.

/// Vendor contact upserted (POST /api/inventory/vendors/{id}/contacts).
/// Payload: full `VendorContact` row state.
pub const VENDOR_CONTACT_UPSERTED: &str = "inventory.vendor_contact.upserted";

/// Vendor contact hard-deleted. Payload `{id, vendor_id, deleted_at}`.
pub const VENDOR_CONTACT_DELETED: &str = "inventory.vendor_contact.deleted";

/// Vendor interaction recorded (call / email / meeting summary).
/// Payload: full `VendorInteraction` row state.
pub const VENDOR_INTERACTION_RECORDED: &str = "inventory.vendor_interaction.recorded";

/// Vendor interaction soft-deleted. Payload `{id, deleted_by,
/// deleted_at}`. Rebuild stamps the soft-delete metadata.
pub const VENDOR_INTERACTION_DELETED: &str = "inventory.vendor_interaction.deleted";

/// Vendor account-team member upserted (territory rep / contract
/// owner / etc.). Payload: full `VendorAccountTeamMember`.
pub const VENDOR_TEAM_ASSIGNED: &str = "inventory.vendor_team.assigned";

/// Vendor account-team member removed. Payload `{vendor_id, role,
/// employee_id, unassigned_at}`.
pub const VENDOR_TEAM_UNASSIGNED: &str = "inventory.vendor_team.unassigned";

/// Vendor contract upserted (draft / active / terminated lifecycle).
/// Payload: full `VendorContract` row state.
pub const VENDOR_CONTRACT_UPSERTED: &str = "inventory.vendor_contract.upserted";

/// Manual/atomic inventory value movement — the SAME kind the ledger
/// movement endpoint emits, because it re-projects through the SAME
/// `gl_fact_projection_rules` row. Emitted by the batch-upsert
/// handler when its atomic opening JE actually inserts the fact
/// (payload verbatim; payload-authoritative source_table on rebuild).
pub const LEDGER_INVENTORY_TRANSFERRED: &str = "ledger.inventory.transferred";
