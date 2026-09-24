//! Property-based tests for the `warehouse_status` aggregator.
//!
//! Unit tests in `src/warehouse_status.rs` lock in specific scenarios.
//! These properties hold for *any* input — random bags of inventory
//! items and purchase orders — and crash-tighten the aggregator
//! against off-by-one, overflow, and zero-case bugs that the
//! hand-rolled unit tests can't cover exhaustively.
//!
//! Properties encoded:
//!
//! 1. `prop_parts_stock_totals_conserve` — the summary's
//!    `total_on_hand` / `total_allocated` / `total_available` /
//!    `total_skus` match the straight sums over the input items.
//!
//! 2. `prop_parts_stock_below_reorder_is_saturated` — `below_reorder_count`
//!    is exactly the number of items whose `available <= reorder_point`,
//!    regardless of how many appear in the preview slice.
//!
//! 3. `prop_parts_stock_preview_is_sorted_and_capped` — the preview
//!    list is ordered by severity (ascending `available - reorder_point`)
//!    and never exceeds `LOW_STOCK_PREVIEW_LIMIT`.
//!
//! 4. `prop_inbound_pos_bucket_partitioning` — the status-bucket
//!    counts (`draft + submitted + acknowledged + in_transit`) sum to
//!    `total_open`, and the date buckets (`late`, `arriving_this_week`)
//!    are each ≤ `total_open`.
//!
//! 5. `prop_inbound_pos_preview_sorted_and_capped` — `recent` is
//!    ordered by `expected_on` ascending and never exceeds
//!    `INBOUND_PO_PREVIEW_LIMIT`.
//!
//! 6. `prop_build_warehouse_status_passes_through` — the cross-service
//!    summary (`outbound_shipments`) is returned byte-identical to the
//!    caller — no silent dropping or reshuffling.

use boss_inventory::types::{InventoryItem, PoStatus, PurchaseOrder, PurchaseOrderLine};
use boss_inventory::warehouse_status::{
    INBOUND_PO_PREVIEW_LIMIT, LOW_STOCK_PREVIEW_LIMIT, OutboundShipmentSummary,
    build_warehouse_status,
};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Keep integer ranges well below u32::MAX / 2 so sums don't overflow
/// i64 even for very long vectors. The production numbers are tiny
/// (stock in the hundreds, allocations in the tens); 100_000 is already
/// a million-x safety margin.
fn arb_item() -> impl Strategy<Value = InventoryItem> {
    (
        "[A-Z]{3}-[0-9]{1,4}",
        "[A-Z]-[0-9]{1,3}",
        0u32..100_000,
        0u32..100_000,
        0u32..100_000,
    )
        .prop_map(
            |(sku, bin, on_hand, allocated, reorder_point)| InventoryItem {
                part_sku: sku,
                bin,
                on_hand,
                allocated,
                reorder_point,
                reorder_qty: 100,
                trailing_90d_usage: 30,
                value_cents: 0,
                avg_cost_cents: 0,
                vendor_price_cents: None,
                vendor_category: None,
            },
        )
}

fn arb_po_status() -> impl Strategy<Value = PoStatus> {
    prop_oneof![
        Just(PoStatus::new(PoStatus::DRAFT)),
        Just(PoStatus::new(PoStatus::SUBMITTED)),
        Just(PoStatus::new(PoStatus::ACKNOWLEDGED)),
        Just(PoStatus::new(PoStatus::IN_TRANSIT)),
        Just(PoStatus::new(PoStatus::RECEIVED)),
        Just(PoStatus::new(PoStatus::CLOSED)),
    ]
}

/// Dates within a ~2-year window of today. Production POs don't span
/// decades; constraining the range keeps `.num_days()` arithmetic inside
/// the i64 range trivially.
fn arb_date(anchor: NaiveDate) -> impl Strategy<Value = NaiveDate> {
    (-365i64..365).prop_map(move |d| anchor + Duration::days(d))
}

fn arb_po(today: NaiveDate) -> impl Strategy<Value = PurchaseOrder> {
    (
        "[A-Z]{2}-[0-9]{1,5}",
        "[A-Z][a-z]{2,8}",
        arb_po_status(),
        arb_date(today),
        arb_date(today),
    )
        .prop_map(
            |(id, vendor, status, placed_on, expected_on)| PurchaseOrder {
                id,
                vendor: Some(vendor),
                status,
                placed_on: Some(placed_on),
                expected_on: Some(expected_on),
                received_on: None,
                lines: vec![PurchaseOrderLine {
                    part_sku: "PART-001".into(),
                    qty: 10,
                    unit_cost_cents: 5_000,
                    currency: "USD".into(),
                }],
            },
        )
}

fn arb_items(n: usize) -> impl Strategy<Value = Vec<InventoryItem>> {
    prop::collection::vec(arb_item(), 0..=n)
}

fn arb_pos(n: usize, today: NaiveDate) -> impl Strategy<Value = Vec<PurchaseOrder>> {
    prop::collection::vec(arb_po(today), 0..=n)
}

fn empty_outbound() -> OutboundShipmentSummary {
    OutboundShipmentSummary {
        label_created: 0,
        picked_up: 0,
        in_transit: 0,
        exception: 0,
        delivered_7d: 0,
        recent: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

fn today_anchor() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 4, 24).unwrap()
}

