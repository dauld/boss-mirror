//! Cross-entity device-insights projection.
//!
//! Backs `GET /api/assets/{asset_id}/insights` and the Insights
//! section of the SR 360 + Device 360 views (operations-needs session
//! 3, E4). Given a serial, assembles:
//!
//! - Prior failure modes on the device's model (ranked by frequency)
//! - Likely-failure parts on-hand (high-usage spare parts joined with
//!   inventory stock)
//!
//! A third section, service history, listed prior `field-service`
//! Jobs on the serial. `field-service` was authored only in the
//! retiring device-shop example's seed bundle — no instance publishes
//! it (the Algedonic instance read 0 such Jobs and no Workflow row,
//! 2026-09-24) — so the section, its jobs-API fan-out and its
//! properties were retired with that tenant (backlog a8991c86, car 3).
//!
//! Device usage hours and contract coverage are v1.1 follow-ups —
//! assets doesn't track hours yet, and contract coverage needs a
//! assets → commerce hop that we'll add once the single-endpoint
//! pattern has proven itself.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Re-exported so callers can import the nested types from one place
// and the wire shape has one Rust-side anchor.
pub use boss_catalog_client::{CatalogModelSummary, FailureMode, SparePart};
pub use boss_inventory_client::PartStockLevel;

/// Top-of-wire response shape for `GET /api/assets/{asset_id}/insights`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInsights {
    pub serial: String,
    pub model_sku: Option<String>,
    pub model_name: Option<String>,
    pub manufacturer: Option<String>,
    pub prior_failure_modes: Vec<FailureModeInsight>,
    pub likely_failure_parts: Vec<LikelyFailurePart>,
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureModeInsight {
    pub code: String,
    pub name: String,
    pub frequency: f32,
    pub typical_fix: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LikelyFailurePart {
    pub part_sku: String,
    pub name: String,
    pub lead_time_days: u16,
    pub high_usage: bool,
    /// `None` when the part isn't stocked — caller renders "not
    /// stocked, lead time N days" with an emphasis on the lead time.
    pub stock: Option<PartStockLevel>,
}

pub const FAILURE_MODE_PREVIEW_LIMIT: usize = 5;

/// Combine the pre-fetched inputs into the wire shape. Pure function —
/// no I/O, so tests can pin every ranking + join rule deterministically.
pub fn build_asset_insights(
    serial: String,
    model: Option<CatalogModelSummary>,
    parts_stock: Vec<PartStockLevel>,
    as_of: DateTime<Utc>,
) -> DeviceInsights {
    let (model_sku, model_name, manufacturer, prior_failure_modes, likely_failure_parts) =
        match model {
            Some(m) => {
                let modes = rank_failure_modes(&m.failure_modes);
                let parts = join_likely_failure_parts(&m.spare_parts, &parts_stock);
                (
                    Some(m.sku),
                    Some(m.name),
                    Some(m.manufacturer),
                    modes,
                    parts,
                )
            }
            None => (None, None, None, Vec::new(), Vec::new()),
        };

    DeviceInsights {
        serial,
        model_sku,
        model_name,
        manufacturer,
        prior_failure_modes,
        likely_failure_parts,
        as_of,
    }
}

/// Most-frequent-first, capped at `FAILURE_MODE_PREVIEW_LIMIT`.
fn rank_failure_modes(modes: &[FailureMode]) -> Vec<FailureModeInsight> {
    let mut ranked: Vec<&FailureMode> = modes.iter().collect();
    // Sort by frequency descending — `partial_cmp` handles f32 safely
    // (no NaN frequency values are emitted by the catalog seed data).
    ranked.sort_by(|a, b| {
        b.frequency
            .partial_cmp(&a.frequency)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
        .into_iter()
        .take(FAILURE_MODE_PREVIEW_LIMIT)
        .map(|m| FailureModeInsight {
            code: m.code.clone(),
            name: m.name.clone(),
            frequency: m.frequency,
            typical_fix: m.typical_fix.clone(),
        })
        .collect()
}

/// For each `high_usage` spare part, attach the stock level (if any).
/// Non-high-usage parts are dropped — the Insights section is narrowly
/// about what's likely to be needed on the next visit, not the full BOM.
fn join_likely_failure_parts(
    bom: &[SparePart],
    stock: &[PartStockLevel],
) -> Vec<LikelyFailurePart> {
    let by_sku: std::collections::HashMap<&str, &PartStockLevel> =
        stock.iter().map(|s| (s.part_sku.as_str(), s)).collect();
    bom.iter()
        .filter(|p| p.high_usage)
        .map(|p| LikelyFailurePart {
            part_sku: p.part_sku.clone(),
            name: p.name.clone(),
            lead_time_days: p.lead_time_days,
            high_usage: p.high_usage,
            stock: by_sku.get(p.part_sku.as_str()).map(|s| (*s).clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ts(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
    }

    fn fm(code: &str, freq: f32) -> FailureMode {
        FailureMode {
            code: code.to_string(),
            name: format!("mode {code}"),
            frequency: freq,
            typical_fix: "fix".to_string(),
        }
    }

    fn part(sku: &str, high_usage: bool, lead: u16) -> SparePart {
        SparePart {
            part_sku: sku.to_string(),
            name: format!("Part {sku}"),
            lead_time_days: lead,
            high_usage,
        }
    }

    fn stk(sku: &str, on_hand: u32, available: u32, below: bool) -> PartStockLevel {
        PartStockLevel {
            part_sku: sku.to_string(),
            on_hand,
            allocated: on_hand - available,
            available,
            reorder_point: 5,
            below_reorder: below,
        }
    }

    // --- failure-mode ranking ---------------------------------------------

    #[test]
    fn failure_modes_ranked_highest_frequency_first_and_capped() {
        let model = CatalogModelSummary {
            sku: "LUM".into(),
            name: "LUM".into(),
            manufacturer: "x".into(),
            failure_modes: vec![
                fm("C", 0.05),
                fm("A", 0.30),
                fm("D", 0.02),
                fm("B", 0.25),
                fm("E", 0.20),
                fm("F", 0.10),
                fm("G", 0.15),
            ],
            spare_parts: vec![],
        };
        let insights = build_asset_insights("SN-1".into(), Some(model), vec![], ts(2026, 4, 22));
        let codes: Vec<&str> = insights
            .prior_failure_modes
            .iter()
            .map(|m| m.code.as_str())
            .collect();
        assert_eq!(codes, vec!["A", "B", "E", "G", "F"]);
        assert_eq!(
            insights.prior_failure_modes.len(),
            FAILURE_MODE_PREVIEW_LIMIT
        );
    }

    // --- likely-failure-parts join -----------------------------------------

    #[test]
    fn likely_failure_parts_filters_to_high_usage_and_joins_stock() {
        let model = CatalogModelSummary {
            sku: "LUM".into(),
            name: "LUM".into(),
            manufacturer: "x".into(),
            failure_modes: vec![],
            spare_parts: vec![
                part("P-HIGH", true, 7),
                part("P-LOW", false, 30),
                part("P-UNSTOCKED", true, 14),
            ],
        };
        let stock = vec![stk("P-HIGH", 10, 8, false)];
        let insights = build_asset_insights("SN-1".into(), Some(model), stock, ts(2026, 4, 22));
        let skus: Vec<&str> = insights
            .likely_failure_parts
            .iter()
            .map(|p| p.part_sku.as_str())
            .collect();
        assert_eq!(skus, vec!["P-HIGH", "P-UNSTOCKED"]);
        let by: std::collections::HashMap<_, _> = insights
            .likely_failure_parts
            .iter()
            .map(|p| (p.part_sku.as_str(), p))
            .collect();
        assert!(by["P-HIGH"].stock.is_some());
        assert_eq!(by["P-HIGH"].stock.as_ref().unwrap().available, 8);
        assert!(by["P-UNSTOCKED"].stock.is_none());
    }

    // --- missing model -----------------------------------------------------

    #[test]
    fn missing_model_zeroes_model_blocks() {
        let insights = build_asset_insights("SN-OPAQUE".into(), None, vec![], ts(2026, 4, 22));
        assert_eq!(insights.serial, "SN-OPAQUE");
        assert!(insights.model_sku.is_none());
        assert!(insights.prior_failure_modes.is_empty());
        assert!(insights.likely_failure_parts.is_empty());
    }

    #[test]
    fn as_of_round_trips_through_builder() {
        let when = ts(2026, 4, 22);
        let insights = build_asset_insights("SN-1".into(), None, vec![], when);
        assert_eq!(insights.as_of, when);
    }

    // ---------------------------------------------------------------------
    // Property-based tests
    // ---------------------------------------------------------------------

    use proptest::collection::vec;
    use proptest::prelude::*;

    prop_compose! {
        fn arb_failure_mode()(
            code in "[A-Z]{2,4}",
            freq in 0.0f32..1.0,
        ) -> FailureMode {
            fm(&code, freq)
        }
    }

    prop_compose! {
        fn arb_spare_part()(
            sku in "P-[A-Z]{2,4}",
            lead in 0u16..365,
            high in any::<bool>(),
        ) -> SparePart {
            part(&sku, high, lead)
        }
    }

    prop_compose! {
        fn arb_stock()(
            sku in "P-[A-Z]{2,4}",
            on_hand in 0u32..1_000,
            available_frac in 0.0f32..1.0,
        ) -> PartStockLevel {
            let available = (on_hand as f32 * available_frac) as u32;
            stk(&sku, on_hand, available, on_hand < 5)
        }
    }

    fn model_with(modes: Vec<FailureMode>, parts: Vec<SparePart>) -> CatalogModelSummary {
        CatalogModelSummary {
            sku: "LUM".into(),
            name: "LUM".into(),
            manufacturer: "x".into(),
            failure_modes: modes,
            spare_parts: parts,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        // Failure-mode ranking is descending by frequency and never
        // exceeds the preview cap.
        #[test]
        fn prop_failure_modes_sorted_desc_and_capped(
            modes in vec(arb_failure_mode(), 0..20),
        ) {
            let insights = build_asset_insights(
                "SN-1".into(),
                Some(model_with(modes, vec![])),
                vec![], ts(2026, 4, 22),
            );
            let ranked = &insights.prior_failure_modes;
            prop_assert!(ranked.len() <= FAILURE_MODE_PREVIEW_LIMIT);
            for pair in ranked.windows(2) {
                prop_assert!(
                    pair[0].frequency >= pair[1].frequency,
                    "ranking must be non-increasing by frequency",
                );
            }
        }

        // Likely-failure-parts: only high_usage parts pass; every entry
        // has `high_usage=true`.
        #[test]
        fn prop_likely_parts_filters_to_high_usage(
            parts in vec(arb_spare_part(), 0..20),
            stock in vec(arb_stock(), 0..20),
        ) {
            let high_count = parts.iter().filter(|p| p.high_usage).count();
            let insights = build_asset_insights(
                "SN-1".into(),
                Some(model_with(vec![], parts)),
                stock, ts(2026, 4, 22),
            );
            prop_assert_eq!(insights.likely_failure_parts.len(), high_count);
            for row in &insights.likely_failure_parts {
                prop_assert!(row.high_usage);
            }
        }

        // Missing model collapses every model-dependent field.
        #[test]
        fn prop_missing_model_zeroes_model_fields(
            stock in vec(arb_stock(), 0..10),
        ) {
            let insights = build_asset_insights(
                "SN-OPAQUE".into(), None, stock, ts(2026, 4, 22),
            );
            prop_assert!(insights.model_sku.is_none());
            prop_assert!(insights.model_name.is_none());
            prop_assert!(insights.manufacturer.is_none());
            prop_assert!(insights.prior_failure_modes.is_empty());
            prop_assert!(insights.likely_failure_parts.is_empty());
        }

        // Serial + as_of pass through by value regardless of other inputs.
        #[test]
        fn prop_serial_and_as_of_round_trip(
            serial in "SN-[0-9]{1,6}",
            modes in vec(arb_failure_mode(), 0..10),
            parts in vec(arb_spare_part(), 0..10),
            stock in vec(arb_stock(), 0..10),
        ) {
            let when = ts(2026, 4, 22);
            let insights = build_asset_insights(
                serial.clone(),
                Some(model_with(modes, parts)),
                stock, when,
            );
            prop_assert_eq!(insights.serial, serial);
            prop_assert_eq!(insights.as_of, when);
        }
    }
}
