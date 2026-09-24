//! Shared test scaffolding for the commerce crate.
//!
//! Provides:
//! - `CommerceTestApp` builder wiring InMemoryCommerce + HTTP router
//!   + RecordingEventBus + DomainPublisher
//! - `invoice_fixture()` and `opportunity_fixture()` helpers
//!
//! `#[allow(dead_code)]`: Rust sees each `tests/*.rs` binary as its
//! own compilation unit, and some helpers here are only used by a
//! subset — unused-in-this-binary ones fire dead_code even though
//! every helper is used somewhere in the test tree.
#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use boss_commerce::InMemoryCommerce;
use boss_commerce::http::{CommerceApiState, router};
use boss_commerce::types::*;
use boss_core::publisher::DomainPublisher;
use boss_people_client::{FakePeopleClient, PeopleClient};
use boss_policy_client::{PermissivePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;

/// A fully wired commerce service for tests.
pub struct CommerceTestApp {
    pub router: Router,
    pub bus: Arc<RecordingEventBus>,
    /// The adapter behind the router — outbox phase 2 records the
    /// four invoice kinds HERE (the in-memory analogue of
    /// event_outbox), not on the bus.
    pub repo: Arc<InMemoryCommerce>,
}

impl CommerceTestApp {
    /// Assert exactly the outbox-recorded events of `kind` and return
    /// them — the phase-2 analogue of `bus.assert_event_emitted`.
    pub fn recorded_of_kind(&self, kind: &str) -> Vec<boss_core::event::Event> {
        self.repo
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == kind)
            .collect()
    }

    pub fn assert_recorded(&self, kind: &str) -> boss_core::event::Event {
        let mut events = self.recorded_of_kind(kind);
        assert!(
            !events.is_empty(),
            "expected an outbox-recorded `{kind}` event; recorded kinds: {:?}",
            self.repo
                .recorded_events()
                .iter()
                .map(|e| e.kind.clone())
                .collect::<Vec<_>>()
        );
        events.remove(0)
    }
}

impl CommerceTestApp {
    /// Build a fresh test app with empty invoices.
    pub fn new() -> Self {
        Self::build(vec![])
    }

    /// Build a test app pre-populated with invoices.
    pub fn with_invoices(invoices: Vec<Invoice>) -> Self {
        Self::build(invoices)
    }

    fn build(invoices: Vec<Invoice>) -> Self {
        let people_client: Arc<dyn PeopleClient> =
            Arc::new(FakePeopleClient::new().with_account("account-001"));
        Self::with_people_client(invoices, people_client)
    }

    pub fn with_people_client(
        invoices: Vec<Invoice>,
        people_client: Arc<dyn PeopleClient>,
    ) -> Self {
        let commerce = Arc::new(InMemoryCommerce::new(invoices));
        let repo = commerce.clone();
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "commerce");
        let policy: Arc<dyn PolicyClient> = Arc::new(PermissivePolicyClient);
        let state = CommerceApiState {
            commerce,
            publisher: Some(publisher),
            people_client,
            policy: Some(policy),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        };
        let router = router(state);
        Self { router, bus, repo }
    }
}

/// Build a valid Invoice suitable for create/paid tests. Single
/// line item carrying the full amount so the sum-matches invariant
/// in `create_invoice` is satisfied.
pub fn invoice_fixture(id: &str) -> Invoice {
    Invoice {
        id: id.to_string(),
        account_id: "account-001".to_string(),
        issued_on: chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
        due_on: chrono::NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 1_200_000,
        currency: "USD".to_string(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: format!("{id}-l1"),
            invoice_id: id.to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 1_200_000,
            currency: "USD".to_string(),
            description: "Device sale".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    }
}
