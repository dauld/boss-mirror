//! Postgres adapter for `CadenceRepository`.
//!
//! The four statements moved here verbatim from the conductor's
//! private pool (`boss-cli/src/cadence.rs`), which is the whole point
//! of the module: the conductor used to reach a database directly, and
//! on 2026-08-13 it reached a DIFFERENT one than the operator was
//! reading, so the registry said `min_dock_depth = 4` while the
//! running loop enforced 8. One door means one answer.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{CadenceError, CadenceRepository};
use super::types::{CadenceRuleRow, LastFiring, NewFiring};

pub struct PgCadence {
    pool: PgPool,
}

impl PgCadence {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> CadenceError {
    CadenceError::Storage(e.to_string())
}

#[async_trait]
impl CadenceRepository for PgCadence {
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError> {
        // EVERY COLUMN A BASIS NEEDS MUST BE SELECTED HERE. The
        // calendar basis (202608282135) added `cadence`, `anchor_date`
        // and `business_calendar`; the conductor's own SELECT was not
        // widened when they landed, and the loop skipped
        // protocol-retro-daily on every tick — the rule was in the
        // table and visible over the API the whole time. Now that this
        // adapter is what serves the loop, an unserved column is a
        // rule the loop cannot read.
        let rows = sqlx::query(
            "SELECT name, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes, \
                    cadence, anchor_date, business_calendar \
             FROM cadence_rules WHERE status = 'active' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;

        rows.iter()
            .map(|row| {
                Ok(CadenceRuleRow {
                    name: row.try_get("name").map_err(storage)?,
                    verb: row.try_get("verb").map_err(storage)?,
                    basis: row.try_get("basis").map_err(storage)?,
                    every_minutes: row.try_get("every_minutes").map_err(storage)?,
                    at_times: row.try_get("at_times").map_err(storage)?,
                    min_dock_depth: row.try_get("min_dock_depth").map_err(storage)?,
                    cooldown_minutes: row.try_get("cooldown_minutes").map_err(storage)?,
                    cadence: row.try_get("cadence").map_err(storage)?,
                    anchor_date: row.try_get("anchor_date").map_err(storage)?,
                    business_calendar: row.try_get("business_calendar").map_err(storage)?,
                })
            })
            .collect()
    }

    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, CadenceError> {
        let row = sqlx::query(
            "SELECT firing_id, fired_at FROM cadence_firings WHERE rule_name = $1 \
             ORDER BY fired_at DESC LIMIT 1",
        )
        .bind(rule)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;

        match row {
            None => Ok(None),
            Some(r) => Ok(Some(LastFiring {
                firing_id: r.try_get("firing_id").map_err(storage)?,
                fired_at: r.try_get("fired_at").map_err(storage)?,
            })),
        }
    }

    async fn claim_firing(&self, new: &NewFiring) -> Result<bool, CadenceError> {
        let fired_at: DateTime<Utc> = new.fired_at;
        let res = sqlx::query(
            "INSERT INTO cadence_firings (firing_id, rule_name, verb, basis, fired_at, detail) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (firing_id) DO NOTHING",
        )
        .bind(&new.firing_id)
        .bind(&new.rule_name)
        .bind(&new.verb)
        .bind(&new.basis)
        // boss-clock time, bound by the caller — never the DB wallclock.
        .bind(fired_at)
        .bind(&new.detail)
        .execute(&self.pool)
        .await
        .map_err(storage)?;

        Ok(res.rows_affected() == 1)
    }

    async fn record_outcome(
        &self,
        firing_id: &str,
        rc: i32,
        runtime_secs: u64,
    ) -> Result<(), CadenceError> {
        sqlx::query("UPDATE cadence_firings SET detail = detail || $2 WHERE firing_id = $1")
            .bind(firing_id)
            .bind(serde_json::json!({ "rc": rc, "runtime_secs": runtime_secs }))
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }
}
