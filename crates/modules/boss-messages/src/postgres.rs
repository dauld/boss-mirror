//! Postgres adapter for `MessageRepository`.
//!
//! Queries the `messages` table and assembles into domain structs.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::port::{MessageError, MessageRepository};
use crate::types::{EntityRef, Message, MessageKind};

pub struct PgMessages {
    pool: PgPool,
}

impl PgMessages {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MessageRepository for PgMessages {
    async fn inbox(&self, recipient_id: &str) -> Result<Vec<Message>, MessageError> {
        let rows: Vec<MessageRow> =
            sqlx::query_as("SELECT * FROM messages WHERE recipient_id = $1 ORDER BY sent_at DESC")
                .bind(recipient_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into_message()).collect())
    }

    async fn unread_count(
        &self,
        recipient_id: &str,
        kind: Option<&str>,
    ) -> Result<u32, MessageError> {
        // One statement with a NULL-tolerant clause rather than two
        // query strings: `$2 IS NULL OR kind = $2` keeps the filtered
        // and unfiltered counts provably the same query.
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM messages \
             WHERE recipient_id = $1 AND read_at IS NULL \
               AND ($2::text IS NULL OR kind = $2)",
        )
        .bind(recipient_id)
        .bind(kind)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(row.0 as u32)
    }

    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError> {
        let row: Option<MessageRow> = sqlx::query_as("SELECT * FROM messages WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(row.map(|r| r.into_message()))
    }

    async fn mark_read(
        &self,
        id: &str,
        read_at: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        let result = sqlx::query("UPDATE messages SET read_at = $1 WHERE id = $2")
            .bind(read_at)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the read event records with the update —
        // and only when a row actually updated. A phantom id keeps
        // the endpoint's tolerant 200 but records no event (before,
        // it published a read event for a nonexistent message).
        if result.rows_affected() > 0 {
            let event = stamp.event(
                crate::events::MESSAGE_READ,
                serde_json::json!({ "id": id, "read_at": read_at }),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(MessageError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(())
    }

    async fn send(
        &self,
        msg: &Message,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        // Transparent newtype — the bare kebab code the column stores.
        let kind_str = msg.kind.as_str();
        let (entity_type, entity_id, entity_path) = match &msg.entity_ref {
            Some(er) => (
                Some(er.entity_type.as_str()),
                Some(er.entity_id.as_str()),
                er.entity_path.as_deref(),
            ),
            None => (None, None, None),
        };

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        // Identity write-through (subject-model R1, Q1).
        boss_subject_kinds::subjects::record_subject_in_tx(&mut tx, "message", &msg.id, None)
            .await
            .map_err(MessageError::Storage)?;
        let result = sqlx::query(
            "INSERT INTO messages (id, sender_id, recipient_id, subject, body, entity_type, entity_id, entity_path, kind, sent_at, reply_to) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&msg.id)
        .bind(&msg.sender_id)
        .bind(&msg.recipient_id)
        .bind(&msg.subject)
        .bind(&msg.body)
        .bind(entity_type)
        .bind(entity_id)
        .bind(entity_path)
        .bind(kind_str)
        .bind(msg.sent_at)
        .bind(&msg.reply_to)
        .execute(&mut *tx)
        .await
        .map_err(|e| MessageError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the sent event (full row state) records
        // with the row — and only when the INSERT actually inserted.
        // The ON CONFLICT (id) DO NOTHING idempotency guard doubles
        // as the event gate: a redelivered notification collapses
        // and records nothing (before, it published a duplicate
        // sent event per redelivery).
        if result.rows_affected() > 0 {
            let event = stamp.event(
                crate::events::MESSAGE_SENT,
                serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({ "id": msg.id })),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(MessageError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(())
    }

    async fn delete_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        let result = sqlx::query("DELETE FROM messages WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MessageError::NotFound(format!("no message with ID {id}")));
        }
        // OUTBOX (phase 2): the deleted event records only after the
        // row actually deleted (NotFound above returns pre-recording).
        let event = stamp.event(
            crate::events::MESSAGE_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(MessageError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError> {
        // Find the root of the thread, then fetch all messages with that root.
        // A thread is: the root message + all messages whose reply_to chain leads to it.
        // For simplicity, we fetch the root (reply_to IS NULL or equals itself) and
        // all direct replies. Deep threading can be added later.
        let rows: Vec<MessageRow> = sqlx::query_as(
            "SELECT * FROM messages WHERE id = $1 OR reply_to = $1 ORDER BY sent_at ASC",
        )
        .bind(message_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MessageError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into_message()).collect())
    }

    async fn expire_signals_under(
        &self,
        path_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        // RETURNING id, so the audit events name exactly the rows that
        // moved. A bulk UPDATE plus one summary event would leave the
        // rebuilder unable to reproduce the projection — it would know
        // that N messages were archived but not which, and the five-
        // property protocol's determinism clause is the thing that
        // breaks. Per-job this is a handful of rows.
        //
        // `LIKE prefix || '%'` matches both shapes the senders use:
        // `/jobs/{id}` from job-level notifications and
        // `/jobs/{id}/steps/{step}` from the step notifier.
        let ids: Vec<(String,)> = sqlx::query_as(
            "UPDATE messages SET kind = 'archived' \
             WHERE entity_path LIKE $1 || '%' \
               AND kind = 'signal' \
               AND read_at IS NULL \
             RETURNING id",
        )
        .bind(path_prefix)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| MessageError::Storage(e.to_string()))?;

        for (id,) in &ids {
            let event = stamp.event(
                crate::events::MESSAGE_ARCHIVED,
                serde_json::json!({ "id": id, "archived_at": now, "reason": "entity-past-relevancy" }),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(|e| MessageError::Storage(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(ids.len() as u32)
    }

    async fn expire_notices_under(
        &self,
        path_prefix: &str,
        id_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        // Same shape as `expire_signals_under`: RETURNING id so each
        // row moved gets its own event. `starts_with` rather than LIKE
        // for the id — a notice id is `notify:{uuid}:{recipient}`, and a
        // recipient id may carry `_`, which LIKE would read as a
        // wildcard. Any kind but `archived`: an assignee's notice is a
        // `direct`, which is the whole point (backlog 0b2bac00).
        let ids: Vec<(String,)> = sqlx::query_as(
            "UPDATE messages SET kind = 'archived' \
             WHERE entity_path LIKE $1 || '%' \
               AND starts_with(id, $2) \
               AND kind <> 'archived' \
               AND read_at IS NULL \
             RETURNING id",
        )
        .bind(path_prefix)
        .bind(id_prefix)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| MessageError::Storage(e.to_string()))?;

        for (id,) in &ids {
            let event = stamp.event(
                crate::events::MESSAGE_ARCHIVED,
                serde_json::json!({ "id": id, "archived_at": now, "reason": "entity-past-relevancy" }),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(|e| MessageError::Storage(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(ids.len() as u32)
    }

    async fn archive_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), MessageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        let result = sqlx::query("UPDATE messages SET kind = 'archived' WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MessageError::NotFound(format!("no message with ID {id}")));
        }
        // OUTBOX (phase 2): the archived event records only after the
        // row actually updated.
        let event = stamp.event(
            crate::events::MESSAGE_ARCHIVED,
            serde_json::json!({ "id": id, "archived_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(MessageError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| MessageError::Storage(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    sender_id: String,
    recipient_id: String,
    subject: String,
    body: String,
    entity_type: Option<String>,
    entity_id: Option<String>,
    entity_path: Option<String>,
    kind: String,
    sent_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
    reply_to: Option<String>,
}

impl MessageRow {
    fn into_message(self) -> Message {
        let entity_ref = match (self.entity_type, self.entity_id) {
            (Some(et), Some(eid)) => Some(EntityRef {
                entity_type: et,
                entity_id: eid,
                entity_path: self.entity_path,
            }),
            _ => None,
        };
        Message {
            id: self.id,
            sender_id: self.sender_id,
            recipient_id: self.recipient_id,
            subject: self.subject,
            body: self.body,
            entity_ref,
            // Free-text Class code; the column holds the kebab string,
            // so the newtype wraps it as-is.
            kind: MessageKind::new(self.kind),
            sent_at: self.sent_at,
            read_at: self.read_at,
            reply_to: self.reply_to,
        }
    }
}

// MessageKind is a newtype around String accepting arbitrary values,
// so the adapter wraps the database column directly
// (`MessageKind::new(self.kind)`) — no parse step. Kind values are
// validated against the Class registry at the messages API boundary,
// not in this storage adapter.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_serde() {
        // Transparent newtype serializes to the bare kebab code the
        // column stores and round-trips back.
        for code in [
            MessageKind::DIRECT,
            MessageKind::SIGNAL,
            MessageKind::ARCHIVED,
        ] {
            let k = MessageKind::new(code);
            assert_eq!(k.as_str(), code);
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(json, format!("\"{code}\""));
            let back: MessageKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, k);
        }
    }
}
