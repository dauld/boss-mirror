//! Property-based tests for the `asset_insights` aggregator.
//!
//! Unit tests in `src/asset_insights.rs` lock in scenarios with a
//! known model + fixtures. These properties hold for any input and
//! crash-tighten the ranker + joiner against overflow, NaN, and
//! off-by-one bugs.
//!
//! Properties encoded:
//!
//! 1. `prop_service_history_total_counts_input_exactly` — the
//!    summary's `total` equals the input slice length, even when
//!    that exceeds the preview cap.
//!
//! 2. `prop_service_history_preview_is_capped` — `recent` never
//!    exceeds `SERVICE_HISTORY_PREVIEW_LIMIT` and never exceeds
//!    `total`.
//!
//! 3. `prop_failure_modes_sorted_descending_and_capped` — ranked
//!    output is ordered by frequency descending and capped at
//!    `FAILURE_MODE_PREVIEW_LIMIT`.
//!
//! 4. `prop_likely_failure_parts_only_high_usage` — the join filter
//!    never emits a part whose `high_usage` is false, regardless of
//!    how the stock levels line up.
//!
//! 5. `prop_missing_model_empties_model_fields` — when `model =
//!    None`, the response has empty failure-mode + likely-parts
//!    vectors and `None` for model_sku / model_name / manufacturer.

use boss_assets::asset_insights::{
    CatalogModelSummary, FAILURE_MODE_PREVIEW_LIMIT, FailureMode, PartStockLevel,
    SERVICE_HISTORY_PREVIEW_LIMIT, ServiceHistoryRow, SparePart, build_asset_insights,
};
use chrono::{NaiveDate, TimeZone, Utc};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn arb_history_row() -> impl Strategy<Value = ServiceHistoryRow> {
    (
        "[A-Z]{2}-[0-9]{1,5}",
        "[A-Z][a-z]{2,10}",
        prop_oneof![
            Just("emergency".to_string()),
            Just("urgent".to_string()),
            Just("standard".to_string()),
            Just("scheduled".to_string()),
        ],
        prop_oneof![
            Just("open".to_string()),
            Just("closed".to_string()),
            Just("draft".to_string()),
        ],
        (-365i64..365).prop_map(|d| {
            NaiveDate::from_ymd_opt(2026, 4, 24).unwrap() + chrono::Duration::days(d)
        }),
    )
        .prop_map(
            |(job_id, title, priority, status, opened_on)| ServiceHistoryRow {
                job_id,
                title,
                priority,
                status,
                opened_on,
                closed_on: None,
            },
        )
}

fn arb_failure_mode() -> impl Strategy<Value = FailureMode> {
    (
        "[A-Z]{2,4}-[0-9]{1,3}",
        "[A-Z][a-z]{2,15}",
        // Constrain frequency to a well-formed probability range; this
        // matches the catalog seed data. NaN / negative values are not
        // something the catalog emits and are out of scope.
        0.0f32..1.0,
        "[A-Z][a-z]{3,20}",
    )
        .prop_map(|(code, name, frequency, typical_fix)| FailureMode {
            code,
            name,
            frequency,
            typical_fix,
        })
}

fn arb_spare_part() -> impl Strategy<Value = SparePart> {
    (
        "PART-[0-9]{3,5}",
        "[A-Z][a-z]{3,15}",
        0u16..365,
        any::<bool>(),
    )
        .prop_map(|(part_sku, name, lead_time_days, high_usage)| SparePart {
            part_sku,
            name,
            lead_time_days,
            high_usage,
        })
}

fn arb_stock_level() -> impl Strategy<Value = PartStockLevel> {
    ("PART-[0-9]{3,5}", 0u32..10_000, 0u32..10_000, 0u32..10_000).prop_map(
        |(part_sku, on_hand, allocated, reorder_point)| {
            let available = on_hand.saturating_sub(allocated);
            PartStockLevel {
                part_sku,
                on_hand,
                allocated,
                available,
                reorder_point,
                below_reorder: available <= reorder_point,
            }
        },
    )
}

