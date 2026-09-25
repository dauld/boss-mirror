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
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use super::port::{CadenceError, CadenceRegistry, CadenceRepository};
use super::types::{CadenceRuleRow, CadenceRuleSpec, LastFiring, NewFiring};
use crate::registry::WorkflowStatus;

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

/// The rule columns, ONCE, for every SELECT on the table. EVERY COLUMN
/// A BASIS NEEDS MUST BE HERE. The calendar basis (202608282135) added
/// `cadence`, `anchor_date` and `business_calendar`; the conductor's
/// own SELECT was not widened when they landed, and the loop skipped
/// protocol-retro-daily on every tick — the rule was in the table and
/// visible over the API the whole time. An unserved column is a rule
/// the loop cannot read, and (since the bundle seed reads the same
/// list) a column the equality pin cannot compare.
const RULE_COLUMNS: &str = "name, verb, basis, every_minutes, at_times, min_dock_depth, \
     cooldown_minutes, cadence, anchor_date, business_calendar, regate_hold_minutes";

fn rule_of(row: &PgRow) -> Result<CadenceRuleRow, CadenceError> {
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
        regate_hold_minutes: row.try_get("regate_hold_minutes").map_err(storage)?,
    })
}

#[async_trait]
impl CadenceRepository for PgCadence {
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError> {
        let rows = sqlx::query(&format!(
            "SELECT {RULE_COLUMNS} FROM cadence_rules WHERE status = 'active' ORDER BY name"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(rule_of).collect()
    }

    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, CadenceError> {
        // `rc` is merged into `detail` by record_outcome rather than held in
        // a column of its own, so it is read back out of the JSON here. A
        // firing with no outcome yet has no `rc` key and reads as NULL —
        // which is the "still in flight" case evaluation must distinguish
        // from a failure.
        let row = sqlx::query(
            "SELECT firing_id, fired_at, (detail->>'rc')::int AS rc FROM cadence_firings \
             WHERE rule_name = $1 ORDER BY fired_at DESC LIMIT 1",
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
                rc: r.try_get("rc").map_err(storage)?,
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

#[async_trait]
impl CadenceRegistry for PgCadence {
    async fn live_versions(&self, name: &str) -> Result<Vec<CadenceRuleSpec>, CadenceError> {
        let rows = sqlx::query(&format!(
            "SELECT version, status, created_at, {RULE_COLUMNS} \
             FROM cadence_rules WHERE name = $1 ORDER BY version"
        ))
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let status: String = row.try_get("status").map_err(storage)?;
                Ok(CadenceRuleSpec {
                    version: row.try_get("version").map_err(storage)?,
                    status: status.parse().map_err(CadenceError::Storage)?,
                    row: rule_of(row)?,
                    created_at: row.try_get("created_at").map_err(storage)?,
                })
            })
            .collect()
    }

    async fn publish_declared(
        &self,
        mut spec: CadenceRuleSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<CadenceRuleSpec, CadenceError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        // The newest version the lineage holds, any status — the same
        // question the in-memory adapter asks, so a publish at or below
        // it is the same 409 over either. Locked for the transaction:
        // two publishes of one name racing past this read would both
        // pass the bound and the second insert would trip the (name,
        // version) key as a 500 instead of the 409 it is.
        let newest: Option<i32> = sqlx::query_scalar(
            "SELECT MAX(version) FROM (SELECT version FROM cadence_rules WHERE name = $1 FOR UPDATE) v",
        )
        .bind(spec.name())
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let newest = super::in_memory::newest_version(newest.into_iter());
        if spec.version <= newest {
            return Err(super::in_memory::not_above(
                spec.name(),
                spec.version,
                newest,
            ));
        }

        // RETIRE BY NAME, THEN INSERT — the safe supersede idiom
        // 202609032030 established after a version-keyed retire missed
        // the live row and a depth change silently never took.
        // `cadence_rules_one_active_per_name` is a plain partial unique
        // index, enforced per statement, so the order is load-bearing.
        sqlx::query(
            "UPDATE cadence_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
        )
        .bind(spec.name())
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        let r = &spec.row;
        sqlx::query(&format!(
            "INSERT INTO cadence_rules (version, status, created_at, {RULE_COLUMNS}) \
             VALUES ($1, 'active', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"
        ))
        .bind(spec.version)
        .bind(spec.created_at)
        .bind(&r.name)
        .bind(&r.verb)
        .bind(&r.basis)
        .bind(r.every_minutes)
        .bind(&r.at_times)
        .bind(r.min_dock_depth)
        .bind(r.cooldown_minutes)
        .bind(&r.cadence)
        .bind(r.anchor_date)
        .bind(&r.business_calendar)
        .bind(r.regate_hold_minutes)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // The row and its fact commit or roll back together (the
        // stations posture): staged on the outbox, moved to audit_log
        // and the bus by boss-event-relay.
        let event =
            crate::events::cadence_registry_event(crate::events::CADENCE_PUBLISHED, actor, &spec);
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CadenceError::Storage)?;

        tx.commit().await.map_err(storage)?;
        Ok(spec)
    }

    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<Option<CadenceRuleSpec>, CadenceError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        // Read the active row first — the event payload is the retired
        // row, and the nothing-active path must write and record
        // nothing. At most one row is active per name
        // (`cadence_rules_one_active_per_name`), so this is the row.
        let active = sqlx::query(&format!(
            "SELECT version, status, created_at, {RULE_COLUMNS} \
             FROM cadence_rules WHERE name = $1 AND status = 'active' FOR UPDATE"
        ))
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let Some(row) = active else {
            return Ok(None);
        };
        let mut spec = CadenceRuleSpec {
            version: row.try_get("version").map_err(storage)?,
            status: WorkflowStatus::Active,
            row: rule_of(&row)?,
            created_at: row.try_get("created_at").map_err(storage)?,
        };

        sqlx::query(
            "UPDATE cadence_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
        )
        .bind(name)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        spec.status = WorkflowStatus::Retired;

        let event =
            crate::events::cadence_registry_event(crate::events::CADENCE_RETIRED, actor, &spec);
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CadenceError::Storage)?;

        tx.commit().await.map_err(storage)?;
        Ok(Some(spec))
    }
}
