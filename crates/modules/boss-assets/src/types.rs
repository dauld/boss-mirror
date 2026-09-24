//! Domain types for asset tracking.
//!
//! `AssetEvent` is a tagged enum so JSON payloads carry their `kind`.
//! `AssetId` and `EmployeeId`-shaped strings are newtyped to prevent
//! argument-order mistakes at call sites.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub(crate) fn default_currency() -> String {
    "USD".to_string()
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Boss-issued asset identifier, format SYS-{:08}.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Event id — unique across all asset events.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetEventId(pub String);

impl AssetEventId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// How a physical unit entered the install base (used trade-in,
/// buyback, returned lease, OEM-new, …). Free-text wrapper around a
/// kebab-case string; tenants extend via the Class registry under
/// `(subject_kind='asset')`. Carried on the `Received` event payload
/// (JSONB), not a flat column — validation happens at the asset-event
/// ingest boundary against the active Class set per
/// docs/design/class-registry.md. Serializes transparently to the bare
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntakeSource(pub String);

impl IntakeSource {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IntakeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for IntakeSource {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for IntakeSource {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Warranty coverage tier (standard, extended, …). Free-text wrapper
/// around a kebab-case string; tenants extend via the Class registry
/// under `(subject_kind='asset')`. Carried on the `WarrantyStarted`
/// event payload (JSONB), validated at the asset-event ingest boundary
/// per docs/design/class-registry.md. Serializes transparently to the
/// bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WarrantyCoverage(pub String);

impl WarrantyCoverage {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WarrantyCoverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for WarrantyCoverage {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for WarrantyCoverage {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// The discriminated union of things that can happen to a asset.
///
/// Serialized with a `kind` tag so consumers can pattern-match without
/// knowing the full enum ahead of time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AssetEventKind {
    // Identity
    /// The identity-birth event: this asset instance exists. Carries
    /// nothing but the envelope's `asset_id` (plus an optional OEM
    /// serial if it's on the sticker). Identity-first — a unit can be
    /// registered before its catalog model (`sku`), custody, or
    /// location are known, then enriched. Lands the projection at
    /// `phase = Registered` with `sku = None`.
    Registered {
        #[serde(default, alias = "serial")]
        oem_serial: Option<String>,
    },
    /// Sets the catalog model (`sku`) once it's known — the
    /// "register now, identify later" path (e.g. a unit whose model is
    /// read off it after intake). Does not advance custody phase; pure
    /// identification. `Received` may also carry the sku when it's known
    /// at intake, so this event is only needed for late identification.
    Identified {
        sku: String,
    },

    // Custody
    Received {
        /// Catalog SKU this physical unit is an instance of, when known
        /// at intake. `None` = received but not yet identified; a later
        /// `Identified` event sets it. Optional so receipt
        /// is pure custody, decoupled from identification.
        #[serde(default)]
        sku: Option<String>,
        source: IntakeSource,
        /// OEM-assigned factory serial captured at intake. Distinct
        /// from the Boss-assigned asset_id.
        #[serde(default, alias = "serial")]
        oem_serial: Option<String>,
    },
    PutAway {
        bin: String,
    },
    // TriageCompleted, RefurbStarted, RefurbCompleted and QaPassed —
    // the used-device shop's refurb pipeline — retired with that tenant
    // (backlog a8991c86, 2026-09-24): only its engine wrote them, and
    // the system of record's audit log held none of them, nor any asset
    // event at all. A log that did would now refuse to deserialise
    // rather than project a phase nothing else can reach.
    PartReplaced {
        part_sku: String,
        reason: String,
    },

    // Commerce
    Sold {
        account_id: String,
        price_cents: i64,
        #[serde(default = "crate::types::default_currency")]
        currency: String,
        order_id: Option<String>,
        condition: AssetCondition,
    },
    // Custody events carry the typed holder pair (Q5): WHO has the
    // asset is a (kind, id) edge, not an account id. The brewery
    // installs equipment at locations; the device shop ships to
    // accounts — same event, honest subject either way.
    Shipped {
        holder_kind: String,
        holder_id: String,
    },
    Installed {
        holder_kind: String,
        holder_id: String,
    },
    WarrantyStarted {
        through: NaiveDate,
        coverage: WarrantyCoverage,
    },
    WarrantyExpired,
    OwnershipTransferred {
        from_account_id: String,
        to_account_id: String,
    },

