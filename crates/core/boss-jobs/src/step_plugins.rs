//! Step UX Plugin Registry — see `docs/architecture-decisions.md`
//! §Step UX & frontend.
//!
//! Third-party step kinds ship as plugins. v1 kinds defined in
//! `step_registry::v1()` are implicit — the in-tree catalog stays
//! the canonical source for those. The DB table only stores
//! plugins.
//!
//! Shape mirrors `WorkflowRegistry`: append-only versioning + a
//! status lifecycle (draft → active → retired), with a partial
//! unique index enforcing at most one active row per kind.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::registry::WorkflowStatus;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A full plugin row. Serializes directly to the `step_plugins`
/// JSONB columns with the same names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepPluginSpec {
    pub kind: String,
    pub version: i32,
    pub status: WorkflowStatus,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: String,
    pub metadata_schema: serde_json::Value,
    /// Relative path under `/var/lib/boss/step-plugins/` that the
    /// gateway serves from `/plugins/<path>`. v1 is always a static
    /// JS bundle path (Q2).
    pub frontend_url: String,
    pub owning_team: String,
    #[serde(default)]
    pub authoring_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl StepPluginSpec {
    /// Convenience constructor for tests + seeds.
    pub fn draft(
        kind: impl Into<String>,
        label: impl Into<String>,
        category: impl Into<String>,
        frontend_url: impl Into<String>,
        metadata_schema: serde_json::Value,
    ) -> Self {
        Self {
            kind: kind.into(),
            version: 1,
            status: WorkflowStatus::Draft,
            label: label.into(),
            description: None,
            category: category.into(),
            metadata_schema,
            frontend_url: frontend_url.into(),
            owning_team: "authoring".to_string(),
            authoring_job_id: None,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StepPluginError {
    #[error("step plugin not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

#[async_trait]
pub trait StepPluginRegistry: Send + Sync {
    /// Currently-active spec for `kind`.
    async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError>;

    /// Specific historical version.
    async fn get_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<StepPluginSpec, StepPluginError>;

    /// Every active spec, optionally filtered by category.
    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<StepPluginSpec>, StepPluginError>;

    /// Every version of one kind (oldest first). Includes drafts + retired.
    async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError>;

    /// Append a new draft row. Version = max(version)+1 or 1.
    ///
    /// Every write method takes `actor` + `now` because under 3P a
    /// registry write IS a network configuration change
    /// (protocol-policy-publish.md, Constraints): the adapter builds
    /// the corresponding event via
    /// `events::step_plugin_registry_event` and records it atomically
    /// with the row — the caller supplies the who (session actor, or
    /// a named automation) and the when (clock-routed, never
    /// wallclock in production paths). Records
    /// `jobs.step_plugin.draft_saved` (payload = the stored draft).
    async fn create_draft(
        &self,
        spec: StepPluginSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError>;

    /// Flip the latest draft to active and demote the previous
    /// active. Transactional; records `jobs.step_plugin.published`
    /// (payload = the promoted spec) with the row flips.
    async fn publish(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError>;

    /// Flip the active row to retired. Idempotent if already retired
    /// — and silent then too: only a write that touched a row records
    /// `jobs.step_plugin.retired` (payload = the retired spec).
    async fn retire(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), StepPluginError>;

    /// Publish `spec` at ITS OWN declared version — the platform
    /// bundle's write (`crate::step_plugin_seed`, backlog 393d3234).
    ///
    /// `create_draft` + `publish` assign `max(version) + 1`, which is
    /// right for an author and wrong for a bundle: a bundle row
    /// carries its version (a version bump is the edit path), and a
    /// fresh deployment must land `sign-off` at v3 — the version the
    /// migrations produced and every open step's
    /// `step_plugin_version` names — not at v1 with two versions of
    /// history nobody wrote.
    ///
    /// In one transaction: retire any active row of the same kind,
    /// INSERT the row active at `spec.version` with `created_at =
    /// now`, record `jobs.step_plugin.published` (payload = the row
    /// written). `Conflict` when (kind, version) already exists — the
    /// caller decides what an existing row means, this never
    /// overwrites one.
    async fn publish_declared(
        &self,
        spec: StepPluginSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError>;
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

pub struct InMemoryStepPlugins {
    rows: Arc<Mutex<HashMap<(String, i32), StepPluginSpec>>>,
    /// What the Pg adapter records into `event_outbox` inside the
    /// row transaction, this adapter collects here — same events at
    /// the same write points, so tests assert the event contract
    /// through the port without a database.
    recorded: Arc<Mutex<Vec<boss_core::event::Event>>>,
}

impl Default for InMemoryStepPlugins {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStepPlugins {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every event a write method recorded, in write order — the
    /// in-memory stand-in for `SELECT ... FROM event_outbox`.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().unwrap().clone()
    }

    fn record(&self, event: boss_core::event::Event) {
        self.recorded.lock().unwrap().push(event);
    }

    /// Seed helper for tests. Inserts as-is without versioning logic.
    pub fn seed(&self, spec: StepPluginSpec) -> Result<(), StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.kind.clone(), spec.version)) {
            return Err(StepPluginError::Conflict(format!(
                "row already exists: {}@{}",
                spec.kind, spec.version
            )));
        }
        rows.insert((spec.kind.clone(), spec.version), spec);
        Ok(())
    }

    fn snapshot(&self) -> Vec<StepPluginSpec> {
        self.rows.lock().unwrap().values().cloned().collect()
    }

    fn max_version(&self, kind: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(k, _)| k == kind)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl StepPluginRegistry for InMemoryStepPlugins {
    async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
        self.snapshot()
            .into_iter()
            .find(|r| r.kind == kind && r.status == WorkflowStatus::Active)
            .ok_or_else(|| StepPluginError::NotFound(format!("no active plugin: {kind}")))
    }

    async fn get_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<StepPluginSpec, StepPluginError> {
        self.rows
            .lock()
            .unwrap()
            .get(&(kind.to_string(), version))
            .cloned()
            .ok_or_else(|| StepPluginError::NotFound(format!("{kind}@v{version}")))
    }

    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        let mut rows: Vec<StepPluginSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == WorkflowStatus::Active)
            .filter(|r| category.is_none_or(|c| r.category == c))
            .collect();
        rows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(rows)
    }

    async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        let mut rows: Vec<StepPluginSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn create_draft(
        &self,
        mut spec: StepPluginSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = WorkflowStatus::Draft;
        spec.created_at = now;
        self.rows
            .lock()
            .unwrap()
            .insert((spec.kind.clone(), spec.version), spec.clone());
        self.record(crate::events::step_plugin_registry_event(
            crate::events::STEP_PLUGIN_DRAFT_SAVED,
            actor,
            &spec,
        ));
        Ok(spec)
    }

    async fn publish(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        let latest_draft = rows
            .values()
            .filter(|r| r.kind == kind && r.status == WorkflowStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                StepPluginError::NotFound(format!("no draft to publish for plugin: {kind}"))
            })?;

        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        let key = (latest_draft.kind.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = WorkflowStatus::Active;
        let promoted = row.clone();
        drop(rows);
        self.record(crate::events::step_plugin_registry_event(
            crate::events::STEP_PLUGIN_PUBLISHED,
            actor,
            &promoted,
        ));
        Ok(promoted)
    }

    async fn retire(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<(), StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        let any_active = rows
            .values()
            .any(|r| r.kind == kind && r.status == WorkflowStatus::Active);
        if !any_active {
            // Idempotent — nothing to do, so nothing to record.
            return Ok(());
        }
        let mut retired: Option<StepPluginSpec> = None;
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
                retired = Some(row.clone());
            }
        }
        drop(rows);
        if let Some(spec) = retired {
            self.record(crate::events::step_plugin_registry_event(
                crate::events::STEP_PLUGIN_RETIRED,
                actor,
                &spec,
            ));
        }
        Ok(())
    }

    async fn publish_declared(
        &self,
        mut spec: StepPluginSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        let key = (spec.kind.clone(), spec.version);
        if rows.contains_key(&key) {
            return Err(StepPluginError::Conflict(format!(
                "row already exists: {}@{}",
                spec.kind, spec.version
            )));
        }
        for ((k, _), row) in rows.iter_mut() {
            if *k == spec.kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        rows.insert(key, spec.clone());
        drop(rows);
        self.record(crate::events::step_plugin_registry_event(
            crate::events::STEP_PLUGIN_PUBLISHED,
            actor,
            &spec,
        ));
        Ok(spec)
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    pub struct PgStepPlugins {
        pool: PgPool,
    }

    impl PgStepPlugins {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        version: i32,
        status: String,
        label: String,
        description: Option<String>,
        category: String,
        metadata_schema: serde_json::Value,
        frontend_url: String,
        owning_team: String,
        authoring_job_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<StepPluginSpec, StepPluginError> {
        let status = r
            .status
            .parse::<WorkflowStatus>()
            .map_err(StepPluginError::Storage)?;
        Ok(StepPluginSpec {
            kind: r.kind,
            version: r.version,
            status,
            label: r.label,
            description: r.description,
            category: r.category,
            metadata_schema: r.metadata_schema,
            frontend_url: r.frontend_url,
            owning_team: r.owning_team,
            authoring_job_id: r.authoring_job_id,
            created_at: r.created_at,
        })
    }

    const SELECT: &str = "SELECT kind, version, status, label, description, category, \
                          metadata_schema, frontend_url, owning_team, authoring_job_id, created_at \
                          FROM step_plugins";

    #[async_trait]
    impl StepPluginRegistry for PgStepPlugins {
        async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 AND status = 'active'"))
                    .bind(kind)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StepPluginError::NotFound(format!("no active plugin: {kind}")))
        }

        async fn get_version(
            &self,
            kind: &str,
            version: i32,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 AND version = $2"))
                    .bind(kind)
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StepPluginError::NotFound(format!("{kind}@v{version}")))
        }

        async fn list_active(
            &self,
            category: Option<&str>,
        ) -> Result<Vec<StepPluginSpec>, StepPluginError> {
            let rows: Vec<Row> = match category {
                Some(c) => {
                    sqlx::query_as(&format!(
                        "{SELECT} WHERE status = 'active' AND category = $1 ORDER BY kind"
                    ))
                    .bind(c)
                    .fetch_all(&self.pool)
                    .await
                }
                None => {
                    sqlx::query_as(&format!("{SELECT} WHERE status = 'active' ORDER BY kind"))
                        .fetch_all(&self.pool)
                        .await
                }
            }
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError> {
            let rows: Vec<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 ORDER BY version"))
                    .bind(kind)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn create_draft(
            &self,
            mut spec: StepPluginSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM step_plugins WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            spec.version = max.0.map(|v| v + 1).unwrap_or(1);
            spec.status = WorkflowStatus::Draft;
            spec.created_at = now;

            sqlx::query(
                "INSERT INTO step_plugins
                    (kind, version, status, label, description, category,
                     metadata_schema, frontend_url, owning_team, authoring_job_id, created_at)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&spec.metadata_schema)
            .bind(&spec.frontend_url)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let event = crate::events::step_plugin_registry_event(
                crate::events::STEP_PLUGIN_DRAFT_SAVED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StepPluginError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(
            &self,
            kind: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            // Read the full draft row inside the tx — the published
            // event's payload is the promoted spec, and it must
            // describe the row THIS transaction flips (a post-commit
            // re-fetch could race a concurrent writer and record a
            // spec the flip never produced).
            let draft: Option<Row> = sqlx::query_as(&format!(
                "{SELECT} WHERE kind = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1"
            ))
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            let mut promoted = draft
                .map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StepPluginError::NotFound(format!("no draft to publish: {kind}")))?;

            sqlx::query(
                "UPDATE step_plugins SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            sqlx::query(
                "UPDATE step_plugins SET status = 'active'
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(promoted.version)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            promoted.status = WorkflowStatus::Active;

            let event = crate::events::step_plugin_registry_event(
                crate::events::STEP_PLUGIN_PUBLISHED,
                actor,
                &promoted,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StepPluginError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            Ok(promoted)
        }

        async fn retire(
            &self,
            kind: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<(), StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            // Read the active row first — the event payload is the
            // retired spec, and the idempotent no-op path (nothing
            // active) must record nothing. At most one row can be
            // active per kind (partial unique index).
            let active: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 AND status = 'active'"))
                    .bind(kind)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let Some(row) = active else {
                // Idempotent — nothing to do, so nothing to record.
                return Ok(());
            };
            let mut spec = row_to_spec(row)?;

            sqlx::query(
                "UPDATE step_plugins SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            spec.status = WorkflowStatus::Retired;

            let event = crate::events::step_plugin_registry_event(
                crate::events::STEP_PLUGIN_RETIRED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StepPluginError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            Ok(())
        }

        async fn publish_declared(
            &self,
            mut spec: StepPluginSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let exists: Option<(i32,)> =
                sqlx::query_as("SELECT version FROM step_plugins WHERE kind = $1 AND version = $2")
                    .bind(&spec.kind)
                    .bind(spec.version)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            if exists.is_some() {
                return Err(StepPluginError::Conflict(format!(
                    "row already exists: {}@{}",
                    spec.kind, spec.version
                )));
            }

            // RETIRE FIRST, THEN INSERT: `step_plugins_one_active_per_kind`
            // is a plain partial unique index, enforced per statement,
            // so the order is load-bearing (registry-bump-retires-first.sh
            // holds the migrations to the same order).
            sqlx::query(
                "UPDATE step_plugins SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(&spec.kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            spec.status = WorkflowStatus::Active;
            spec.created_at = now;
            sqlx::query(
                "INSERT INTO step_plugins
                    (kind, version, status, label, description, category,
                     metadata_schema, frontend_url, owning_team, authoring_job_id, created_at)
                 VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&spec.metadata_schema)
            .bind(&spec.frontend_url)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let event = crate::events::step_plugin_registry_event(
                crate::events::STEP_PLUGIN_PUBLISHED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StepPluginError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            Ok(spec)
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgStepPlugins;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: &str) -> StepPluginSpec {
        StepPluginSpec::draft(
            kind,
            format!("Test {kind}"),
            "qa",
            format!("{kind}.js"),
            serde_json::json!({}),
        )
    }

    /// Shared write-path actor: every registry write records an
    /// event, and the event needs a who. Tests are exempt from the
    /// no-wallclock lint, so `Utc::now()` rides along at call sites.
    fn test_actor() -> boss_core::actor::ActorId {
        boss_core::actor::ActorId::Human("emp-test".into())
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryStepPlugins::new();
        let v1 = reg
            .create_draft(sample("emerald-inspection"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, WorkflowStatus::Draft);
        let v2 = reg
            .create_draft(sample("emerald-inspection"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
    }

    #[tokio::test]
    async fn publish_retires_previous_active() {
        let reg = InMemoryStepPlugins::new();
        reg.create_draft(sample("kk"), &test_actor(), Utc::now())
            .await
            .unwrap();
        let active = reg.publish("kk", &test_actor(), Utc::now()).await.unwrap();
        assert_eq!(active.status, WorkflowStatus::Active);

        reg.create_draft(sample("kk"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("kk", &test_actor(), Utc::now()).await.unwrap();

        let v1 = reg.get_version("kk", 1).await.unwrap();
        assert_eq!(v1.status, WorkflowStatus::Retired);
        let cur = reg.get_active("kk").await.unwrap();
        assert_eq!(cur.version, 2);
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryStepPlugins::new();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_active_filters_by_category() {
        let reg = InMemoryStepPlugins::new();
        let mut q = sample("qa-plugin");
        q.category = "qa".into();
        q.status = WorkflowStatus::Active;
        reg.seed(q).unwrap();

        let mut s = sample("sales-plugin");
        s.category = "sales".into();
        s.status = WorkflowStatus::Active;
        reg.seed(s).unwrap();

        let qa_only = reg.list_active(Some("qa")).await.unwrap();
        assert_eq!(qa_only.len(), 1);
        assert_eq!(qa_only[0].kind, "qa-plugin");

        let all = reg.list_active(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    // -----------------------------------------------------------
    // Registry events — every write records an outbox event with
    // the row (protocol-policy-publish.md, Constraints). The
    // InMemory adapter collects what the Pg adapter records inside
    // the row transaction; `recorded_events()` is the test window.
    // -----------------------------------------------------------

    #[tokio::test]
    async fn publish_records_exactly_one_published_event() {
        let reg = InMemoryStepPlugins::new();
        let actor = test_actor();
        reg.create_draft(sample("emerald-inspection"), &actor, Utc::now())
            .await
            .unwrap();
        let published = reg
            .publish("emerald-inspection", &actor, Utc::now())
            .await
            .unwrap();

        let events: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STEP_PLUGIN_PUBLISHED)
            .collect();
        assert_eq!(
            events.len(),
            1,
            "publish must record exactly one jobs.step_plugin.published"
        );
        let payload = &events[0].payload;
        // The actor rides as `_actor` in EventStamp's exact shape —
        // a Human serializes as the bare employee id.
        assert_eq!(payload["_actor"], "emp-test");
        // The payload IS the promoted spec.
        assert_eq!(payload["kind"], "emerald-inspection");
        assert_eq!(payload["version"], published.version);
        assert_eq!(payload["status"], "active");
    }

    #[tokio::test]
    async fn create_draft_records_draft_saved() {
        let reg = InMemoryStepPlugins::new();
        let draft = reg
            .create_draft(sample("emerald-inspection"), &test_actor(), Utc::now())
            .await
            .unwrap();

        let events = reg.recorded_events();
        assert_eq!(events.len(), 1, "one write, one event");
        assert_eq!(events[0].kind, crate::events::STEP_PLUGIN_DRAFT_SAVED);
        assert_eq!(events[0].payload["kind"], "emerald-inspection");
        assert_eq!(events[0].payload["version"], draft.version);
        assert_eq!(events[0].payload["status"], "draft");
    }

    #[tokio::test]
    async fn retire_records_once_and_stays_silent_when_already_retired() {
        let reg = InMemoryStepPlugins::new();
        let actor = test_actor();
        reg.create_draft(sample("emerald-inspection"), &actor, Utc::now())
            .await
            .unwrap();
        reg.publish("emerald-inspection", &actor, Utc::now())
            .await
            .unwrap();

        reg.retire("emerald-inspection", &actor, Utc::now())
            .await
            .unwrap();
        // Second retire is the idempotent no-op path: rows_affected
        // is 0, so no event — the log records what happened, and
        // nothing happened (transactional-audit-log.md discipline).
        reg.retire("emerald-inspection", &actor, Utc::now())
            .await
            .unwrap();

        let retired: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STEP_PLUGIN_RETIRED)
            .collect();
        assert_eq!(
            retired.len(),
            1,
            "idempotent retire must not record a second event"
        );
        assert_eq!(retired[0].payload["kind"], "emerald-inspection");
        assert_eq!(retired[0].payload["status"], "retired");
    }
}
