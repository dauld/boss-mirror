//! Postgres adapter for [`ClassRepository`]. Queries the single
//! `classes` table.

use async_trait::async_trait;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::port::{ClassError, ClassRepository, declared_event};

pub struct PgClasses {
    pool: PgPool,
}

impl PgClasses {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Row shape mirroring the `classes` table. Kept private so
/// `boss-core::Class` doesn't need a `sqlx::FromRow` derive (which
/// would force `sqlx` into `boss-core`'s dep graph).
#[derive(sqlx::FromRow)]
struct ClassRow {
    subject_kind: String,
    code: String,
    display_name: String,
    parent_code: Option<String>,
    member_attribute: Option<String>,
    metadata: Value,
    sort_order: i32,
    retired_at: Option<DateTime<Utc>>,
}

impl From<ClassRow> for Class {
    fn from(r: ClassRow) -> Self {
        Self {
            subject_kind: r.subject_kind,
            code: r.code,
            display_name: r.display_name,
            parent_code: r.parent_code,
            member_attribute: r.member_attribute,
            metadata: r.metadata,
            sort_order: r.sort_order,
            retired_at: r.retired_at,
        }
    }
}

const SELECT_COLUMNS: &str = "subject_kind, code, display_name, parent_code, member_attribute, \
     metadata, sort_order, retired_at";

#[async_trait]
impl ClassRepository for PgClasses {
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM classes \
             WHERE subject_kind = $1 AND retired_at IS NULL \
             ORDER BY sort_order ASC, code ASC"
        );
        let rows: Vec<ClassRow> = sqlx::query_as(&sql)
            .bind(subject_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM classes \
             WHERE subject_kind = $1 AND code = $2"
        );
        let row: Option<ClassRow> = sqlx::query_as(&sql)
            .bind(&class_ref.subject_kind)
            .bind(&class_ref.code)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(row.map(Into::into))
    }

    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM classes \
             WHERE subject_kind = $1 AND code = $2 AND retired_at IS NULL)",
        )
        .bind(&class_ref.subject_kind)
        .bind(&class_ref.code)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(exists)
    }

    async fn update(&self, class: &Class) -> Result<bool, ClassError> {
        // The composite key is deliberately absent from the SET list:
        // a code is an identity other rows point at, so renaming it in
        // place would orphan them silently. `retired_at` is likewise
        // untouched — retiring a Class is its own action, not a side
        // effect of editing a label.
        let result = sqlx::query(
            "UPDATE classes SET \
             display_name = $3, parent_code = $4, member_attribute = $5, \
             metadata = $6, sort_order = $7, updated_at = now() \
             WHERE subject_kind = $1 AND code = $2",
        )
        .bind(&class.subject_kind)
        .bind(&class.code)
        .bind(&class.display_name)
        .bind(&class.parent_code)
        .bind(&class.member_attribute)
        .bind(&class.metadata)
        .bind(class.sort_order)
        .execute(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn retire(&self, class_ref: &ClassRef) -> Result<bool, ClassError> {
        // Stamp only when not already stamped — when a Class was
        // withdrawn is a fact, and a repeat call must not move it.
        let stamped = sqlx::query(
            "UPDATE classes SET retired_at = now(), updated_at = now() \
             WHERE subject_kind = $1 AND code = $2 AND retired_at IS NULL",
        )
        .bind(&class_ref.subject_kind)
        .bind(&class_ref.code)
        .execute(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        if stamped.rows_affected() > 0 {
            return Ok(true);
        }
        // Nothing stamped: either already retired (idempotent success)
        // or no such row (the caller's 404).
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM classes WHERE subject_kind = $1 AND code = $2)",
        )
        .bind(&class_ref.subject_kind)
        .bind(&class_ref.code)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(exists)
    }

    async fn batch_upsert(&self, rows: &[Class], stamp: &EventStamp) -> Result<u64, ClassError> {
        // One `ON CONFLICT DO NOTHING` per row inside a single
        // transaction, mirroring the idempotent semantics of the seed
        // `classes.sql`. `created_at` / `updated_at` default in the
        // table; `retired_at` is intentionally not seeded (rows arrive
        // active). Row-at-a-time keeps the bind logic trivial — the
        // registry is tiny (≤ a few hundred rows) so a multi-row VALUES
        // batch would buy nothing — and is what lets `rows_affected`
        // say, per row, whether THIS one was inserted: only those
        // stage a `class.declared` on the outbox, in the same
        // transaction, so the fact and the row commit or roll back
        // together (backlog d9409039).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        let mut inserted: u64 = 0;
        for r in rows {
            let result = sqlx::query(
                "INSERT INTO classes \
                 (subject_kind, code, display_name, parent_code, member_attribute, metadata, sort_order) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (subject_kind, code) DO NOTHING",
            )
            .bind(&r.subject_kind)
            .bind(&r.code)
            .bind(&r.display_name)
            .bind(&r.parent_code)
            .bind(&r.member_attribute)
            .bind(&r.metadata)
            .bind(r.sort_order)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                // The classes.subject_kind FK (SQLSTATE 23503):
                // surface WHICH kind was unregistered instead of a
                // generic storage 500.
                if let sqlx::Error::Database(ref dbe) = e
                    && dbe.code().as_deref() == Some("23503")
                {
                    return ClassError::UnregisteredKind(r.subject_kind.clone());
                }
                ClassError::Storage(e.to_string())
            })?;
            if result.rows_affected() == 1 {
                boss_events::outbox::record_event_in_tx(&mut tx, &declared_event(stamp, r)?)
                    .await
                    .map_err(ClassError::Storage)?;
                inserted += 1;
            }
        }
        tx.commit()
            .await
            .map_err(|e| ClassError::Storage(e.to_string()))?;
        Ok(inserted)
    }
}
