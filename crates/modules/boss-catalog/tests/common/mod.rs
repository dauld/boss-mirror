//! Shared test scaffolding for the knowledge-base crate.
//!
//! Provides:
//! - `KbTestApp` builder that wires InMemoryKb + HTTP router + a stub
//!   AssetsClient (publisher `None` — outbox phase 2: adapters record
//!   events in the domain write, so the in-memory repo's
//!   `recorded_events()` is the assertion surface, not a bus)
//! - `model_fixture()` and helpers to build valid AssetModel instances
//!
//! Delete-guard tests use the shipped `boss_assets_client::FakeAssetsClient`.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use boss_assets_client::{AssetsClient, AssetsClientError};
use boss_catalog::InMemoryKb;
use boss_catalog::http::{KbApiState, router};
use boss_catalog::types::*;

/// Stub AssetsClient that reports zero for every call. Used by
/// kb tests that don't exercise the delete guard path — they
/// just need a client that satisfies the trait bound. Tests that
/// exercise the guard use `boss_assets_client::FakeAssetsClient` instead.
struct StubAssetsClient;

#[async_trait]
impl AssetsClient for StubAssetsClient {
    async fn open_ticket_count_for_account(
        &self,
        _account_id: &str,
    ) -> Result<u64, AssetsClientError> {
        Ok(0)
    }
    async fn active_asset_count_for_sku(&self, _sku: &str) -> Result<u64, AssetsClientError> {
        Ok(0)
    }
}

/// A fully wired knowledge-base service for tests:
/// - InMemoryKb repository (collects outbox-recorded events)
/// - Axum Router ready to accept requests
pub struct KbTestApp {
    pub router: Router,
    pub catalog: Arc<InMemoryKb>,
}

impl KbTestApp {
    /// Build a fresh test app with an empty kb.
    pub fn new() -> Self {
        Self::with_models(vec![])
    }

    /// Build a test app pre-populated with the given models.
    pub fn with_models(models: Vec<AssetModel>) -> Self {
        let catalog = Arc::new(InMemoryKb::new(models));
        let state = KbApiState {
            catalog: catalog.clone(),
            publisher: None,
            assets_client: Arc::new(StubAssetsClient),
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, catalog }
    }

    /// Assert exactly-one recorded event of `kind` and return it.
    pub fn assert_recorded(&self, kind: &str) -> boss_core::event::Event {
        let matches: Vec<_> = self
            .catalog
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == kind)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one recorded `{kind}` event, got {}",
            matches.len()
        );
        matches.into_iter().next().unwrap()
    }

    /// Assert no recorded event of `kind`.
    pub fn assert_not_recorded(&self, kind: &str) {
        assert!(
            !self
                .catalog
                .recorded_events()
                .iter()
                .any(|e| e.kind == kind),
            "expected no recorded `{kind}` event"
        );
    }
}

/// Build a valid AssetModel suitable for create/update tests.
/// Provides sensible defaults for every required field.
pub fn model_fixture(sku: &str) -> AssetModel {
    AssetModel {
        sku: sku.to_string(),
        name: format!("Test Model {sku}"),
        manufacturer: "TestCo".to_string(),
        model_year: 2024,
        category: DeviceCategory::new("router"),
        extras: serde_json::json!({"port_count": 24}),
        physical: Physical {
            width_cm: 50.0,
            depth_cm: 50.0,
            height_cm: 100.0,
            weight_kg: 80.0,
            power_requirements: "120V".to_string(),
        },
        regulatory: Regulatory {
            clearance_id: None,
            clearance_date: None,
            regulator_device_class: 2,
        },
        commerce: Commerce {
            list_price_new_cents: 5_000_000,
            typical_refurb_price_cents: None,
            currency: "USD".to_string(),
            lead_time_days: None,
            tagline: "Fixture model for tests".to_string(),
            description: "A model used by automated tests.".to_string(),
            use_cases: vec![],
            hero_image: None,
        },
        service: ServiceProfile {
            preventive_maintenance_hours: 2.0,
            preventive_maintenance_interval_months: 6,
            calibration_interval_months: 12,
            required_skill_level: 3,
            depot_required: false,
            common_failure_modes: vec![],
            pm_checklist: vec![],
        },
        spare_parts: vec![],
        consumables: vec![],
        documents: vec![],
        end_of_support: None,
        current_firmware: None,
    }
}
