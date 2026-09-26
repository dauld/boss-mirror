//! Rebuild the `messages` projection from `audit_log`.
//!
//! Pilot rebuilder for the "events are the source of truth" arc — the
//! same shape every other service projection will eventually grow.
//! `audit_log` is canonical; this function drops the projection table
//! and replays every `messages.message.*` event in id order to
//! reconstruct it.
//!
//! Mirrors `boss-ledger`'s rebuild: advisory lock, single transaction,
//! deterministic output. Different lock key so the two rebuilds don't
//! fight each other.
//!
//! Event payload contract (see `boss-messages/src/http.rs`):
//!
//! - `messages.message.sent` — full Message row state (id, sender_id,
//!   recipient_id, subject, body, entity_ref, kind, sent_at, reply_to)
//! - `messages.message.read` — `{id, read_at}`
//! - `messages.message.archived` — `{id, archived_at}` (sets `archived_at`
//!   once, beside `kind`; a repeat archive the old door recorded is
//!   skipped, so the first archive's time stands — backlog 9bda9726)
//! - `messages.message.deleted` — `{id, deleted_at}` (DELETE row)
//!
//! Rebuilds that hit a payload missing required fields (older events
//! emitted before the enrichment landed) skip with a warning rather
//! than abort — the rebuild stays resilient when a slice of history
//! pre-dates the enrichment.

use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;

use crate::types::{EntityRef, MessageKind};

/// Advisory-lock key for the messages rebuilder, derived from the
/// projection name so it is distinct from every other rebuilder's key.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("messages");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub rows_inserted: u64,
    pub rows_marked_read: u64,
    pub rows_archived: u64,
    pub rows_deleted: u64,
}

/// Shape we deserialize the SENT payload into. Mirrors
/// `crate::types::Message` but tolerant of fields we don't strictly
/// need on rebuild (e.g. unknown keys carried by future enrichment).
#[derive(Debug, Deserialize)]
struct SentPayload {
    id: String,
    sender_id: String,
    recipient_id: String,
    subject: String,
    body: String,
    #[serde(default)]
    entity_ref: Option<EntityRef>,
    kind: MessageKind,
    sent_at: DateTime<Utc>,
    #[serde(default)]
    read_at: Option<DateTime<Utc>>,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadPayload {
    id: String,
    read_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct IdOnlyPayload {
    id: String,
}

/// Every archive door writes `archived_at`; a bare `{id}` from an older
/// writer falls back to the log row's own time, as the backfill
/// migration (20260926061106) does.
#[derive(Debug, Deserialize)]
struct ArchivedPayload {
    id: String,
    #[serde(default)]
    archived_at: Option<DateTime<Utc>>,
}

/// Drop every row in `messages` and replay every `messages.message.*`
/// event from `audit_log` in id order. Wrapped in a single
/// transaction holding an advisory lock for the duration —
/// concurrent writes block briefly.
pub async fn rebuild_messages(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // Wipe the projection. The reply_to FK is `ON DELETE SET NULL`,
    // so the cascade is harmless — and we re-insert in event order
    // anyway, so every reply_to that survives a SENT event lands
    // back as the original value.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM messages"],
        "kind LIKE 'messages.message.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "messages.message.sent" => {
                    let p: SentPayload = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            // Pre-enrichment events that only carried
                            // `{id}` land here. Skip — there's nothing to
                            // rebuild from a bare id.
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping pre-enrichment SENT event (payload missing required fields)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    let (entity_type, entity_id, entity_path) = match &p.entity_ref {
                        Some(er) => (
                            Some(er.entity_type.as_str()),
                            Some(er.entity_id.as_str()),
                            er.entity_path.as_deref(),
                        ),
                        None => (None, None, None),
                    };
                    sqlx::query(
                        "INSERT INTO messages \
                         (id, sender_id, recipient_id, subject, body, entity_type, entity_id, entity_path, \
                          kind, sent_at, read_at, reply_to) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    )
                    .bind(&p.id)
                    .bind(&p.sender_id)
                    .bind(&p.recipient_id)
                    .bind(&p.subject)
                    .bind(&p.body)
                    .bind(entity_type)
                    .bind(entity_id)
                    .bind(entity_path)
                    .bind(p.kind.as_str())
                    .bind(p.sent_at)
                    .bind(p.read_at)
                    .bind(&p.reply_to)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?;
                    report.rows_inserted += 1;
                    Ok(Applied::Yes)
                }
                "messages.message.read" => {
                    // Read events require a fully-shaped payload
                    // (id + read_at); anything else is a hard error
                    // that blocks rebuild rather than silently
                    // dropping the read.
                    let p: ReadPayload = serde_json::from_value(ev.payload).map_err(|e| {
                        format!(
                            "invalid payload for event id {} kind {}: {e}",
                            ev.audit_id, ev.kind
                        )
                    })?;
                    let n = sqlx::query("UPDATE messages SET read_at = $1 WHERE id = $2")
                        .bind(p.read_at)
                        .bind(&p.id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?
                        .rows_affected();
                    if n == 0 {
                        // READ for a message we never SENT (or already
                        // DELETED). Tolerate — the projection's already
                        // in the right "row absent" state.
                        Ok(Applied::Skipped)
                    } else {
                        report.rows_marked_read += 1;
                        Ok(Applied::Yes)
                    }
                }
                "messages.message.archived" => {
                    let p: ArchivedPayload = serde_json::from_value(ev.payload).map_err(|e| {
                        format!(
                            "invalid payload for event id {} kind {}: {e}",
                            ev.audit_id, ev.kind
                        )
                    })?;
                    // The live doors' guard, replayed: only a row not
                    // yet archived moves, and `kind` is left as sent.
                    let n = sqlx::query(
                        "UPDATE messages SET archived_at = $2 \
                         WHERE id = $1 AND archived_at IS NULL",
                    )
                    .bind(&p.id)
                    .bind(p.archived_at.unwrap_or(ev.ts))
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .rows_affected();
                    if n == 0 {
                        Ok(Applied::Skipped)
                    } else {
                        report.rows_archived += 1;
                        Ok(Applied::Yes)
                    }
                }
                "messages.message.deleted" => {
                    let p: IdOnlyPayload = serde_json::from_value(ev.payload).map_err(|e| {
                        format!(
                            "invalid payload for event id {} kind {}: {e}",
                            ev.audit_id, ev.kind
                        )
                    })?;
                    let n = sqlx::query("DELETE FROM messages WHERE id = $1")
                        .bind(&p.id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?
                        .rows_affected();
                    if n == 0 {
                        Ok(Applied::Skipped)
                    } else {
                        report.rows_deleted += 1;
                        Ok(Applied::Yes)
                    }
                }
                other => {
                    // Some other `messages.message.*` topic we don't know
                    // about — likely added since this rebuilder shipped.
                    warn!(event_id = ev.audit_id, kind = %other, "unknown messages.message.* event kind; skipping");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}
