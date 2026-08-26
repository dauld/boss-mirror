//! Rebuild the scheduling projections from `audit_log`.
//!
//! The handler emits a `scheduling.*` event for every write to the
//! four scheduling projections (tech_availability,
//! scheduled_assignments, tech_shift_patterns, tech_calendar_tokens)
//! (see `super::events`); this module replays them back into the
//! projection tables, so the projections survive a `boss-rebuild-all`.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::types::{ScheduledAssignment, TechAvailability, TechShiftPattern};

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("scheduling");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub availability_upserted: u64,
    pub availability_deleted: u64,
    pub assignments_upserted: u64,
    pub assignments_deleted: u64,
    pub assignment_status_changes: u64,
    pub shift_patterns_upserted: u64,
    pub calendar_tokens_rotated: u64,
}

pub async fn rebuild_scheduling(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // Wipe all four projections. Order: assignments first (no
    // children FK into it from this set), then the rest. CASCADE
    // not needed within this set — the FKs go upward to employees /
    // jobs which we leave untouched.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &[
            "DELETE FROM scheduled_assignments",
            "DELETE FROM tech_availability",
            "DELETE FROM tech_shift_patterns",
            "DELETE FROM tech_calendar_tokens",
        ],
        "kind LIKE 'scheduling.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "scheduling.availability.created" => {
                    let a: TechAvailability = match serde_json::from_value(ev.payload.clone()) {
                        Ok(a) => a,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed availability.created");
                            return Ok(Applied::Skipped);
                        }
                    };
                    replay_upsert_availability(&mut *conn, &a)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.availability_upserted += 1;
                    Ok(Applied::Yes)
                }
                "scheduling.availability.deleted" => {
                    if let Some(id) = parse_uuid(&ev.payload, "id") {
                        let n = sqlx::query("DELETE FROM tech_availability WHERE id = $1")
                            .bind(id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.availability_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "scheduling.assignment.created" => {
                    let a: ScheduledAssignment = match serde_json::from_value(ev.payload.clone()) {
                        Ok(a) => a,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed assignment.created");
                            return Ok(Applied::Skipped);
                        }
                    };
                    replay_upsert_assignment(&mut *conn, &a)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.assignments_upserted += 1;
                    Ok(Applied::Yes)
                }
                "scheduling.assignment.deleted" => {
                    if let Some(id) = parse_uuid(&ev.payload, "id") {
                        let n = sqlx::query("DELETE FROM scheduled_assignments WHERE id = $1")
                            .bind(id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.assignments_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "scheduling.assignment.status-changed" => {
                    let id = parse_uuid(&ev.payload, "id");
                    let status = ev
                        .payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let (Some(id), Some(status)) = (id, status) {
                        // The live write stamped updated_at with the
                        // clock value it also put in the payload as
                        // changed_at, so THAT is the reproducing
                        // value. ev.ts is the audit_log row time, which
                        // is a different instant — close enough to look
                        // right and wrong by ~100us, which is exactly
                        // the kind of drift a row-count assertion never
                        // sees. Falls back to ev.ts for events written
                        // before the payload carried it.
                        let changed_at = ev
                            .payload
                            .get("changed_at")
                            .and_then(|v| v.as_str())
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|d| d.with_timezone(&chrono::Utc))
                            .unwrap_or(ev.ts);
                        // updated_at COMES FROM THE EVENT, NOT THE CLOCK.
                        // This is a replay path: NOW() here stamps the
                        // moment of the REBUILD, so a projection rebuilt
                        // from the log did not match the one built live
                        // and the divergence grew with every replay.
                        // ReplayEvent::ts is the recorded event time and
                        // exists for exactly this (d7b8158e); the
                        // inventory and campaigns rebuilders already
                        // thread it the same way.
                        let n = sqlx::query(
                            "UPDATE scheduled_assignments SET status = $2, updated_at = $3 \
                             WHERE id = $1",
                        )
                        .bind(id)
                        .bind(&status)
                        .bind(changed_at)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?
                        .rows_affected();
                        if n > 0 {
                            report.assignment_status_changes += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "scheduling.shift-pattern.upserted" => {
                    let p: TechShiftPattern = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, error = %e, "skipping malformed shift-pattern.upserted");
                            return Ok(Applied::Skipped);
                        }
                    };
                    replay_upsert_shift_pattern(&mut *conn, &p)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.shift_patterns_upserted += 1;
                    Ok(Applied::Yes)
                }
                "scheduling.calendar-token.rotated" => {
                    let emp_id = ev
                        .payload
                        .get("employee_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let token = ev
                        .payload
                        .get("token")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let (Some(emp_id), Some(token)) = (emp_id, token) {
                        sqlx::query(
                            "INSERT INTO tech_calendar_tokens (employee_id, token, created_at) \
                             VALUES ($1, $2, NOW()) \
                             ON CONFLICT (employee_id) DO UPDATE SET \
                                token = EXCLUDED.token, created_at = EXCLUDED.created_at",
                        )
                        .bind(&emp_id)
                        .bind(&token)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                        report.calendar_tokens_rotated += 1;
                        Ok(Applied::Yes)
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown scheduling.* event kind");
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

fn parse_uuid(payload: &serde_json::Value, key: &str) -> Option<Uuid> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

async fn replay_upsert_availability(
    conn: &mut sqlx::PgConnection,
    a: &TechAvailability,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO tech_availability \
            (id, employee_id, kind, starts_at, ends_at, notes, source, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(a.id)
    .bind(&a.employee_id)
    .bind(a.kind.as_str())
    .bind(a.starts_at)
    .bind(a.ends_at)
    .bind(a.notes.as_deref())
    .bind(a.source.as_str())
    .bind(a.created_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

async fn replay_upsert_assignment(
    conn: &mut sqlx::PgConnection,
    a: &ScheduledAssignment,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO scheduled_assignments \
            (id, tech_id, target_job_id, kind, starts_at, ends_at, status, notes, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(a.id)
    .bind(&a.tech_id)
    .bind(a.target_job_id)
    .bind(a.kind.as_str())
    .bind(a.starts_at)
    .bind(a.ends_at)
    .bind(a.status.as_str())
    .bind(a.notes.as_deref())
    .bind(a.created_at)
    .bind(a.updated_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}

async fn replay_upsert_shift_pattern(
    conn: &mut sqlx::PgConnection,
    p: &TechShiftPattern,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO tech_shift_patterns \
            (id, employee_id, day_of_week, starts_at_time, ends_at_time, \
             timezone, effective_from, effective_to, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (employee_id, day_of_week, effective_from) DO UPDATE SET \
            starts_at_time = EXCLUDED.starts_at_time, \
            ends_at_time   = EXCLUDED.ends_at_time, \
            timezone       = EXCLUDED.timezone, \
            effective_to   = EXCLUDED.effective_to",
    )
    .bind(p.id)
    .bind(&p.employee_id)
    .bind(p.day_of_week)
    .bind(p.starts_at_time)
    .bind(p.ends_at_time)
    .bind(&p.timezone)
    .bind(p.effective_from)
    .bind(p.effective_to)
    .bind(p.created_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}
