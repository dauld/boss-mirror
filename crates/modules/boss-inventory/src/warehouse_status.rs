//! Cross-entity warehouse-status projection.
//!
//! Backs `/api/inventory/warehouse-status` and the `/warehouse`
//! dashboard + B5 embedded slice plugin (operations-needs session 3,
//! E1). The projection aggregates three views of physical inventory:
//!
//! - Parts stock (on-hand / allocated / available / below-reorder)
//! - Inbound POs (draft / submitted / in-transit / late / arriving-soon)
//! - Outbound shipments (status counts + recent rows)
//!
//! The outbound summary is computed by the shipping client adapter
//! (`boss-shipping-client`); this module combines it with inventory's
//! own items + POs into the wire shape the frontend consumes.
//!
//! Recent receive/put-away activity is a v1.1 follow-up (needs an assets
//! events feed the warehouse dashboard can subscribe to).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{InventoryItem, PoStatus, PurchaseOrder};

// Re-exported so callers can import everything they need from one place
// and so the frontend TS types have a single Rust-side anchor.
pub use boss_shipping_client::{OutboundShipmentRow, OutboundShipmentSummary};

/// Top-of-wire response shape for `/api/inventory/warehouse-status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarehouseStatus {
    pub parts_stock: PartsStockSummary,
    pub inbound_pos: InboundPoSummary,
    pub outbound_shipments: OutboundShipmentSummary,
    /// Snapshot time — clients can display "updated X ago" without
    /// threading a second timestamp through.
    pub as_of: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Parts stock
// ---------------------------------------------------------------------------

/// Aggregate + top-N view of the parts stock room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartsStockSummary {
    pub total_skus: i64,
    pub total_on_hand: i64,
    pub total_allocated: i64,
    pub total_available: i64,
    pub below_reorder_count: i64,
    /// Below-reorder rows, most-severe first (lowest `available - reorder_point`).
    /// Capped at `LOW_STOCK_PREVIEW_LIMIT`; the `/warehouse` page fetches
    /// the full list via `/api/inventory/items` if the user expands.
    pub below_reorder_items: Vec<LowStockItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowStockItem {
    pub part_sku: String,
    pub bin: String,
    pub on_hand: u32,
    pub allocated: u32,
    pub available: u32,
    pub reorder_point: u32,
}

pub const LOW_STOCK_PREVIEW_LIMIT: usize = 20;

fn parts_stock_summary(items: &[InventoryItem]) -> PartsStockSummary {
    let mut total_on_hand: i64 = 0;
    let mut total_allocated: i64 = 0;
    let mut total_available: i64 = 0;
    let mut below_reorder: Vec<LowStockItem> = Vec::new();

    for item in items {
        let available = item.on_hand.saturating_sub(item.allocated);
        total_on_hand += i64::from(item.on_hand);
        total_allocated += i64::from(item.allocated);
        total_available += i64::from(available);
        if available <= item.reorder_point {
            below_reorder.push(LowStockItem {
                part_sku: item.part_sku.clone(),
                bin: item.bin.clone(),
                on_hand: item.on_hand,
                allocated: item.allocated,
                available,
                reorder_point: item.reorder_point,
            });
        }
    }

    // Most severe first — lowest (available - reorder_point) comes first,
    // so zero-stock criticals bubble to the top of the table.
    below_reorder.sort_by_key(|i| i64::from(i.available) - i64::from(i.reorder_point));
    let below_reorder_count = below_reorder.len() as i64;
    below_reorder.truncate(LOW_STOCK_PREVIEW_LIMIT);

    PartsStockSummary {
        total_skus: items.len() as i64,
        total_on_hand,
        total_allocated,
        total_available,
        below_reorder_count,
        below_reorder_items: below_reorder,
    }
}

