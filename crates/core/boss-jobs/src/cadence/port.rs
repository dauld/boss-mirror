//! Cadence port — the four operations the train conductor needs from
//! the registry, and the registry half that declares a rule.
//!
//! No outbox events on the conductor's half, deliberately.
//! `cadence_firings` IS the measurement record: every firing is
//! already a row with its window, its basis, and (after the verb) its
//! exit code and runtime. Adding a parallel event stream would
//! duplicate the fact without adding a queryable one. The registry
//! half is different — see [`CadenceRegistry`].

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
/// fires one. Three operations: the two every platform bundle seed
/// needs (`crate::bundle_seed::BundleRegistry` — read a name's whole
/// lineage, land a declared version) and the retire an operator needs.
///
/// WHY A SECOND TRAIT AND NOT MORE METHODS ON THE ONE ABOVE
/// (backlog 393d3234, consolidation H4, car 3, 2026-09-18). Until this
/// car the only writer of `cadence_rules` was a migration: nine of
/// them, the last six re-versioning one rule, each a contended
/// timestamped file that a fresh instance could only reproduce by
/// replaying. The conductor never writes a rule and never reads a
/// retired one, so its port stays the four verbs it has; the seed's
/// two land here, and a test double of the conductor's port owes
/// nothing to the seed.
///
/// EVERY WRITE HERE RECORDS ITS EVENT, atomically with the row, the
/// way the station and workflow registries do (backlog 13d1fff3,
/// 2026-09-18). Car 3 left publish un-evented on the reasoning that
/// no migration had ever recorded one and `cadence_firings` records
/// what a rule DID; the door this trait now fronts is an operator's
/// (`POST /api/cadence/rules/{name}/publish|retire`), and a schedule
/// change made by a person and witnessed by nothing is exactly the
/// un-evented registry write protocol-policy-publish.md forbids.
/// `jobs.cadence.published` carries the row written;
/// `jobs.cadence.retired` the row retired. The seed publishes through
/// the same method, so a fresh boot leaves one event per rule landed.
#[async_trait]
pub trait CadenceRegistry: Send + Sync {
    /// Every row of this name, any status, any order — the whole
    /// lineage. Empty when the registry has never held the name.
    async fn live_versions(&self, name: &str) -> Result<Vec<CadenceRuleSpec>, CadenceError>;

    /// Retire any active row of the same name, then insert `spec` at
    /// its declared version, active, stamped `now` — in that order,
    /// because `cadence_rules_one_active_per_name` is a plain partial
    /// unique index enforced per statement (the class
    /// `registry-bump-retires-first` guards in migrations). Records
    /// `jobs.cadence.published` (payload = the row written).
    ///
    /// `Conflict` unless `spec.version` is ABOVE every version the
    /// lineage holds — a row at (name, version) is never overwritten,
    /// and a version below the newest would retire the newer row
    /// under an older declaration, which no caller means: the seed's
    /// decision table never publishes below a live row, and an
    /// operator's publish is the version bump the bundle README names.
    async fn publish_declared(
        &self,
        spec: CadenceRuleSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<CadenceRuleSpec, CadenceError>;

    /// Retire the active row of `name`, recording
    /// `jobs.cadence.retired` (payload = the row retired), and return
    /// it. `None` when no row of the name is active — nothing was
    /// written, so nothing is recorded; the door answers 404 rather
    /// than a silent 204, because the operator retiring a rule by
    /// name wants to know the name was live.
    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<Option<CadenceRuleSpec>, CadenceError>;
}
