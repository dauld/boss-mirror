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

/// Does a cadence verb put a train on the track? `board` assembles and
/// departs one; `run` is reconcile-then-board. Exactly these — a
/// reconcile or a packet-open never needs a clear track.
///
/// ONE DEFINITION (CLAUDE.md §9a). The conductor decides serialization
/// and idle-firing outcomes on it, and the yard decides whether a
/// `basis=clock` row is a BOARDING trigger worth describing to the
/// operator ("Boards at 06:05 / 18:05 UTC") on it. It lived only in the
/// conductor until 634a475b, so the yard selected clock rows by basis
/// alone and would have named a clock row with any other verb as a
/// boarding window — latent while the only live clock row is
/// `train-window` with verb `run`. Tier 1 owns it because Tier 1 cannot
/// read the orchestrator, and both readers can read here.
pub fn departs_a_train(verb: &str) -> bool {
    matches!(verb, "board" | "run")
}

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
    /// The verb's exit code, once `record_outcome` has merged it into the
    /// firing's `detail`. `None` means no outcome is recorded yet — the run
    /// is still in flight, or it was cut off without one — which is NOT the
    /// same as a failure and must not be read as one. Evaluation needs this
    /// to tell a firing that did its work from one that died on its first
    /// syscall: without it, a board that boarded nothing still consumed its
    /// whole cooldown (2026-09-04, two hours of a threshold-met dock).
    #[serde(default)]
    pub rc: Option<i32>,
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
