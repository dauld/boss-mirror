//! Hexagonal port: `AssetsRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod) implement
//! this trait.

use async_trait::async_trait;

use crate::types::{AssetCurrentState, AssetEvent, AssetId, AssetsSummary};

#[derive(Debug, thiserror::Error)]
pub enum AssetsError {
    #[error("unknown asset: {0}")]
    UnknownSystem(AssetId),
    #[error("duplicate event id: {0}")]
    DuplicateEvent(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// Result of a `batch_append` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchAppendStats {
    pub inserted: u64,
    pub duplicates: u64,
}

/// Persistence port for the assets event log + current-state projection.
///
/// Implementations MUST maintain the invariant that the event log is
/// append-only: once stored, an event's payload never changes. The
/// current-state projection is either stored alongside (and updated as
/// events are appended) or computed on demand from the log.
#[async_trait]
pub trait AssetsRepository: Send + Sync {
    /// Append one event to a asset's log. Creates the asset's log on
    /// first event. Returns `DuplicateEvent` if the event id already exists.
    async fn append(&self, event: AssetEvent) -> Result<(), AssetsError>;

    /// Append a batch of events. Implementations are free to choose a
    /// faster bulk path; the contract is "same observable result as
    /// calling `append` once per event in the input order, except that
    /// duplicates do not error out — they're counted and skipped."
    ///
    /// Default implementation loops `append` for adapters that don't
    /// have a real bulk path.
    async fn batch_append(&self, events: Vec<AssetEvent>) -> Result<BatchAppendStats, AssetsError> {
        let mut stats = BatchAppendStats::default();
        for event in events {
            match self.append(event).await {
                Ok(()) => stats.inserted += 1,
                Err(AssetsError::DuplicateEvent(_)) => stats.duplicates += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(stats)
    }

    /// Full event log for a asset, chronological (oldest first). Returns
    /// an empty Vec if the asset is unknown.
    async fn events_for(&self, asset_id: &AssetId) -> Result<Vec<AssetEvent>, AssetsError>;

    /// Current state for a asset, or None if the asset is unknown.
    async fn current_state(
        &self,
        asset_id: &AssetId,
    ) -> Result<Option<AssetCurrentState>, AssetsError>;

    /// All known asset ids, unordered.
    async fn all_asset_ids(&self) -> Result<Vec<AssetId>, AssetsError>;

    /// Return a page of asset ids with total count.
    async fn list_asset_ids(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AssetId>, i64), AssetsError>;

    /// Return a page of asset summaries with total count.
    ///
    /// `account_id` scopes the result to assets currently HELD by
    /// (holder_kind='account', holder_id=account_id) — Q5's typed
    /// pair; location-held equipment never matches. Owned by
    /// (or last associated with) one account — used by the unified
    /// account-detail view to render the Devices panel without
    /// pulling every assets row across the wire.
    async fn list_assets(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<AssetCurrentState>, i64), AssetsError>;

    /// Count open service Jobs associated with a given account.
    ///
    /// Used by cross-service guards (e.g., the account delete path in
    /// boss-people) to reject destructive operations while assets at
    /// that account still have unresolved service work.
    async fn open_ticket_count_for_account(&self, account_id: &str) -> Result<u64, AssetsError>;

    /// Count assets in any active lifecycle phase (not
    /// decommissioned) that reference the given catalog SKU.
    ///
    /// Used by the boss-catalog asset model delete guard to refuse
    /// deleting a catalog entry while real assets in custody or in
    /// the field still depend on it.
    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsError>;

    /// Aggregated assets summary for dashboards. SQL-aggregated server-
    /// side so the Assets list kanban never has to download the full
    /// asset ids list to compute phase distribution.
    ///
    /// `today` anchors the warranty-expiring-30d count. The HTTP
    /// handler sources it from ClockClient so the count respects
    /// sim-time.
    async fn assets_summary(&self, today: chrono::NaiveDate) -> Result<AssetsSummary, AssetsError>;
}
