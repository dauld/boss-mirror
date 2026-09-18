//! Postgres adapter for `DeliveryPolicyRepository`.
//!
//! One door, same as `cadence::postgres`: the conductor runs OUTSIDE
//! the cluster and has no database of its own to consult, so the number
//! an operator reads and the number the pipeline obeys are the same
//! number. The cadence registry learned that the expensive way on
//! 2026-08-13, when the conductor's private pool pointed at a different
//! Postgres and the system of record answered "why has the train not
//! boarded" confidently and wrongly.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{DeliveryPolicyError, DeliveryPolicyRegistry, DeliveryPolicyRepository};
use super::types::{DeliveryPolicyRow, DeliveryPolicySpec};
use crate::registry::WorkflowStatus;

/// The policy columns, ONCE, for every SELECT and the seed's INSERT
/// on the table. A column added to the row is added here, and (since
/// the bundle seed reads the same list) a column missing here is one
/// the equality pin cannot compare.
const COLUMNS: &str = "name, version, max_red_trains, stall_hours, \
                       consist_budget_secs, consist_output_budget, consist_files_named, \
                       skip_reason_file_budget, blip_cause_budget, ci_host_floor_gb, \
                       gate_max_concurrent";

pub struct PgDeliveryPolicy {
    pool: PgPool,
}

impl PgDeliveryPolicy {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> DeliveryPolicyError {
    DeliveryPolicyError::Storage(e.to_string())
}

fn row_of(row: &sqlx::postgres::PgRow) -> Result<DeliveryPolicyRow, DeliveryPolicyError> {
    Ok(DeliveryPolicyRow {
        name: row.try_get("name").map_err(storage)?,
        version: row.try_get("version").map_err(storage)?,
        max_red_trains: row.try_get("max_red_trains").map_err(storage)?,
        stall_hours: row.try_get("stall_hours").map_err(storage)?,
        consist_budget_secs: row.try_get("consist_budget_secs").map_err(storage)?,
        consist_output_budget: row.try_get("consist_output_budget").map_err(storage)?,
        consist_files_named: row.try_get("consist_files_named").map_err(storage)?,
        skip_reason_file_budget: row.try_get("skip_reason_file_budget").map_err(storage)?,
        blip_cause_budget: row.try_get("blip_cause_budget").map_err(storage)?,
        ci_host_floor_gb: row.try_get("ci_host_floor_gb").map_err(storage)?,
        gate_max_concurrent: row.try_get("gate_max_concurrent").map_err(storage)?,
    })
}

#[async_trait]
impl DeliveryPolicyRepository for PgDeliveryPolicy {
    async fn active_policy(
        &self,
        name: &str,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM delivery_policy WHERE name = $1 AND status = 'active'"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.as_ref().map(row_of).transpose()
    }

    async fn policy_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError> {
        // Status is deliberately not filtered: a train in flight reads
        // the version it departed under, and that version may have been
        // retired by an edit made while it was running. That is the
        // whole point of pinning.
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM delivery_policy WHERE name = $1 AND version = $2"
        ))
        .bind(name)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.as_ref().map(row_of).transpose()
    }
}

#[async_trait]
impl DeliveryPolicyRegistry for PgDeliveryPolicy {
    async fn live_versions(
        &self,
        name: &str,
    ) -> Result<Vec<DeliveryPolicySpec>, DeliveryPolicyError> {
        let rows = sqlx::query(&format!(
            "SELECT status, created_at, {COLUMNS} \
             FROM delivery_policy WHERE name = $1 ORDER BY version"
        ))
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let status: String = row.try_get("status").map_err(storage)?;
                Ok(DeliveryPolicySpec {
                    status: status.parse().map_err(DeliveryPolicyError::Storage)?,
                    row: row_of(row)?,
                    created_at: row.try_get("created_at").map_err(storage)?,
                })
            })
            .collect()
    }

    async fn publish_declared(
        &self,
        mut spec: DeliveryPolicySpec,
        _actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<DeliveryPolicySpec, DeliveryPolicyError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        let exists: Option<(i32,)> =
            sqlx::query_as("SELECT version FROM delivery_policy WHERE name = $1 AND version = $2")
                .bind(spec.name())
                .bind(spec.version())
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?;
        if exists.is_some() {
            return Err(DeliveryPolicyError::Conflict(format!(
                "row already exists: {}@{}",
                spec.name(),
                spec.version()
            )));
        }

        // RETIRE BY NAME, THEN INSERT — the order 202609050500 used, and
        // the one `delivery_policy_one_active_per_name` demands: a plain
        // partial unique index, enforced per statement.
        sqlx::query(
            "UPDATE delivery_policy SET status = 'retired' WHERE name = $1 AND status = 'active'",
        )
        .bind(spec.name())
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        let r = &spec.row;
        sqlx::query(&format!(
            "INSERT INTO delivery_policy (status, created_at, {COLUMNS}) \
             VALUES ('active', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
        ))
        .bind(spec.created_at)
        .bind(&r.name)
        .bind(r.version)
        .bind(r.max_red_trains)
        .bind(r.stall_hours)
        .bind(r.consist_budget_secs)
        .bind(r.consist_output_budget)
        .bind(r.consist_files_named)
        .bind(r.skip_reason_file_budget)
        .bind(r.blip_cause_budget)
        .bind(r.ci_host_floor_gb)
        .bind(r.gate_max_concurrent)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        tx.commit().await.map_err(storage)?;
        Ok(spec)
    }
}
