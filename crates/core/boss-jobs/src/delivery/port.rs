//! Delivery-policy port — the two reads the train conductor needs from
//! the registry, and the two operations the platform seed needs to
//! DECLARE a policy. Nothing else.
//!
//! No in-place write anywhere. A policy change is a version bump in
//! `infra/platform/delivery-policy/<name>.toml` (since 2026-09-18,
//! backlog 393d3234; before it, a migration — 202608242117 then
//! 202609050500): retire the active row, insert the next version, so
//! "what was the policy when this train departed?" stays answerable
//! against the version the train pinned. An endpoint that let anything
//! mutate a row in place would take that answer away.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::types::{DeliveryPolicyRow, DeliveryPolicySpec};

#[derive(Debug, thiserror::Error)]
pub enum DeliveryPolicyError {
    #[error("bad request: {0}")]
    BadRequest(String),
    /// A declared (name, version) the registry already holds — the
    /// seed publishes only what is absent, never over a row.
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait DeliveryPolicyRepository: Send + Sync {
    /// The active policy for `name`, or `None` when the registry holds
    /// none. `None` is not an error: the conductor answers it with its
    /// compiled fallback and a loud journal line.
    async fn active_policy(
        &self,
        name: &str,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError>;

    /// One specific version, whatever its status — this is what an
    /// in-flight train reads, and a train may well outlive the
    /// retirement of the policy it departed under.
    async fn policy_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError>;
}

/// The registry half — what DECLARES a policy, as distinct from what
/// reads one. Two operations, the same two every platform bundle seed
/// needs (`crate::bundle_seed::BundleRegistry`): read a name's whole
/// lineage, and land a declared version.
///
/// A second trait rather than two more methods on the one above, as
/// `cadence::CadenceRegistry` is (backlog 393d3234, consolidation H4,
/// car 4, 2026-09-18): the conductor never writes a policy and never
/// reads a retired one except by pinned version, so its port stays the
/// two reads it has, and a test double of the conductor's port owes
/// nothing to the seed. No outbox event on publish: no migration ever
/// recorded one, and the train Job's `delivery_policy_version` stamp
/// is the record of which policy a train ran under.
#[async_trait]
pub trait DeliveryPolicyRegistry: Send + Sync {
    /// Every row of this name, any status, any order — the whole
    /// lineage. Empty when the registry has never held the name.
    async fn live_versions(
        &self,
        name: &str,
    ) -> Result<Vec<DeliveryPolicySpec>, DeliveryPolicyError>;

    /// Retire any active row of the same name, then insert `spec` at
    /// its declared version, active, stamped `now` — in that order,
    /// because `delivery_policy_one_active_per_name` is a plain partial
    /// unique index enforced per statement (the class
    /// `registry-bump-retires-first` guards in migrations). A row
    /// already at (name, version) is a conflict, not an overwrite.
    async fn publish_declared(
        &self,
        spec: DeliveryPolicySpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<DeliveryPolicySpec, DeliveryPolicyError>;
}
