//! Cadence port — the four operations the train conductor needs from
//! the registry, and nothing else.
//!
//! No outbox events here, deliberately. `cadence_firings` IS the
//! measurement record: every firing is already a row with its window,
//! its basis, and (after the verb) its exit code and runtime. Adding a
//! parallel event stream would duplicate the fact without adding a
//! queryable one.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::types::{CadenceRuleRow, CadenceRuleSpec, LastFiring, NewFiring};

#[derive(Debug, thiserror::Error)]
pub enum CadenceError {
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
pub trait CadenceRepository: Send + Sync {
    /// Active rules, name-ordered. Rows are returned unparsed — see
    /// `types::CadenceRuleRow` for why the parse stays in the caller.
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError>;

    /// The newest firing of `rule`, or `None` if it has never fired.
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, CadenceError>;

    /// Claim a firing id. `Ok(true)` means this caller won the window
    /// and must run the verb; `Ok(false)` means someone else already
    /// holds it. Exactly-once rests on the `firing_id` primary key, so
    /// two conductors racing the same window cannot both win.
    async fn claim_firing(&self, new: &NewFiring) -> Result<bool, CadenceError>;

    /// Merge the verb's exit code and runtime into the firing's
    /// `detail`. Merging (not replacing) preserves whatever the claim
    /// recorded — e.g. the dock depth that triggered a queue-depth
    /// rule.
    async fn record_outcome(
        &self,
        firing_id: &str,
        rc: i32,
        runtime_secs: u64,
    ) -> Result<(), CadenceError>;
}

/// The registry half — what DECLARES a rule, as distinct from what
/// fires one. Two operations, the same two every platform bundle seed
/// needs (`crate::bundle_seed::BundleRegistry`): read a name's whole
/// lineage, and land a declared version.
///
/// WHY A SECOND TRAIT AND NOT TWO MORE METHODS ON THE ONE ABOVE
/// (backlog 393d3234, consolidation H4, car 3, 2026-09-18). Until this
/// car the only writer of `cadence_rules` was a migration: nine of
/// them, the last six re-versioning one rule, each a contended
/// timestamped file that a fresh instance could only reproduce by
/// replaying. The conductor never writes a rule and never reads a
/// retired one, so its port stays the four verbs it has; the seed's
/// two land here, and a test double of the conductor's port owes
/// nothing to the seed. No outbox event on publish, for the reason the
/// module doc gives: no migration ever recorded one, and
/// `cadence_firings` is the record of what a rule DID.
#[async_trait]
pub trait CadenceRegistry: Send + Sync {
    /// Every row of this name, any status, any order — the whole
    /// lineage. Empty when the registry has never held the name.
    async fn live_versions(&self, name: &str) -> Result<Vec<CadenceRuleSpec>, CadenceError>;

    /// Retire any active row of the same name, then insert `spec` at
    /// its declared version, active, stamped `now` — in that order,
    /// because `cadence_rules_one_active_per_name` is a plain partial
    /// unique index enforced per statement (the class
    /// `registry-bump-retires-first` guards in migrations). A row
    /// already at (name, version) is a conflict, not an overwrite.
    async fn publish_declared(
        &self,
        spec: CadenceRuleSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<CadenceRuleSpec, CadenceError>;
}
