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
