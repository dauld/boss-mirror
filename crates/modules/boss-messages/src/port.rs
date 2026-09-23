//! Port (trait) defining the message repository contract.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use crate::types::Message;

#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the stamp
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. Idempotency guards sit
/// AHEAD of the recording, so a collapsed replay records nothing.
#[async_trait]
pub trait MessageRepository: Send + Sync {
    async fn inbox(&self, recipient_id: &str) -> Result<Vec<Message>, MessageError>;
    /// Unread messages for a recipient, optionally narrowed to one
    /// `kind`. The narrowing is what makes the count usable as a
    /// badge: an inbox holding 1,980 unread `signal` rows against 3
    /// unread `direct` ones renders the noise as a number unless the
    /// caller can ask the question the reader actually has, which is
    /// "is anything addressed to me?". `None` counts every kind.
    async fn unread_count(
        &self,
        recipient_id: &str,
        kind: Option<&str>,
    ) -> Result<u32, MessageError>;
    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError>;
    /// Mark a message read at the given timestamp. Caller picks the
    /// timestamp so the same value can be carried in the
    /// `messages.message.read` event payload — letting a rebuild
    /// reconstruct the projection's `read_at` exactly.
    /// Records `messages.message.read` (`{id, read_at}`) in-tx —
    /// only when the row actually updated (a phantom id records
    /// nothing).
    async fn mark_read(
        &self,
        id: &str,
        read_at: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.sent` (full row state) in-tx — only
    /// when the INSERT actually inserted (a redelivered notification
    /// collapses on ON CONFLICT (id) and records nothing).
    async fn send(&self, msg: &Message, stamp: &EventStamp) -> Result<(), MessageError>;
    /// Records `messages.message.deleted` (`{id, deleted_at}`) in-tx
    /// after the row actually deleted.
    async fn delete_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.archived` (`{id, archived_at}`)
    /// in-tx after the row actually updated.
    /// Archive every UNREAD `signal` whose `entity_path` starts with
    /// `path_prefix`, returning how many moved. The expiry path for
    /// notifications about work that has finished (David, 2026-08-14:
    /// "a way to automatically expire inbox messages for jobs that
    /// have moved past relevancy").
    ///
    /// Three narrowings, each deliberate:
    ///
    /// - **`signal` only.** A `direct` is one person addressing
    ///   another; it does not stop being addressed to you because a
    ///   job closed, and auto-clearing it would delete the one
    ///   category the inbox's needs-you filter is built on.
    /// - **Unread only.** A read message has already done its job and
    ///   rewriting it would churn the log for no reader.
    /// - **Archived, not deleted.** `read_at` would claim someone read
    ///   it, which is false; deletion would lose the record. Archiving
    ///   says exactly what happened — it stopped being relevant.
    async fn expire_signals_under(
        &self,
        path_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError>;

    /// Archive every UNREAD notice under `path_prefix` whose id starts
    /// with `id_prefix`, of ANY kind, returning how many moved. The
    /// retirement path for the notices the machine sends about a step
    /// (backlog 0b2bac00, 2026-09-23): the dispatcher's notifier sends
    /// an assignee's ready/assigned notice as a `direct` with the id
    /// `notify:{step}:{recipient}`, and `expire_signals_under` above
    /// never touches a direct, so 83 of David's 90 direct notices
    /// pointed at completed steps while his badge counted them.
    ///
    /// The id prefix is the narrowing, where the kind is above. A
    /// person's direct carries a minted id and a `done:` announcement
    /// its own prefix, so neither moves; the caller names the prefix
    /// (a dispatcher rule's argument), so which notices retire is rule
    /// data rather than a list in this crate. Unread only, and
    /// archived rather than read or deleted, for the reasons above.
    async fn expire_notices_under(
        &self,
        path_prefix: &str,
        id_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError>;

    async fn archive_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Return all messages in a thread (the root message + all replies).
    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError>;
}
