//! Wire types for the credentials registry.
//!
//! `CredentialRow` is the RAW registry row, the same posture as
//! `DeliveryPolicyRow`: the reader owns the parse because the reader
//! owns the consequence. `scopes` and `consumers` stay `serde_json::
//! Value` (JSON arrays) rather than typed vectors so a row a future
//! migration enriches never turns the list endpoint into a 500.
//!
//! THE ROW NEVER CARRIES A VALUE. `storage_location` says where the
//! secret lives; nothing in this shape can say what it is.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One phase of a credential rotation, in protocol order. The four
/// phases mirror the rotate-a-credential packet's machine steps
/// (issue → install → verify → revoke) and each maps to exactly one
/// event kind — `credential.minted` / `.installed` / `.verified` /
/// `.revoked` — the rotation's immutable trace on the audit log.
///
/// The maiden rotation (2026-09-03) left NO domain events: its
/// provenance existed only as step-completion metadata, findable by
/// archaeology rather than by kind, and a never-emitted kind is
/// invisible to the audit-integrity checker — only design review
/// caught it. This enum is the one place the phase → kind mapping
/// lives; the migration declaring the kinds is pinned to it by test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationPhase {
    /// A replacement token was minted at the issuer.
    Minted,
    /// The minted value was written into the credential's declared
    /// storage location. The one phase that stamps `rotated_at`:
    /// this is the moment the value last changed, which is exactly
    /// what that column records.
    Installed,
    /// The new credential was verified by effect.
    Verified,
    /// The old token was revoked at the issuer and confirmed absent.
    Revoked,
}

impl RotationPhase {
    /// Every phase, in protocol order.
    pub const ALL: [RotationPhase; 4] = [
        RotationPhase::Minted,
        RotationPhase::Installed,
        RotationPhase::Verified,
        RotationPhase::Revoked,
    ];

    /// The URL path segment the rotation door accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            RotationPhase::Minted => "minted",
            RotationPhase::Installed => "installed",
            RotationPhase::Verified => "verified",
            RotationPhase::Revoked => "revoked",
        }
    }

    /// The event kind this phase records on the audit log.
    pub fn event_kind(self) -> &'static str {
        match self {
            RotationPhase::Minted => "credential.minted",
            RotationPhase::Installed => "credential.installed",
            RotationPhase::Verified => "credential.verified",
            RotationPhase::Revoked => "credential.revoked",
        }
    }

    /// Parse a door path segment. `None` names nothing — the HTTP
    /// door turns it into a 400 listing the valid phases.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// One row of `credentials`, unparsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRow {
    /// The durable identity, e.g. `boss-dev-forge-token`. Survives
    /// rotation — forge-side token instances derive their names from
    /// it (`{id}-{first 8 of packet id}`).
    pub id: String,
    /// `forgejo-access-token`, `k8s-serviceaccount`, `machine-token`,
    /// `kubeconfig`, ... Open string: a new kind is a row, not code.
    pub kind: String,
    /// Who mints it.
    pub issuer: String,
    /// Whose authority it carries.
    pub principal: String,
    /// JSON array of scope strings as the issuer spells them. Empty
    /// means scope-unverified — the audit fills it, nobody guesses.
    pub scopes: serde_json::Value,
    /// Where the value LIVES (Secret ns/name/key, file path). Never
    /// the value.
    pub storage_location: String,
    /// JSON array of `{kind, location}` — every place that reads it.
    pub consumers: serde_json::Value,
    /// `on-demand` | `scheduled`.
    pub rotation_policy: String,
    /// When the value last changed; `None` = no rotation recorded
    /// since the registry existed.
    pub rotated_at: Option<DateTime<Utc>>,
    pub notes: String,
}
