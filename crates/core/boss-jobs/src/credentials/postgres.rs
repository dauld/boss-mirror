//! Postgres adapter for `CredentialsRegistry`.
//!
//! One door, same reasoning as `delivery::postgres`: the audit runs
//! on the forge host and `boss credential list` runs wherever a
//! session is — neither has a database of its own, so the row an
//! operator reads and the row the audit compares are the same row.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use boss_core::publisher::EventStamp;

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::{CredentialRow, RotationPhase};

const COLUMNS: &str = "id, kind, issuer, principal, scopes, storage_location, consumers, \
                       rotation_policy, rotated_at, notes";

pub struct PgCredentials {
    pool: PgPool,
}

impl PgCredentials {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> CredentialsError {
    CredentialsError::Storage(e.to_string())
}

fn row_of(row: &sqlx::postgres::PgRow) -> Result<CredentialRow, CredentialsError> {
    Ok(CredentialRow {
        id: row.try_get("id").map_err(storage)?,
        kind: row.try_get("kind").map_err(storage)?,
        issuer: row.try_get("issuer").map_err(storage)?,
        principal: row.try_get("principal").map_err(storage)?,
        scopes: row.try_get("scopes").map_err(storage)?,
        storage_location: row.try_get("storage_location").map_err(storage)?,
        consumers: row.try_get("consumers").map_err(storage)?,
        rotation_policy: row.try_get("rotation_policy").map_err(storage)?,
        rotated_at: row.try_get("rotated_at").map_err(storage)?,
        notes: row.try_get("notes").map_err(storage)?,
    })
}

#[async_trait]
impl CredentialsRegistry for PgCredentials {
    async fn list(&self) -> Result<Vec<CredentialRow>, CredentialsError> {
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM credentials ORDER BY id"))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(row_of).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialRow>, CredentialsError> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM credentials WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(row_of).transpose()
    }

    async fn record_rotation(
        &self,
        id: &str,
        phase: RotationPhase,
        evidence: serde_json::Value,
        stamp: &EventStamp,
    ) -> Result<(), CredentialsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // The row check and the conditional `rotated_at` stamp are one
        // statement: the install phase updates, every other phase
        // touches nothing but must still prove the row exists before
        // an event may annotate it. `rotated_at` binds the SAME
        // instant the event carries (stamp.timestamp), the scheduling
        // adapter's one-instant lesson.
        let exists: bool = if phase == RotationPhase::Installed {
            sqlx::query("UPDATE credentials SET rotated_at = $2 WHERE id = $1")
                .bind(id)
                .bind(stamp.timestamp)
                .execute(&mut *tx)
                .await
                .map_err(storage)?
                .rows_affected()
                > 0
        } else {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?
        };
        if !exists {
            return Err(CredentialsError::UnknownCredential(id.to_string()));
        }
        // OUTBOX: the event records in the same transaction as the
        // row, through the reliable-delivery path every other in-tx
        // recorder uses — the log and the registry cannot disagree
        // about whether this phase was recorded.
        let event = stamp.event(phase.event_kind(), evidence);
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CredentialsError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }
}
