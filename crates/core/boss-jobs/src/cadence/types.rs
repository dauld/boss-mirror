//! Wire types for the cadence surface.
//!
//! `CadenceRuleRow` is deliberately the RAW registry row — nullable
//! basis-specific columns and all — not a parsed rule. The conductor
//! owns the parse because it owns the consequence: a malformed row is
//! skipped loudly in the conductor's journal every tick, which is
//! where an operator reads it. An API that parsed and 500'd would turn
//! one bad registry row into a dead cadence loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One row of `cadence_rules`, unparsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadenceRuleRow {
    pub name: String,
    pub verb: String,
    pub basis: String,
    #[serde(default)]
    pub every_minutes: Option<i32>,
    #[serde(default)]
    pub at_times: Option<serde_json::Value>,
    #[serde(default)]
    pub min_dock_depth: Option<i32>,
    #[serde(default)]
    pub cooldown_minutes: Option<i32>,
    /// Calendar basis: which days the rule fires on
    /// (`daily|weekly|monthly|...` — parsed by `boss_core::calendar` in
    /// the conductor, deliberately not here; the row stays raw).
    #[serde(default)]
    pub cadence: Option<String>,
    /// Calendar basis: the date the recurrence is anchored to.
    #[serde(default)]
    pub anchor_date: Option<chrono::NaiveDate>,
    /// Calendar basis: optional business-calendar code; absent means
    /// every day is a business day.
    #[serde(default)]
    pub business_calendar: Option<String>,
}

/// The most recent recorded firing of a rule — what the conductor's
/// evaluation compares a candidate window against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastFiring {
    pub firing_id: String,
    pub fired_at: DateTime<Utc>,
}

/// A claim request. `fired_at` is supplied BY THE CALLER and bound as
/// a parameter — it is boss-clock time, never the database's
/// wallclock. Sim runs depend on this: a sim-dated conductor tick must
/// record a sim-dated firing, and `NOW()` would silently overwrite it
/// with real time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFiring {
    pub firing_id: String,
    pub rule_name: String,
    pub verb: String,
    pub basis: String,
    pub fired_at: DateTime<Utc>,
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// Result of a claim. `claimed: false` means the window was already
/// taken — by a concurrent conductor, or by this one before a crash
/// mid-verb. The caller must not run the verb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimResult {
    pub claimed: bool,
}

/// What the verb cost, merged into the firing's `detail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiringOutcome {
    pub rc: i32,
    pub runtime_secs: u64,
}
