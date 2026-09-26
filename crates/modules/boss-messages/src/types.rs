//! Domain types for the messages module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How a message is routed / surfaced. Free-text wrapper around a
/// kebab-case string; the platform kinds (`direct`, `signal`) are seeded
/// as Class rows under `(subject_kind='message', member_attribute='kind')`
/// and a tenant adds its own routing kind by seeding a row, not forking
/// core. `archived` was a third until backlog 9bda9726: archiving is
/// the message's `archived_at`, beside its kind, and that Class row is
/// retired.
/// The messages API validates an incoming kind against the active Class
/// set at the send boundary (fail-loud → 400). Serializes transparently
/// to the bare string; the `messages.kind` column stores it directly.
/// See docs/design/class-registry.md.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageKind(pub String);

impl MessageKind {
    pub const DIRECT: &'static str = "direct";
    pub const SIGNAL: &'static str = "signal";

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The default kind for a message sent without one specified.
    pub fn direct() -> Self {
        Self(Self::DIRECT.to_string())
    }
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for MessageKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MessageKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRef {
    pub entity_type: String,
    pub entity_id: String,
    /// SPA-resolvable path to the entity (e.g.
    /// `/jobs/00000000-0000-0000-0000-000000000001`). When
    /// populated, the inbox UI renders the link from this field
    /// directly; when absent it falls back to the InboxPage
    /// `entityPath()` dispatcher. Producers should populate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub sender_id: String,
    pub recipient_id: String,
    pub subject: String,
    pub body: String,
    pub entity_ref: Option<EntityRef>,
    pub kind: MessageKind,
    pub sent_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    /// ID of the message this is replying to (thread parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// When the message left the inbox. A state BESIDE `kind`, not a
    /// value of it (backlog 9bda9726): archiving used to overwrite
    /// `kind` with `archived`, so the projection forgot whether the
    /// message had been a direct or a signal, and only the sent event
    /// in the log still knew. Skipped when absent, so a sent event's
    /// payload — the full row at send time — is unchanged by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}
