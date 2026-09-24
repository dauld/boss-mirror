//! Hexagonal port: `ShippingRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use crate::types::{Shipment, ShipmentDirection};

#[derive(Debug, thiserror::Error)]
pub enum ShippingError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Persistence port for shipments.
///
/// Mutation methods come in two flavors: a convenience overload
/// that stamps `Utc::now()` server-side (with a platform-automation
/// event stamp — test-path ergonomics), and an `_at` variant that
/// takes an explicit timestamp plus the caller's [`EventStamp`] so
/// the projection write and the audit_log event share one timestamp —
/// required for the audit_log → projection rebuild path. See
/// `docs/design/projection-rebuilders.md`.
///
/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the
/// stamp (`boss_events::outbox::record_event_in_tx`);
/// boss-event-relay delivers to audit_log + NATS post-commit.
/// Nothing publishes post-commit.
#[async_trait]
pub trait ShippingRepository: Send + Sync {
    /// Return every shipment.
    async fn all_shipments(&self) -> Result<Vec<Shipment>, ShippingError>;

    /// Return a page of shipments with total count.
    /// `account_id` filters to a single account when `Some`. The account
    /// detail view uses this to scope the shipments section.
    async fn list_shipments(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Shipment>, i64), ShippingError>;

    /// Return a single shipment by ID, or `None` if not found.
    async fn shipment_by_id(&self, id: &str) -> Result<Option<Shipment>, ShippingError>;

    /// Create a new shipment. Returns the ID. Errors if ID already exists.
    /// Records `shipping.shipment.created` (full row state) in-tx.
    async fn create_shipment(&self, shipment: &Shipment) -> Result<String, ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.create_shipment_at(shipment, stamp.timestamp, &stamp)
            .await
    }
    async fn create_shipment_at(
        &self,
        shipment: &Shipment,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<String, ShippingError>;

    /// Replace a shipment by ID. Errors if ID doesn't exist.
    /// Records `shipping.shipment.updated` (full row state) in-tx.
    async fn update_shipment(&self, id: &str, shipment: &Shipment) -> Result<(), ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.update_shipment_at(id, shipment, stamp.timestamp, &stamp)
            .await
    }
    async fn update_shipment_at(
        &self,
        id: &str,
        shipment: &Shipment,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Delete a shipment and satellite data. Errors if ID doesn't exist.
    /// Records `shipping.shipment.deleted` (`{id, deleted_at}`) in-tx.
    async fn delete_shipment(&self, id: &str) -> Result<(), ShippingError> {
        let stamp = EventStamp::new("shipping", ActorId::Automation("platform".into()));
        self.delete_shipment_at(id, stamp.timestamp, &stamp).await
    }
    async fn delete_shipment_at(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Record one carrier scan for a shipment + roll up the
    /// shipment's `status` column when the scan moves it to a
    /// row-state-changing value (in-transit, delivered).
    /// Idempotent on (shipment_id, status, occurred_on).
    /// Errors with `NotFound` when the shipment doesn't exist
    /// (allows the HTTP layer to skip cleanly on out-of-order
    /// scan delivery).
    /// Records `shipping.tracking.recorded` in-tx — and ONLY when
    /// the scan row actually inserted, so an idempotent replay
    /// records nothing (the guard sits ahead of the recording).
    async fn record_tracking_scan(
        &self,
        shipment_id: &str,
        status: &str,
        occurred_on: chrono::NaiveDate,
        stage_index: Option<i16>,
        stamp: &EventStamp,
    ) -> Result<(), ShippingError>;

    /// Aggregate status summary for one direction — counts per status
    /// (in-flight only) + count of deliveries in the trailing 7 days +
    /// a top-N preview of recent rows (in-flight first, then recently
    /// delivered). Postgres backends should implement this with a
    /// GROUP BY + bounded LIMIT rather than fetching the full table
    /// and aggregating in Rust — at scale the shipments table reaches
    /// tens of thousands of rows and full-table scans trip the 5s
    /// client timeout.
    async fn status_summary(
        &self,
        direction: ShipmentDirection,
        today: chrono::NaiveDate,
        recent_limit: i64,
    ) -> Result<boss_shipping_client::OutboundShipmentSummary, ShippingError>;
}
