//! Rebuild the `jobs` + `steps` projections from `audit_log`.
//!
//! Second projection rebuilder in the event-canonical arc (after
//! `boss-messages`). See `docs/design/projection-rebuilders.md`.
//!
//! Event topology — only the **state events** drive the rebuild;
//! the **marker events** (`status_changed`, `closed`, `completed`,
//! `signed_off`) are informational duplicates and are skipped.
//!
//! State events:
//! - `jobs.job.created`  — full Job row → INSERT
//! - `jobs.job.updated`  — full Job row → UPSERT (UPDATE if exists,
//!   INSERT if not — tolerates missing CREATE in pre-enrichment
//!   audit slices)
//! - `jobs.step.created` — full Step row → INSERT
//! - `jobs.step.updated` — full Step row → UPSERT
//!
//! Schema columns `created_at` / `updated_at` get filled from the
//! audit_log row's own `timestamp` field — the event-time recorded
//! by `DomainPublisher.emit`. Same shape as Layer 2 of the
//! immutable-audit-log: events are the canonical clock, projections
//! follow.

use boss_core::job::{Job, Step};
use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::warn;

use crate::postgres::{
    blocked_by_uuids, job_status_str, priority_str, step_status_str, subject_parts,
};

/// Advisory-lock key for the jobs/steps rebuilder, derived from the
/// projection name so it is distinct from every other rebuilder's key.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("jobs");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub jobs_inserted: u64,
    pub jobs_updated: u64,
    pub steps_inserted: u64,
    pub steps_updated: u64,
}

/// Drop every row in `steps` and `jobs` and replay every
/// `jobs.job.*` / `jobs.step.*` event from `audit_log` in id order.
/// Wrapped in a single transaction holding an advisory lock for the
/// duration — concurrent writes block briefly.
pub async fn rebuild_jobs_and_steps(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // Steps cascade on jobs deletion (FK ON DELETE CASCADE), but we
    // delete steps first to make the order explicit and to make the
    // rebuild work even if the cascade is ever loosened.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM steps", "DELETE FROM jobs"],
        "kind LIKE 'jobs.job.%' OR kind LIKE 'jobs.step.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "jobs.job.created" | "jobs.job.updated" => {
                    let job: Job = match serde_json::from_value(ev.payload.clone()) {
                        Ok(j) => j,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping event with payload that doesn't deserialize as a Job (likely a pre-enrichment marker)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    // `_simulated` on the create event is where a
                    // Job's origin lives in the log. Read it here so a
                    // replay reproduces the same flag the live write
                    // set — otherwise a rebuild would quietly turn the
                    // whole simulated company real.
                    let simulated = ev
                        .payload
                        .get("_simulated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let inserted_now = upsert_job(&mut *conn, &job, ev.ts, simulated)
                        .await
                        .map_err(|e| e.to_string())?;
                    if inserted_now {
                        report.jobs_inserted += 1;
                    } else {
                        report.jobs_updated += 1;
                    }
                    Ok(Applied::Yes)
                }
                "jobs.step.created" | "jobs.step.updated" => {
                    let step: Step = match serde_json::from_value(ev.payload.clone()) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping event with payload that doesn't deserialize as a Step"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    let inserted_now = upsert_step(&mut *conn, &step, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    if inserted_now {
                        report.steps_inserted += 1;
                    } else {
                        report.steps_updated += 1;
                    }
                    Ok(Applied::Yes)
                }
                // Marker events — the sibling state event already carried
                // full row state. Counted as skipped; not anomalous.
                "jobs.job.status_changed"
                | "jobs.job.closed"
                | "jobs.step.completed"
                | "jobs.step.signed_off" => Ok(Applied::Skipped),
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown jobs.* event kind; skipping");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

/// Upsert a Job row, stamping `created_at` (only on insert) and
/// `updated_at` from the audit_log event timestamp. Returns
/// `true` if the row was inserted (new), `false` if updated.
/// `simulated` is written on INSERT and deliberately absent from the
/// DO UPDATE list: a Job's origin is decided when it is created and
/// never revisited, so a later `jobs.job.updated` must not be able to
/// move it. The storage enforces the immutability rather than trusting
/// every caller to.
async fn upsert_job(
    conn: &mut sqlx::PgConnection,
    job: &Job,
    ts: DateTime<Utc>,
    simulated: bool,
) -> Result<bool, RebuildError> {
    let (subj_kind, subj_ref) = subject_parts(&job.subject);
    let result = sqlx::query(
        r#"
        INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id,
                          status, priority, opened_on, due_on, closed_on, metadata, tags,
                          workflow_version, created_at, updated_at, simulated)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15, $16)
        ON CONFLICT (id) DO UPDATE SET
            kind = EXCLUDED.kind,
            workflow_version = EXCLUDED.workflow_version,
            subject_kind = EXCLUDED.subject_kind,
            subject_id = EXCLUDED.subject_id,
            title = EXCLUDED.title,
            owner_id = EXCLUDED.owner_id,
            status = EXCLUDED.status,
            priority = EXCLUDED.priority,
            opened_on = EXCLUDED.opened_on,
            due_on = EXCLUDED.due_on,
            closed_on = EXCLUDED.closed_on,
            metadata = EXCLUDED.metadata,
            tags = EXCLUDED.tags,
            updated_at = EXCLUDED.updated_at
        RETURNING (xmax = 0) AS inserted
        "#,
    )
    .bind(*job.id.inner().as_uuid())
    .bind(&job.kind)
    .bind(subj_kind)
    .bind(subj_ref)
    .bind(&job.title)
    .bind(&job.owner_id)
    .bind(job_status_str(job.status))
    .bind(priority_str(job.priority))
    .bind(job.opened_on)
    .bind(job.due_on)
    .bind(job.closed_on)
    .bind(&job.metadata)
    .bind(&job.tags)
    .bind(job.workflow_version)
    .bind(ts)
    .bind(simulated)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    use sqlx::Row;
    Ok(result.get::<bool, _>("inserted"))
}