    // Service
    ServiceJobOpened {
        job_id: String,
        summary: String,
    },
    ServiceJobClosed {
        job_id: String,
        turnaround_days: u32,
    },
    WarrantyClaimed {
        job_id: String,
    },
    Decommissioned {
        reason: String,
    },
}

/// Condition a unit was sold in (new, used, …). Free-text wrapper
/// around a kebab-case string; tenants extend via the Class registry
/// under `(subject_kind='asset')`. Carried on the `Sold` event payload
/// (JSONB), validated at the asset-event ingest boundary per
/// docs/design/class-registry.md. Serializes transparently to the bare
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetCondition(pub String);

impl AssetCondition {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AssetCondition {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AssetCondition {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// One immutable fact about a physical asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetEvent {
    pub id: AssetEventId,
    pub asset_id: AssetId,
    pub ts: NaiveDate,
    /// Who (or what) fired this event. Every event carries one —
    /// there is no "anonymous" emit path. See
    /// `docs/design/human-powered-state-machine.md` (I-2).
    pub actor_id: boss_core::actor::ActorId,
    #[serde(flatten)]
    pub kind: AssetEventKind,
}

// ---------------------------------------------------------------------------
// Current-state projection
// ---------------------------------------------------------------------------

/// Where a physical unit sits in its custody/service lifecycle.
/// Projection-derived: `project.rs` folds the asset event log into one
/// of these phases; callers never post a phase directly. Free-text
/// wrapper around a kebab-case string so the vocabulary is data — the
/// platform phases (`ORDER`) are the ACTIVE Class rows under
/// `(subject_kind='asset', member_attribute='phase')` and a tenant
/// extends the lifecycle by adding a row, not forking core. Serializes
/// transparently to the bare string; the `assets.phase` column stores
/// that string directly (no enum mapping). See
/// docs/design/class-registry.md.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetLifecyclePhase(pub String);

impl AssetLifecyclePhase {
    /// Identity exists; not yet received into custody or identified.
    /// The entry state for an identity-first asset.
    pub const REGISTERED: &'static str = "registered";
    pub const RECEIVED: &'static str = "received";
    pub const SHIPPED: &'static str = "shipped";
    pub const INSTALLED: &'static str = "installed";
    pub const OUT_FOR_SERVICE: &'static str = "out-for-service";
    pub const DECOMMISSIONED: &'static str = "decommissioned";

    /// Every phase the projector can emit, in pipeline order — the one
    /// list both summary adapters count over, and the one the Class
    /// rows the migrations leave active are held equal to
    /// (projection_persistence.rs). Until backlog a8991c86 it was spelled
    /// twice inline and carried the refurb pipeline's triaging /
    /// refurbing / qa / ready (retired by 20260924172233).
    pub const ORDER: [&'static str; 6] = [
        Self::REGISTERED,
        Self::RECEIVED,
        Self::SHIPPED,
        Self::INSTALLED,
        Self::OUT_FOR_SERVICE,
        Self::DECOMMISSIONED,
    ];

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when this is the terminal retirement phase. The projection
    /// stops folding further events once an asset reaches it.
    pub fn is_decommissioned(&self) -> bool {
        self.0 == Self::DECOMMISSIONED
    }
}

impl std::fmt::Display for AssetLifecyclePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AssetLifecyclePhase {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AssetLifecyclePhase {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Summary state at a point in time, derived from the event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetCurrentState {
    pub asset_id: AssetId,
    /// Catalog model this unit is an instance of. `None` until the unit
    /// is identified (identity-first: a registered-but-unidentified
    /// asset has no model-derived attributes — no depreciation basis,
    /// no Equipment-KB model view — until this is set).
    #[serde(default)]
    pub sku: Option<String>,
    pub phase: AssetLifecyclePhase,
    /// The typed custody edge (Q5): who holds the asset — an
    /// account's site, a location, a depot. Set by Shipped/Installed
    /// (custody) and by Sold/OwnershipTransferred (ownership implies
    /// custody handover in the current flows). Validated via R2 once
    /// the edge registry lands.
    pub holder_kind: Option<String>,
    pub holder_id: Option<String>,
    pub warranty_through: Option<NaiveDate>,
    pub open_ticket_count: u32,
    pub first_seen: NaiveDate,
    pub last_event_at: NaiveDate,
    /// OEM-assigned factory serial captured at intake. Distinct from
    /// `asset_id` — that's Boss's internal ID (SYS-NNNNNNNN). Per
    /// the Boss ID concept (docs/design → pass 2), the UI always
    /// labels both explicitly so ops staff don't confuse them when
    /// reading off a physical sticker.
    pub oem_serial: Option<String>,
}

// ---------------------------------------------------------------------------
// Summary aggregates — drive the Assets list header/kanban and any
// dashboard panel that needs totals without downloading every asset.
// ---------------------------------------------------------------------------

/// One row in the `phase_counts` rollup: how many assets are currently
/// in a given lifecycle phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseRollup {
    pub phase: String,
    pub count: i64,
}

/// One row in the `sku_counts` rollup: how many assets in the field
/// (not decommissioned) for a given catalog sku.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkuRollup {
    pub sku: String,
    pub count: i64,
}

/// Assets summary returned by `GET /api/assets/summary`. Every number is
/// SQL-aggregated across the `assets` projection so the UI can render
/// correct totals without paginating the full asset ids list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetsSummary {
    /// Count per lifecycle phase, ordered in pipeline sequence so the
    /// Assets kanban can render the columns left-to-right without
    /// re-sorting.
    pub phase_counts: Vec<PhaseRollup>,
    pub total_systems: i64,
    /// Assets currently in-field (phase != 'decommissioned').
    pub in_field_count: i64,
    /// Sum of `open_ticket_count` across all assets. Matches the
    /// header's "N open tickets" figure.
    pub open_tickets_total: i64,
    /// Top SKUs by asset count — caller can truncate for display.
    pub sku_counts: Vec<SkuRollup>,
    /// Assets whose warranty expires within the next 30 days.
    pub warranty_expiring_30d: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_round_trips_through_json() {
        let s = AssetId::new("LUM-3D-010000");
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(j, "\"LUM-3D-010000\"");
        let back: AssetId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn event_serializes_with_kind_tag() {
        let e = AssetEvent {
            id: AssetEventId::new("evt-1"),
            asset_id: AssetId::new("LUM-3D-010000"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind: AssetEventKind::Received {
                sku: Some("Boss-TEST-2024".into()),
                source: IntakeSource::new("used-trade-in"),
                oem_serial: None,
            },
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"Received\""));
        assert!(json.contains("\"source\":\"used-trade-in\""));
    }

    #[test]
    fn event_round_trips_through_json() {
        let e = AssetEvent {
            id: AssetEventId::new("evt-2"),
            asset_id: AssetId::new("SPE-IPL-012345"),
            ts: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-042"),
            kind: AssetEventKind::ServiceJobClosed {
                job_id: "tkt-0007".into(),
                turnaround_days: 6,
            },
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: AssetEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn intake_source_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&IntakeSource::new("returned-lease")).unwrap(),
            "\"returned-lease\""
        );
    }