// ---------------------------------------------------------------------------
// Inbound POs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundPoSummary {
    pub total_open: i64,
    pub draft_count: i64,
    pub submitted_count: i64,
    pub acknowledged_count: i64,
    pub in_transit_count: i64,
    /// Open POs whose expected_on is before today.
    pub late_count: i64,
    /// Open POs whose expected_on falls within today..=today+7.
    pub arriving_this_week_count: i64,
    /// Open POs sorted by expected_on ascending, capped at preview limit.
    pub recent: Vec<InboundPoRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundPoRow {
    pub id: String,
    pub vendor: String,
    pub status: String,
    pub expected_on: NaiveDate,
    /// Negative when `expected_on` is already in the past (= late by N days).
    pub days_until_expected: i64,
    pub line_count: usize,
}

pub const INBOUND_PO_PREVIEW_LIMIT: usize = 20;

fn inbound_pos_summary(pos: &[PurchaseOrder], today: NaiveDate) -> InboundPoSummary {
    let open: Vec<&PurchaseOrder> = pos.iter().filter(|po| po.status.is_open()).collect();

    let week_end = today + Duration::days(7);

    let mut draft = 0i64;
    let mut submitted = 0i64;
    let mut acknowledged = 0i64;
    let mut in_transit = 0i64;
    let mut late = 0i64;
    let mut arriving_soon = 0i64;

    for po in &open {
        // Known statuses bucket into the named counters; a
        // tenant-added status still counts in total_open above.
        match po.status.as_str() {
            PoStatus::DRAFT => draft += 1,
            PoStatus::SUBMITTED => submitted += 1,
            PoStatus::ACKNOWLEDGED => acknowledged += 1,
            PoStatus::IN_TRANSIT => in_transit += 1,
            _ => {}
        }
        // Only placed POs have an expected-arrival date; a bare Draft
        // (identity-first, no dates yet) is counted above but isn't
        // late/arriving.
        if let Some(expected_on) = po.expected_on {
            if expected_on < today {
                late += 1;
            } else if expected_on <= week_end {
                arriving_soon += 1;
            }
        }
    }

    // The inbound preview is "what's on the way" — placed POs with an
    // expected date. Drafts without dates aren't inbound yet.
    let mut sorted: Vec<&PurchaseOrder> = open
        .iter()
        .copied()
        .filter(|po| po.expected_on.is_some())
        .collect();
    sorted.sort_by_key(|po| po.expected_on);
    let recent: Vec<InboundPoRow> = sorted
        .into_iter()
        .take(INBOUND_PO_PREVIEW_LIMIT)
        .filter_map(|po| {
            let expected_on = po.expected_on?;
            Some(InboundPoRow {
                id: po.id.clone(),
                vendor: po.vendor.clone().unwrap_or_default(),
                status: po.status.to_string(),
                expected_on,
                days_until_expected: (expected_on - today).num_days(),
                line_count: po.lines.len(),
            })
        })
        .collect();

    InboundPoSummary {
        total_open: open.len() as i64,
        draft_count: draft,
        submitted_count: submitted,
        acknowledged_count: acknowledged,
        in_transit_count: in_transit,
        late_count: late,
        arriving_this_week_count: arriving_soon,
        recent,
    }
}

// Outbound shipments (`OutboundShipmentSummary` / `OutboundShipmentRow`)
// are re-exported from the shipping client crate above — one type per
// wire shape.

// ---------------------------------------------------------------------------
// Aggregator
// ---------------------------------------------------------------------------