/// Upsert a Step row. Same timestamp-stamping shape as `upsert_job`.
async fn upsert_step(
    conn: &mut sqlx::PgConnection,
    step: &Step,
    ts: DateTime<Utc>,
) -> Result<bool, RebuildError> {
    let result = sqlx::query(
        r#"
        INSERT INTO steps (id, job_id, kind, title, spec_slug, assignee_id, status, sort_order,
                           blocked_by, sign_offs_required, assurance_required, sign_offs, fields,
                           completed_on, metadata, notes, step_plugin_version,
                           embedded_job, created_at, updated_at, became_ready_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $19,
                CASE WHEN $7 = 'ready' THEN $19 END)
        ON CONFLICT (id) DO UPDATE SET
            job_id = EXCLUDED.job_id,
            kind = EXCLUDED.kind,
            title = EXCLUDED.title,
            spec_slug = EXCLUDED.spec_slug,
            assignee_id = EXCLUDED.assignee_id,
            status = EXCLUDED.status,
            sort_order = EXCLUDED.sort_order,
            blocked_by = EXCLUDED.blocked_by,
            sign_offs_required = EXCLUDED.sign_offs_required,
            assurance_required = EXCLUDED.assurance_required,
            sign_offs = EXCLUDED.sign_offs,
            fields = EXCLUDED.fields,
            completed_on = EXCLUDED.completed_on,
            metadata = EXCLUDED.metadata,
            notes = EXCLUDED.notes,
            step_plugin_version = EXCLUDED.step_plugin_version,
            embedded_job = EXCLUDED.embedded_job,
            updated_at = EXCLUDED.updated_at,
            -- First event that shows the row in `ready` wins, exactly
            -- as the live UPDATE's COALESCE writes the stamp once at
            -- the flip — replay reproduces `became_ready_at` from the
            -- event-time the log carried all along (2a0b034e). Rows
            -- that PREDATE the column even get it backfilled here,
            -- which is the rebuilder doing its one job: reproducing
            -- truth from the log.
            became_ready_at = COALESCE(steps.became_ready_at, EXCLUDED.became_ready_at)
        RETURNING (xmax = 0) AS inserted
        "#,
    )
    .bind(*step.id.inner().as_uuid())
    .bind(*step.job_id.inner().as_uuid())
    .bind(&step.kind)
    .bind(&step.title)
    .bind(&step.spec_slug)
    .bind(&step.assignee_id)
    .bind(step_status_str(step.status))
    .bind(step.sort_order)
    .bind(blocked_by_uuids(&step.blocked_by))
    .bind(serde_json::to_value(&step.sign_offs_required).unwrap_or_default())
    // Same TEXT encoding as the live save path — the omission of
    // this column silently downgraded replayed steps to session
    // assurance (packet d7b8158e).
    .bind(
        step.assurance_required
            .and_then(|a| serde_json::to_value(a).ok())
            .and_then(|v| v.as_str().map(str::to_string)),
    )
    .bind(serde_json::to_value(&step.sign_offs).unwrap_or_default())
    .bind(serde_json::to_value(&step.fields).unwrap_or_default())
    .bind(step.completed_on)
    .bind(&step.metadata)
    .bind(&step.notes)
    .bind(step.step_plugin_version)
    .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
    .bind(ts)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    use sqlx::Row;
    Ok(result.get::<bool, _>("inserted"))
}
