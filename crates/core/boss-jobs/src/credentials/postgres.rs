//! Postgres adapter for `CredentialsRegistry`.
//!
//! One door, same reasoning as `delivery::postgres`: the audit runs
//! on the forge host and `boss credential list` runs wherever a
//! session is — neither has a database of its own, so the row an
//! operator reads and the row the audit compares are the same row.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::CredentialRow;

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
}