fn arb_model() -> impl Strategy<Value = CatalogModelSummary> {
    (
        "SKU-[A-Z0-9]{3,8}",
        "[A-Z][a-z]{3,12}",
        "[A-Z][a-z]{3,10}",
        prop::collection::vec(arb_failure_mode(), 0..12),
        prop::collection::vec(arb_spare_part(), 0..12),
    )
        .prop_map(|(sku, name, manufacturer, failure_modes, spare_parts)| {
            CatalogModelSummary {
                sku,
                name,
                manufacturer,
                failure_modes,
                spare_parts,
            }
        })
}

fn as_of() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap()
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_service_history_total_counts_input_exactly(
        history in prop::collection::vec(arb_history_row(), 0..50),
    ) {
        let total_in = history.len() as i64;
        let insights = build_asset_insights(
            "SN-001".into(),
            None,
            history,
            Vec::new(),
            as_of(),
        );
        prop_assert_eq!(insights.service_history.total, total_in);
    }

    #[test]
    fn prop_service_history_preview_is_capped(
        history in prop::collection::vec(arb_history_row(), 0..50),
    ) {
        let insights = build_asset_insights(
            "SN-001".into(),
            None,
            history,
            Vec::new(),
            as_of(),
        );
        let recent = &insights.service_history.recent;
        prop_assert!(recent.len() <= SERVICE_HISTORY_PREVIEW_LIMIT);
        prop_assert!((recent.len() as i64) <= insights.service_history.total);
    }

    #[test]
    fn prop_failure_modes_sorted_descending_and_capped(model in arb_model()) {
        let insights = build_asset_insights(
            "SN-001".into(),
            Some(model),
            Vec::new(),
            Vec::new(),
            as_of(),
        );
        let modes = &insights.prior_failure_modes;

        prop_assert!(modes.len() <= FAILURE_MODE_PREVIEW_LIMIT);

        // Descending by frequency — most common first.
        for w in modes.windows(2) {
            prop_assert!(
                w[0].frequency >= w[1].frequency,
                "failure modes not descending: {} < {}",
                w[0].frequency,
                w[1].frequency,
            );
        }
    }

    #[test]
    fn prop_likely_failure_parts_only_high_usage(
        model in arb_model(),
        stock in prop::collection::vec(arb_stock_level(), 0..20),
    ) {
        let insights = build_asset_insights(
            "SN-001".into(),
            Some(model.clone()),
            Vec::new(),
            stock,
            as_of(),
        );
        // Every emitted part must have high_usage = true.
        for p in &insights.likely_failure_parts {
            prop_assert!(p.high_usage, "non-high-usage part leaked into output: {}", p.part_sku);
        }
        // And the emitted set is a subset of the model's high-usage
        // spare parts — no phantom rows introduced.
        let expected_skus: std::collections::HashSet<&str> = model
            .spare_parts
            .iter()
            .filter(|p| p.high_usage)
            .map(|p| p.part_sku.as_str())
            .collect();
        for p in &insights.likely_failure_parts {
            prop_assert!(
                expected_skus.contains(p.part_sku.as_str()),
                "unexpected part in output: {}",
                p.part_sku,
            );
        }
    }

    #[test]
    fn prop_missing_model_empties_model_fields(
        history in prop::collection::vec(arb_history_row(), 0..5),
        stock in prop::collection::vec(arb_stock_level(), 0..5),
    ) {
        // When no model is supplied, the aggregator has nothing to rank
        // or join against — every model-derived field must be empty
        // regardless of how much history or stock noise is passed in.
        let insights = build_asset_insights(
            "SN-MISSING".into(),
            None,
            history,
            stock,
            as_of(),
        );
        prop_assert_eq!(insights.model_sku, None);
        prop_assert_eq!(insights.model_name, None);
        prop_assert_eq!(insights.manufacturer, None);
        prop_assert!(insights.prior_failure_modes.is_empty());
        prop_assert!(insights.likely_failure_parts.is_empty());
    }
}
