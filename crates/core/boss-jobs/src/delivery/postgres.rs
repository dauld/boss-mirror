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
use sqlx::{PgPool, Row};

use super::port::{DeliveryPolicyError, DeliveryPolicyRepository};
use super::types::DeliveryPolicyRow;

const COLUMNS: &str = "name, version, max_red_trains, stall_hours, consist_excluded_lints, \
                       consist_budget_secs, consist_output_budget, consist_files_named, \
                       skip_reason_file_budget, blip_cause_budget, ci_host_floor_gb";

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
        consist_excluded_lints: row.try_get("consist_excluded_lints").map_err(storage)?,
        consist_budget_secs: row.try_get("consist_budget_secs").map_err(storage)?,
        consist_output_budget: row.try_get("consist_output_budget").map_err(storage)?,
        consist_files_named: row.try_get("consist_files_named").map_err(storage)?,
        skip_reason_file_budget: row.try_get("skip_reason_file_budget").map_err(storage)?,
        blip_cause_budget: row.try_get("blip_cause_budget").map_err(storage)?,
        ci_host_floor_gb: row.try_get("ci_host_floor_gb").map_err(storage)?,
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
