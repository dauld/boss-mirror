//! Postgres adapter for `PolicyRepository`.
//!
//! Backs the three tables from `infra/postgres/schema/04-policy.sql`:
//!   - `policy_rules` — role × resource × action → scope
//!   - `policy_user_overrides` — per-user exceptions (delegation)
//!   - `policy_rule_audit` — append-only change log
//!
//! All writes emit an audit row in the same transaction as the
//! primary mutation.

use async_trait::async_trait;
use sqlx::PgPool;

use boss_policy_client::port::{Judge, PolicyError, PolicyRepository, ReconcileStats, id_taken};
use boss_policy_client::types::{Action, PolicyRule, Resource, Scope, UserOverride};

pub struct PgPolicy {
    pool: PgPool,
}

impl PgPolicy {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn parse_action(s: &str) -> Result<Action, PolicyError> {
    s.parse()
        .map_err(|e| PolicyError::Storage(format!("action: {e}")))
}

fn parse_resource(s: &str) -> Result<Resource, PolicyError> {
    s.parse()
        .map_err(|e| PolicyError::Storage(format!("resource: {e}")))
}

fn parse_scope(s: &str) -> Result<Scope, PolicyError> {
    Scope::from_db_string(s).map_err(|e| PolicyError::Storage(format!("scope: {e}")))
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    id: String,
    role: String,
    resource: String,
    action: String,
    scope: String,
    active: bool,
}

impl RuleRow {
    fn into_rule(self) -> Result<PolicyRule, PolicyError> {
        Ok(PolicyRule {
            id: self.id,
            role: self.role,
            resource: parse_resource(&self.resource)?,
            action: parse_action(&self.action)?,
            scope: parse_scope(&self.scope)?,
            active: self.active,
        })
    }
}

#[derive(sqlx::FromRow)]
struct OverrideRow {
    id: String,
    user_id: String,
    resource: String,
    action: String,
    scope: String,
    reason: String,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl OverrideRow {
    fn into_override(self) -> Result<UserOverride, PolicyError> {
        Ok(UserOverride {
            id: self.id,
            user_id: self.user_id,
            resource: parse_resource(&self.resource)?,
            action: parse_action(&self.action)?,
            scope: parse_scope(&self.scope)?,
            reason: self.reason,
            expires_at: self.expires_at,
        })
    }
}

#[async_trait]
impl PolicyRepository for PgPolicy {
    async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError> {
        let rows: Vec<RuleRow> = sqlx::query_as(
            "SELECT id, role, resource, action, scope, active \
             FROM policy_rules WHERE active = TRUE ORDER BY role, resource, action",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        rows.into_iter().map(|r| r.into_rule()).collect()
    }

    async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError> {
        let row: Option<RuleRow> = sqlx::query_as(
            "SELECT id, role, resource, action, scope, active \
             FROM policy_rules WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        row.map(|r| r.into_rule()).transpose()
    }

    async fn upsert_rule_judged(
        &self,
        rule: &PolicyRule,
        changed_by: &str,
        judge: Judge<'_, PolicyRule>,
    ) -> Result<(), PolicyError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;

        // The row this write would change, active or not, locked until
        // the write commits — and judged here, not by an earlier port
        // call that another writer could race (backlog b8e75382, F5).
        let existing: Option<RuleRow> = sqlx::query_as(
            "SELECT id, role, resource, action, scope, active \
             FROM policy_rules WHERE id = $1 FOR UPDATE",
        )
        .bind(&rule.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        let existing = existing.map(RuleRow::into_rule).transpose()?;
        judge(existing.as_ref()).map_err(PolicyError::Refused)?;

        // Capture the pre-image for the audit log.
        let before: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT row_to_json(r) FROM policy_rules r WHERE id = $1")
                .bind(&rule.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| PolicyError::Storage(e.to_string()))?;

        // A missing row cannot be locked, so a write judged against "no
        // row" inserts or fails: if another writer created the row after
        // the read above, updating it would apply a Create's judgement to
        // an Update.
        let conflict = if existing.is_some() {
            "ON CONFLICT (id) DO UPDATE SET \
                scope = EXCLUDED.scope, \
                active = EXCLUDED.active, \
                updated_at = NOW(), \
                updated_by = EXCLUDED.updated_by"
        } else {
            "ON CONFLICT (id) DO NOTHING"
        };
        let written = sqlx::query(&format!(
            "INSERT INTO policy_rules (id, role, resource, action, scope, active, \
                created_at, created_by, updated_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, NOW(), $7) {conflict}"
        ))
        .bind(&rule.id)
        .bind(&rule.role)
        .bind(rule.resource.as_str())
        .bind(rule.action.as_str())
        .bind(rule.scope.to_db_string())
        .bind(rule.active)
        .bind(changed_by)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        if written.rows_affected() == 0 {
            return Err(PolicyError::Conflict(format!(
                "rule {} was created by another write while this one was judged as its \
                 creation; read it and send the write again",
                rule.id
            )));
        }

        let after = serde_json::to_value(rule).ok();

        sqlx::query(
            "INSERT INTO policy_rule_audit (kind, target_id, changed_by, before, after) \
             VALUES ('rule.upsert', $1, $2, $3, $4)",
        )
        .bind(&rule.id)
        .bind(changed_by)
        .bind(before)
        .bind(after)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn deactivate_rule(&self, id: &str, changed_by: &str) -> Result<(), PolicyError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;

        let before: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT row_to_json(r) FROM policy_rules r WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| PolicyError::Storage(e.to_string()))?;

        if before.is_none() {
            return Err(PolicyError::NotFound(id.to_string()));
        }

        sqlx::query(
            "UPDATE policy_rules SET active = FALSE, updated_at = NOW(), updated_by = $2 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(changed_by)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        sqlx::query(
            "INSERT INTO policy_rule_audit (kind, target_id, changed_by, before) \
             VALUES ('rule.deactivate', $1, $2, $3)",
        )
        .bind(id)
        .bind(changed_by)
        .bind(before)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_user_overrides(&self, user_id: &str) -> Result<Vec<UserOverride>, PolicyError> {
        let rows: Vec<OverrideRow> = sqlx::query_as(
            "SELECT id, user_id, resource, action, scope, reason, expires_at \
             FROM policy_user_overrides \
             WHERE user_id = $1 \
             AND (expires_at IS NULL OR expires_at > NOW()) \
             ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        rows.into_iter().map(|r| r.into_override()).collect()
    }

    async fn user_override(&self, id: &str) -> Result<Option<UserOverride>, PolicyError> {
        let row: Option<OverrideRow> = sqlx::query_as(
            "SELECT id, user_id, resource, action, scope, reason, expires_at \
             FROM policy_user_overrides WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        row.map(OverrideRow::into_override).transpose()
    }

    async fn upsert_user_override_judged(
        &self,
        ov: &UserOverride,
        changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;

        // The row on the conflict key, expired or not: the upsert below
        // rewrites THAT row, so it is the one judged (backlog b8e75382,
        // F5 — the door read live rows only, judged a revival a Create).
        let existing: Option<OverrideRow> = sqlx::query_as(
            "SELECT id, user_id, resource, action, scope, reason, expires_at \
             FROM policy_user_overrides \
             WHERE user_id = $1 AND resource = $2 AND action = $3 FOR UPDATE",
        )
        .bind(&ov.user_id)
        .bind(ov.resource.as_str())
        .bind(ov.action.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        let existing = existing.map(OverrideRow::into_override).transpose()?;
        judge(existing.as_ref()).map_err(PolicyError::Refused)?;

        // A new row under an id another key owns would violate the
        // primary key, which surfaced as a storage failure (S2 of the
        // hold review of car a8becd52). Named instead, as the Conflict
        // it is.
        if existing.is_none() {
            let owner: Option<OverrideRow> = sqlx::query_as(
                "SELECT id, user_id, resource, action, scope, reason, expires_at \
                 FROM policy_user_overrides WHERE id = $1",
            )
            .bind(&ov.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;
            if let Some(owner) = owner.map(OverrideRow::into_override).transpose()? {
                return Err(id_taken(ov, &owner));
            }
        }

        // The pre-image is the row the conflict key rewrites, which is
        // not always the one under the body's id.
        let before: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT row_to_json(o) FROM policy_user_overrides o \
             WHERE user_id = $1 AND resource = $2 AND action = $3",
        )
        .bind(&ov.user_id)
        .bind(ov.resource.as_str())
        .bind(ov.action.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        // As for rules: a write judged against no row inserts or fails.
        let conflict = if existing.is_some() {
            "ON CONFLICT (user_id, resource, action) DO UPDATE SET \
                scope = EXCLUDED.scope, \
                reason = EXCLUDED.reason, \
                expires_at = EXCLUDED.expires_at"
        } else {
            "ON CONFLICT (user_id, resource, action) DO NOTHING"
        };
        let written = sqlx::query(&format!(
            "INSERT INTO policy_user_overrides \
                (id, user_id, resource, action, scope, reason, expires_at, created_at, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), $8) {conflict}"
        ))
        .bind(&ov.id)
        .bind(&ov.user_id)
        .bind(ov.resource.as_str())
        .bind(ov.action.as_str())
        .bind(ov.scope.to_db_string())
        .bind(&ov.reason)
        .bind(ov.expires_at)
        .bind(changed_by)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        if written.rows_affected() == 0 {
            return Err(PolicyError::Conflict(format!(
                "an override of {} on {} for {} was created by another write while this one was \
                 judged as its creation; read it and send the write again",
                ov.action.as_str(),
                ov.resource.as_str(),
                ov.user_id
            )));
        }

        let after = serde_json::to_value(ov).ok();

        sqlx::query(
            "INSERT INTO policy_rule_audit (kind, target_id, changed_by, before, after) \
             VALUES ('override.upsert', $1, $2, $3, $4)",
        )
        .bind(&ov.id)
        .bind(changed_by)
        .bind(before)
        .bind(after)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn deactivate_user_override_judged(
        &self,
        id: &str,
        changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;

        // Whether retiring this row widens anyone's access depends on
        // its scope and expiry now, so it is locked and judged here.
        let existing: Option<OverrideRow> = sqlx::query_as(
            "SELECT id, user_id, resource, action, scope, reason, expires_at \
             FROM policy_user_overrides WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;
        let Some(existing) = existing.map(OverrideRow::into_override).transpose()? else {
            return Err(PolicyError::NotFound(id.to_string()));
        };
        judge(Some(&existing)).map_err(PolicyError::Refused)?;

        let before: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT row_to_json(o) FROM policy_user_overrides o WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| PolicyError::Storage(e.to_string()))?;

        if before.is_none() {
            return Err(PolicyError::NotFound(id.to_string()));
        }

        sqlx::query("UPDATE policy_user_overrides SET expires_at = NOW() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;

        sqlx::query(
            "INSERT INTO policy_rule_audit (kind, target_id, changed_by, before) \
             VALUES ('override.deactivate', $1, $2, $3)",
        )
        .bind(id)
        .bind(changed_by)
        .bind(before)
        .execute(&mut *tx)
        .await
        .map_err(|e| PolicyError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| PolicyError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn bootstrap_reconcile(
        &self,
        defaults: &[PolicyRule],
    ) -> Result<ReconcileStats, PolicyError> {
        let mut stats = ReconcileStats::default();
        for rule in defaults {
            // Read current row + its `updated_by` so we can decide
            // between insert / refresh / preserve in one place.
            let row: Option<(String, bool, String)> =
                sqlx::query_as("SELECT scope, active, updated_by FROM policy_rules WHERE id = $1")
                    .bind(&rule.id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| PolicyError::Storage(e.to_string()))?;

            match row {
                None => {
                    self.upsert_rule(rule, "bootstrap").await?;
                    stats.inserted += 1;
                }
                Some((scope, active, updated_by)) if updated_by == "bootstrap" => {
                    let scope_changed = scope != rule.scope.to_db_string();
                    let active_changed = active != rule.active;
                    if scope_changed || active_changed {
                        self.upsert_rule(rule, "bootstrap").await?;
                        stats.refreshed += 1;
                    } else {
                        stats.unchanged += 1;
                    }
                }
                Some(_) => {
                    stats.preserved += 1;
                }
            }
        }
        Ok(stats)
    }
}