/// Combine inventory's own data (items + POs) with the pre-fetched
/// outbound shipment summary into the wire shape. Pure function — no
/// I/O, so tests can pin every branch deterministically.
pub fn build_warehouse_status(
    items: &[InventoryItem],
    purchase_orders: &[PurchaseOrder],
    outbound_shipments: OutboundShipmentSummary,
    as_of: DateTime<Utc>,
) -> WarehouseStatus {
    WarehouseStatus {
        parts_stock: parts_stock_summary(items),
        inbound_pos: inbound_pos_summary(purchase_orders, as_of.date_naive()),
        outbound_shipments,
        as_of,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PurchaseOrderLine;

    fn item(sku: &str, on_hand: u32, allocated: u32, reorder_point: u32) -> InventoryItem {
        InventoryItem {
            part_sku: sku.to_string(),
            bin: "A-01".to_string(),
            on_hand,
            allocated,
            reorder_point,
            reorder_qty: 100,
            trailing_90d_usage: 30,
            value_cents: 0,
            avg_cost_cents: 0,
            vendor_price_cents: None,
            vendor_category: None,
        }
    }

    fn po(id: &str, status: PoStatus, expected_on: NaiveDate) -> PurchaseOrder {
        PurchaseOrder {
            id: id.to_string(),
            vendor: Some("Acme".to_string()),
            status,
            placed_on: Some(expected_on - Duration::days(10)),
            expected_on: Some(expected_on),
            received_on: None,
            lines: vec![PurchaseOrderLine {
                part_sku: "PART-001".to_string(),
                qty: 10,
                unit_cost_cents: 5_000,
                currency: "USD".to_string(),
            }],
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn ts(y: i32, m: u32, day: u32) -> DateTime<Utc> {
        d(y, m, day).and_hms_opt(12, 0, 0).unwrap().and_utc()
    }

    // --- Parts stock -------------------------------------------------------

    #[test]
    fn parts_stock_flags_items_below_reorder() {
        let items = vec![
            item("PART-A", 100, 20, 30), // available=80, above reorder
            item("PART-B", 15, 5, 20),   // available=10, below reorder
            item("PART-C", 5, 0, 10),    // available=5, below reorder
        ];
        let summary = parts_stock_summary(&items);
        assert_eq!(summary.total_skus, 3);
        assert_eq!(summary.total_on_hand, 120);
        assert_eq!(summary.total_allocated, 25);
        assert_eq!(summary.total_available, 95);
        assert_eq!(summary.below_reorder_count, 2);
        assert_eq!(summary.below_reorder_items.len(), 2);
    }

    #[test]
    fn parts_stock_sorts_most_severe_first() {
        // PART-A: available=5, reorder=10, severity=-5
        // PART-B: available=0, reorder=20, severity=-20 (most severe)
        // PART-C: available=15, reorder=18, severity=-3
        let items = vec![
            item("PART-A", 5, 0, 10),
            item("PART-B", 0, 0, 20),
            item("PART-C", 15, 0, 18),
        ];
        let summary = parts_stock_summary(&items);
        let skus: Vec<&str> = summary
            .below_reorder_items
            .iter()
            .map(|i| i.part_sku.as_str())
            .collect();
        assert_eq!(skus, vec!["PART-B", "PART-A", "PART-C"]);
    }

    #[test]
    fn parts_stock_caps_preview_at_limit() {
        let items: Vec<InventoryItem> = (0..(LOW_STOCK_PREVIEW_LIMIT + 5))
            .map(|i| item(&format!("P-{i}"), 0, 0, 10))
            .collect();
        let summary = parts_stock_summary(&items);
        assert_eq!(
            summary.below_reorder_count,
            (LOW_STOCK_PREVIEW_LIMIT + 5) as i64
        );
        assert_eq!(summary.below_reorder_items.len(), LOW_STOCK_PREVIEW_LIMIT);
    }

    #[test]
    fn parts_stock_treats_allocated_over_on_hand_as_zero_available() {
        // Data integrity violation (allocated > on_hand) — saturate to
        // zero rather than panic on u32 underflow.
        let items = vec![item("PART-A", 5, 10, 2)];
        let summary = parts_stock_summary(&items);
        assert_eq!(summary.total_available, 0);
        assert_eq!(summary.below_reorder_count, 1);
    }

    // --- Inbound POs -------------------------------------------------------

    #[test]
    fn inbound_pos_exclude_received_and_closed() {
        let today = d(2026, 4, 22);
        let pos = vec![
            po(
                "PO-1",
                PoStatus::new(PoStatus::DRAFT),
                today + Duration::days(3),
            ),
            po(
                "PO-2",
                PoStatus::new(PoStatus::RECEIVED),
                today - Duration::days(10),
            ),
            po(
                "PO-3",
                PoStatus::new(PoStatus::CLOSED),
                today - Duration::days(30),
            ),
            po(
                "PO-4",
                PoStatus::new(PoStatus::IN_TRANSIT),
                today + Duration::days(1),
            ),
        ];
        let summary = inbound_pos_summary(&pos, today);
        assert_eq!(summary.total_open, 2);
        assert_eq!(summary.draft_count, 1);
        assert_eq!(summary.in_transit_count, 1);
        assert_eq!(summary.late_count, 0);
    }

    #[test]
    fn inbound_pos_flags_late_when_expected_date_has_passed() {
        let today = d(2026, 4, 22);
        let pos = vec![
            po(
                "PO-LATE",
                PoStatus::new(PoStatus::IN_TRANSIT),
                today - Duration::days(3),
            ),
            po(
                "PO-SOON",
                PoStatus::new(PoStatus::SUBMITTED),
                today + Duration::days(2),
            ),
            po(
                "PO-FAR",
                PoStatus::new(PoStatus::DRAFT),
                today + Duration::days(30),
            ),
        ];
        let summary = inbound_pos_summary(&pos, today);
        assert_eq!(summary.late_count, 1);
        assert_eq!(summary.arriving_this_week_count, 1);
    }

    #[test]
    fn inbound_pos_recent_sorted_by_expected_on_asc() {
        let today = d(2026, 4, 22);
        let pos = vec![
            po(
                "PO-FAR",
                PoStatus::new(PoStatus::SUBMITTED),
                today + Duration::days(20),
            ),
            po(
                "PO-NEAR",
                PoStatus::new(PoStatus::SUBMITTED),
                today + Duration::days(2),
            ),
            po(
                "PO-LATE",
                PoStatus::new(PoStatus::IN_TRANSIT),
                today - Duration::days(5),
            ),
        ];
        let summary = inbound_pos_summary(&pos, today);
        let ids: Vec<&str> = summary.recent.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["PO-LATE", "PO-NEAR", "PO-FAR"]);
        assert_eq!(summary.recent[0].days_until_expected, -5);
        assert_eq!(summary.recent[1].days_until_expected, 2);
    }

    #[test]
    fn inbound_pos_status_labels_kebab_case() {
        let today = d(2026, 4, 22);
        let pos = vec![po("PO-IT", PoStatus::new(PoStatus::IN_TRANSIT), today)];
        let summary = inbound_pos_summary(&pos, today);
        assert_eq!(summary.recent[0].status, "in-transit");
    }

    // --- Aggregator --------------------------------------------------------

    #[test]
    fn build_warehouse_status_passes_through_cross_service_summaries() {
        let items = vec![item("PART-A", 50, 10, 20)];
        let pos = vec![po(
            "PO-1",
            PoStatus::new(PoStatus::SUBMITTED),
            d(2026, 4, 25),
        )];
        let outbound = OutboundShipmentSummary {
            label_created: 2,
            picked_up: 1,
            in_transit: 5,
            exception: 0,
            delivered_7d: 9,
            recent: vec![],
        };

        let status = build_warehouse_status(&items, &pos, outbound.clone(), ts(2026, 4, 22));

        assert_eq!(status.parts_stock.total_skus, 1);
        assert_eq!(status.inbound_pos.total_open, 1);
        assert_eq!(status.outbound_shipments, outbound);
        assert_eq!(status.as_of, ts(2026, 4, 22));
    }

    // Backlog a8991c86: two summaries filled only by a retired example
    // tenant's Workflows rode this wire, drawn by an SPA block that
    // hid itself for every other tenant. The wire carries what the
    // warehouse page reads, and one key more is a key nobody reads.
    #[test]
    fn the_wire_carries_only_what_the_warehouse_page_reads() {
        let status = build_warehouse_status(
            &[],
            &[],
            OutboundShipmentSummary::default(),
            ts(2026, 4, 22),
        );
        let wire = serde_json::to_value(&status).unwrap();
        let mut keys: Vec<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["as_of", "inbound_pos", "outbound_shipments", "parts_stock"]
        );
    }

    #[test]
    fn build_warehouse_status_empty_inputs_produce_zero_counts() {
        let status = build_warehouse_status(
            &[],
            &[],
            OutboundShipmentSummary::default(),
            ts(2026, 4, 22),
        );
        assert_eq!(status.parts_stock.total_skus, 0);
        assert_eq!(status.parts_stock.below_reorder_count, 0);
        assert_eq!(status.inbound_pos.total_open, 0);
    }

    // ---------------------------------------------------------------------
    // Property-based tests
    //
    // Proptest on the pure aggregators. (A formal-methods doc was
    // cited here and never written.) Each asserts a structural
    // invariant that has to hold regardless of the shape of the input.
    // Strategies build worst-case inputs (empty, zero, overflow,
    // duplicates, monotonic growth) automatically.
    // ---------------------------------------------------------------------

    use proptest::collection::vec;
    use proptest::prelude::*;

    prop_compose! {
        fn arb_item()(
            sku in "[A-Z]{3}-[0-9]{1,4}",
            on_hand in 0u32..10_000,
            allocated in 0u32..10_000,
            reorder_point in 0u32..500,
        ) -> InventoryItem {
            InventoryItem {
                part_sku: sku,
                bin: "BIN".to_string(),
                on_hand,
                allocated,
                reorder_point,
                reorder_qty: 100,
                trailing_90d_usage: 30,
                value_cents: 0,
            avg_cost_cents: 0,
                vendor_price_cents: None,
                vendor_category: None,
            }
        }
    }

    // POs: allow every status, and let expected_on walk within ±180d
    // of a fixed anchor so `late` / `soon` / `far` all get explored.
    prop_compose! {
        fn arb_po()(
            id in "[A-Z]{2}-[0-9]{1,5}",
            status_ix in 0usize..6,
            offset_days in -180i64..180,
        ) -> PurchaseOrder {
            let status = match status_ix {
                0 => PoStatus::new(PoStatus::DRAFT),
                1 => PoStatus::new(PoStatus::SUBMITTED),
                2 => PoStatus::new(PoStatus::ACKNOWLEDGED),
                3 => PoStatus::new(PoStatus::IN_TRANSIT),
                4 => PoStatus::new(PoStatus::RECEIVED),
                _ => PoStatus::new(PoStatus::CLOSED),
            };
            let today = d(2026, 4, 22);
            let expected = today + Duration::days(offset_days);
            PurchaseOrder {
                id,
                vendor: Some("Acme".into()),
                status,
                placed_on: Some(expected - Duration::days(10)),
                expected_on: Some(expected),
                received_on: None,
                lines: vec![PurchaseOrderLine {
                    part_sku: "PART-001".into(),
                    qty: 10,
                    unit_cost_cents: 5_000,
                    currency: "USD".into(),
                }],
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        // Parts stock: sum invariants ----------------------------------------

        #[test]
        fn prop_parts_stock_sums_match(items in vec(arb_item(), 0..60)) {
            let summary = parts_stock_summary(&items);
            let expected_on_hand: i64 = items.iter().map(|i| i64::from(i.on_hand)).sum();
            let expected_allocated: i64 = items.iter().map(|i| i64::from(i.allocated)).sum();
            let expected_available: i64 = items
                .iter()
                .map(|i| i64::from(i.on_hand.saturating_sub(i.allocated)))
                .sum();
            prop_assert_eq!(summary.total_skus, items.len() as i64);
            prop_assert_eq!(summary.total_on_hand, expected_on_hand);
            prop_assert_eq!(summary.total_allocated, expected_allocated);
            prop_assert_eq!(summary.total_available, expected_available);
            prop_assert!(summary.total_available <= summary.total_on_hand);
        }

        // Parts stock: below-reorder count matches the predicate, preview
        // never exceeds the limit, preview rows are a subset of true hits.
        #[test]
        fn prop_parts_stock_below_reorder_invariants(items in vec(arb_item(), 0..60)) {
            let summary = parts_stock_summary(&items);
            let expected_hits = items
                .iter()
                .filter(|i| i.on_hand.saturating_sub(i.allocated) <= i.reorder_point)
                .count() as i64;
            prop_assert_eq!(summary.below_reorder_count, expected_hits);
            prop_assert!(summary.below_reorder_items.len() <= LOW_STOCK_PREVIEW_LIMIT);
            prop_assert!(summary.below_reorder_items.len() as i64 <= summary.below_reorder_count);
            // Every row exposed in the preview satisfies the predicate.
            for row in &summary.below_reorder_items {
                prop_assert!(row.available <= row.reorder_point);
            }
        }

        // Parts stock: preview is sorted most-severe first.
        #[test]
        fn prop_parts_stock_preview_sorted(items in vec(arb_item(), 0..60)) {
            let summary = parts_stock_summary(&items);
            let severities: Vec<i64> = summary
                .below_reorder_items
                .iter()
                .map(|r| i64::from(r.available) - i64::from(r.reorder_point))
                .collect();
            for pair in severities.windows(2) {
                prop_assert!(pair[0] <= pair[1], "preview must be sorted ascending by severity");
            }
        }

        // Inbound POs: open-status counts sum to total_open.
        #[test]
        fn prop_inbound_pos_status_counts_sum(pos in vec(arb_po(), 0..40)) {
            let today = d(2026, 4, 22);
            let summary = inbound_pos_summary(&pos, today);
            let sum = summary.draft_count
                + summary.submitted_count
                + summary.acknowledged_count
                + summary.in_transit_count;
            prop_assert_eq!(sum, summary.total_open);
        }

        // Inbound POs: total_open equals the count of non-terminal POs.
        #[test]
        fn prop_inbound_pos_total_matches_non_terminal(pos in vec(arb_po(), 0..40)) {
            let today = d(2026, 4, 22);
            let summary = inbound_pos_summary(&pos, today);
            let expected = pos
                .iter()
                .filter(|po| po.status.is_open())
                .count() as i64;
            prop_assert_eq!(summary.total_open, expected);
        }

        // Inbound POs: late + arriving-this-week never exceeds total_open
        // (they're disjoint subsets but neither can be > the whole).
        #[test]
        fn prop_inbound_pos_late_and_soon_bounded(pos in vec(arb_po(), 0..40)) {
            let today = d(2026, 4, 22);
            let summary = inbound_pos_summary(&pos, today);
            prop_assert!(summary.late_count <= summary.total_open);
            prop_assert!(summary.arriving_this_week_count <= summary.total_open);
            prop_assert!(
                summary.late_count + summary.arriving_this_week_count <= summary.total_open,
            );
        }

        // Inbound POs: preview respects the cap + is sorted by expected_on.
        #[test]
        fn prop_inbound_pos_preview_bounded_and_sorted(pos in vec(arb_po(), 0..40)) {
            let today = d(2026, 4, 22);
            let summary = inbound_pos_summary(&pos, today);
            prop_assert!(summary.recent.len() <= INBOUND_PO_PREVIEW_LIMIT);
            for pair in summary.recent.windows(2) {
                prop_assert!(pair[0].expected_on <= pair[1].expected_on);
            }
        }

        // build_warehouse_status: cross-service inputs pass through by value.
        #[test]
        fn prop_build_passes_through_cross_service_inputs(
            items in vec(arb_item(), 0..10),
            pos in vec(arb_po(), 0..10),
        ) {
            let outbound = OutboundShipmentSummary::default();
            let as_of = ts(2026, 4, 22);
            let status = build_warehouse_status(&items, &pos, outbound.clone(), as_of);
            prop_assert_eq!(&status.outbound_shipments, &outbound);
            prop_assert_eq!(status.as_of, as_of);
        }
    }
}
