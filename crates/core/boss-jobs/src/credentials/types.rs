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

/// The event a declaration leaves: one `credential.declared` per row
/// the batch INSERTED — the declaration as inserted plus `declared_by`,
/// the actor the request signed with, and `tenant_id`, the declaring
/// instance (20260917071313's rule: one fact per inserted row, none
/// for a kept row, none per batch).
pub const CREDENTIAL_DECLARED: &str = "credential.declared";

/// One place a credential's value is read from, as the registry's
/// `consumers` array spells it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consumer {
    pub kind: String,
    pub location: String,
}

/// One credential as an instance declares it (`[[credential]]` in a
/// tenant's `seeds/credentials.toml`) and as `POST
/// /api/credentials/batch` takes it. KNOWLEDGE only, the registry's
/// one rule: `storage_location` says where the value lives, and the
/// shape has no field a value could ride in — an unknown key (`value`,
/// `token`, `secret`) is refused by the parser, not stored. Until
/// backlog ee368d0c (2026-09-18) these rows were authored only by
/// migrations, so every OSS install booted with one operator's forge,
/// Stripe and Cloudflare credential ids; now the instance declares
/// its own, and a fresh database holds none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialInput {
    /// The durable identity (`stripe-restricted-read`); survives rotation.
    pub id: String,
    /// `forgejo-access-token`, `stripe-restricted-key`, ... open text.
    pub kind: String,
    /// Who mints it.
    pub issuer: String,
    /// Whose authority it carries.
    pub principal: String,
    /// Scope strings as the issuer spells them; empty = unverified.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// WHERE the value lives (a Secret ns/name/key, a file path).
    pub storage_location: String,
    /// Every place that reads the value.
    #[serde(default)]
    pub consumers: Vec<Consumer>,
    /// `on-demand` (the default) or `scheduled`.
    #[serde(default = "on_demand")]
    pub rotation_policy: String,
    #[serde(default)]
    pub notes: String,
}

fn on_demand() -> String {
    "on-demand".to_string()
}

/// Why a declaration is refused, named so the refusal says which
/// check failed; the same check runs in `boss tenant check`, the
/// batch door and both adapters.
pub fn validate_credential(c: &CredentialInput) -> Result<(), String> {
    if c.id.is_empty() {
        return Err("a credential needs an id (e.g. stripe-restricted-read)".into());
    }
    if !c
        .id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(format!(
            "credential {}: id is not kebab-case (lowercase, digits, hyphens)",
            c.id
        ));
    }
    for (field, v) in [
        ("kind", &c.kind),
        ("issuer", &c.issuer),
        ("principal", &c.principal),
        ("storage_location", &c.storage_location),
    ] {
        if v.trim().is_empty() {
            return Err(format!("credential {}: {field} is required", c.id));
        }
    }
    if c.rotation_policy != "on-demand" && c.rotation_policy != "scheduled" {
        return Err(format!(
            "credential {}: rotation_policy `{}` is neither on-demand nor scheduled",
            c.id, c.rotation_policy
        ));
    }
    if let Some(bad) = c
        .consumers
        .iter()
        .find(|k| k.kind.trim().is_empty() || k.location.trim().is_empty())
    {
        return Err(format!(
            "credential {}: a consumer needs both kind and location, got {bad:?}",
            c.id
        ));
    }
    Ok(())
}

/// What `POST /api/credentials/batch` takes: the declaring tenant and
/// its declarations. The tenant id rides on the batch and on each
/// row's `credential.declared` fact, never on the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialBatch {
    pub tenant_id: String,
    pub credentials: Vec<CredentialInput>,
}

/// What a batch did: how many rows it received and how many it
/// inserted (the rest were already there and kept as they were).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsBatchOutcome {
    pub received: usize,
    pub inserted: usize,
}
