//! Postgres adapter for `CredentialsRegistry`.
//!
//! One door, same reasoning as `delivery::postgres`: the audit runs
//! on the forge host and `boss credential list` runs wherever a
//! session is — neither has a database of its own, so the row an
//! operator reads and the row the audit compares are the same row.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use boss_core::publisher::EventStamp;

use super::port::{CredentialsError, CredentialsRegistry, declared_event};
use super::types::{CredentialInput, CredentialRow, CredentialsBatchOutcome, RotationPhase};

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

fn storage_json(e: &serde_json::Error) -> CredentialsError {
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

    async fn publish(
        &self,
        tenant_id: &str,
        declared: &[CredentialInput],
        stamp: &EventStamp,
    ) -> Result<CredentialsBatchOutcome, CredentialsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let mut inserted = 0;
        for c in declared {
            let scopes = serde_json::to_value(&c.scopes).map_err(|e| storage_json(&e))?;
            let consumers = serde_json::to_value(&c.consumers).map_err(|e| storage_json(&e))?;
            // ON CONFLICT DO NOTHING, not DO UPDATE: the held row's
            // rotated_at and notes are the rotation path's book-keeping
            // (202609031700's mutability decision), and a declaration
            // that overwrote them would erase a rotation's record.
            let n = sqlx::query(
                "INSERT INTO credentials \
                 (id, kind, issuer, principal, scopes, storage_location, consumers, \
                  rotation_policy, rotated_at, notes) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(&c.id)
            .bind(&c.kind)
            .bind(&c.issuer)
            .bind(&c.principal)
            .bind(&scopes)
            .bind(&c.storage_location)
            .bind(&consumers)
            .bind(&c.rotation_policy)
            .bind(&c.notes)
            .execute(&mut *tx)
            .await
            .map_err(storage)?
            .rows_affected();
            if n > 0 {
                // OUTBOX, in the insert's own transaction: the row and
                // its `credential.declared` fact commit or roll back
                // together (20260917071313's rule for every tenant
                // batch door).
                boss_events::outbox::record_event_in_tx(
                    &mut tx,
                    &declared_event(stamp, tenant_id, c)?,
                )
                .await
                .map_err(CredentialsError::Storage)?;
                inserted += 1;
            }
        }
        tx.commit().await.map_err(storage)?;
        Ok(CredentialsBatchOutcome {
            received: declared.len(),
            inserted,
        })
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