fn as_of() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_parts_stock_totals_conserve(items in arb_items(60)) {
        let status = build_warehouse_status(
            &items,
            &[],
            empty_outbound(),
            as_of(),
        );
        let ps = &status.parts_stock;

        let expected_on_hand: i64 = items.iter().map(|i| i64::from(i.on_hand)).sum();
        let expected_allocated: i64 = items.iter().map(|i| i64::from(i.allocated)).sum();
        // `available = on_hand.saturating_sub(allocated)` — when allocated
        // > on_hand the available stays at zero rather than going negative.
        let expected_available: i64 = items
            .iter()
            .map(|i| i64::from(i.on_hand.saturating_sub(i.allocated)))
            .sum();

        prop_assert_eq!(ps.total_skus, items.len() as i64);
        prop_assert_eq!(ps.total_on_hand, expected_on_hand);
        prop_assert_eq!(ps.total_allocated, expected_allocated);
        prop_assert_eq!(ps.total_available, expected_available);
    }

    #[test]
    fn prop_parts_stock_below_reorder_is_saturated(items in arb_items(60)) {
        let status = build_warehouse_status(
            &items,
            &[],
            empty_outbound(),
            as_of(),
        );
        let ps = &status.parts_stock;

        let expected_below: i64 = items
            .iter()
            .filter(|i| i.on_hand.saturating_sub(i.allocated) <= i.reorder_point)
            .count() as i64;

        prop_assert_eq!(ps.below_reorder_count, expected_below);
        // The preview is a subset — never more than the count.
        prop_assert!((ps.below_reorder_items.len() as i64) <= ps.below_reorder_count);
    }

    #[test]
    fn prop_parts_stock_preview_is_sorted_and_capped(items in arb_items(60)) {
        let status = build_warehouse_status(
            &items,
            &[],
            empty_outbound(),
            as_of(),
        );
        let preview = &status.parts_stock.below_reorder_items;

        // Cap.
        prop_assert!(preview.len() <= LOW_STOCK_PREVIEW_LIMIT);

        // Every item really is below reorder.
        for item in preview {
            prop_assert!(item.available <= item.reorder_point);
        }

        // Ascending by (available - reorder_point) — severity first.
        for w in preview.windows(2) {
            let a = i64::from(w[0].available) - i64::from(w[0].reorder_point);
            let b = i64::from(w[1].available) - i64::from(w[1].reorder_point);
            prop_assert!(a <= b, "preview not sorted: {a} > {b}");
        }
    }

    #[test]
    fn prop_inbound_pos_bucket_partitioning(pos in arb_pos(40, today_anchor())) {
        let status = build_warehouse_status(
            &[],
            &pos,
            empty_outbound(),
            as_of(),
        );
        let ipo = &status.inbound_pos;

        // `total_open` counts every PO that isn't Received or Closed.
        let expected_open: i64 = pos
            .iter()
            .filter(|p| p.status.is_open())
            .count() as i64;
        prop_assert_eq!(ipo.total_open, expected_open);

        // The status-bucket counts partition `total_open` — draft +
        // submitted + acknowledged + in_transit equals the whole set.
        let bucket_sum =
            ipo.draft_count + ipo.submitted_count + ipo.acknowledged_count + ipo.in_transit_count;
        prop_assert_eq!(bucket_sum, ipo.total_open);

        // Date buckets are each bounded by `total_open` — every late PO
        // is also open, and every arriving-this-week PO is open. The
        // two buckets are mutually exclusive (one ≤ today, one > today)
        // so they can't both cover the same row.
        prop_assert!(ipo.late_count <= ipo.total_open);
        prop_assert!(ipo.arriving_this_week_count <= ipo.total_open);
        prop_assert!(ipo.late_count + ipo.arriving_this_week_count <= ipo.total_open);
    }

    #[test]
    fn prop_inbound_pos_preview_sorted_and_capped(pos in arb_pos(40, today_anchor())) {
        let status = build_warehouse_status(
            &[],
            &pos,
            empty_outbound(),
            as_of(),
        );
        let recent = &status.inbound_pos.recent;

        // Cap.
        prop_assert!(recent.len() <= INBOUND_PO_PREVIEW_LIMIT);
        // Bounded by open.
        prop_assert!((recent.len() as i64) <= status.inbound_pos.total_open);

        // Ascending by expected_on.
        for w in recent.windows(2) {
            prop_assert!(w[0].expected_on <= w[1].expected_on);
        }
    }

    #[test]
    fn prop_build_warehouse_status_passes_through(
        in_transit in 0i64..10_000,
        delivered_7d in 0i64..10_000,
    ) {
        // The cross-service input is opaque to the aggregator — it
        // must return it byte-identical in the response. Regression
        // guard against accidental reshuffling / dropping.
        let outbound = OutboundShipmentSummary {
            in_transit,
            delivered_7d,
            ..empty_outbound()
        };
        let status = build_warehouse_status(
            &[],
            &[],
            outbound.clone(),
            as_of(),
        );
        prop_assert_eq!(status.outbound_shipments, outbound);
    }
}