    // -----------------------------------------------------------------
    // Business invariant: ServiceJobClosed must carry turnaround_days.
    //
    // The invariant is enforced by the Rust type — `turnaround_days: u32`
    // is non-optional, so constructing a closed ticket without one does
    // not compile. These tests lock in the serde boundary so nobody can
    // bypass the type by posting a malformed JSON blob at the wire.
    // -----------------------------------------------------------------

    #[test]
    fn closed_ticket_event_serializes_with_turnaround_days() {
        let e = AssetEvent {
            id: AssetEventId::new("evt-close-1"),
            asset_id: AssetId::new("LUM-3D-010000"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind: AssetEventKind::ServiceJobClosed {
                job_id: "tkt-42".into(),
                turnaround_days: 11,
            },
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(
            json.contains("\"turnaround_days\":11"),
            "closed ticket JSON should include turnaround_days; got: {json}"
        );
        assert!(
            json.contains("\"kind\":\"ServiceJobClosed\""),
            "JSON should tag the variant; got: {json}"
        );
    }

    #[test]
    fn closed_ticket_event_deserialize_rejects_missing_turnaround_days() {
        // Legitimate shape, minus the turnaround_days field.
        let bad = r#"{
            "id": "evt-close-bad",
            "asset_id": "LUM-3D-010000",
            "ts": "2026-04-05",
            "actor_id": null,
            "kind": "ServiceJobClosed",
            "ticket_id": "tkt-99"
        }"#;
        let result: Result<AssetEvent, _> = serde_json::from_str(bad);
        assert!(
            result.is_err(),
            "deserializing ServiceJobClosed without turnaround_days should fail; \
             got Ok: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("turnaround_days") || err.contains("missing field"),
            "error should mention the missing field; got: {err}"
        );
    }

    #[test]
    fn closed_ticket_event_deserialize_rejects_null_turnaround_days() {
        let bad = r#"{
            "id": "evt-close-null",
            "asset_id": "LUM-3D-010000",
            "ts": "2026-04-05",
            "actor_id": null,
            "kind": "ServiceJobClosed",
            "ticket_id": "tkt-99",
            "turnaround_days": null
        }"#;
        let result: Result<AssetEvent, _> = serde_json::from_str(bad);
        assert!(
            result.is_err(),
            "deserializing ServiceJobClosed with null turnaround_days should fail; \
             got Ok: {result:?}"
        );
    }
}
