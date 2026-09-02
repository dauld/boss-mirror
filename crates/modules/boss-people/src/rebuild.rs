//! Rebuild the `employees` + `employee_skills` +
//! `employee_certifications` projections from `audit_log`.
//!
//! Sixth projection rebuilder in the event-canonical arc (after
//! messages / jobs / inventory / accounts / shipping). See
//! `docs/design/projection-rebuilders.md`.
//!
//! State events consumed:
//! - `people.employee.created` / `.updated` — full `Employee`
//!   payload (parent row + `skills` list + `certifications` list).
//! - `people.employee.deleted` — `{id, deleted_at}`.
//! - `people.employee.change-recorded` — `EmployeeChangeRecord`,
//!   the audit-trail row in `employee_changes`. Replaying it keeps
//!   the change rows out of the workflow-status-update audit-chain
//!   bypass (they'd otherwise be wiped on rebuild via the employees
//!   CASCADE).
//!
//! `requisitions` — `people.requisition.opened` UPSERTs the row
//! (status changes ride on the same event kind via
//! ON CONFLICT DO UPDATE).
//!
//! Out of scope: `pto` is still an append-only fact log not yet
//! eventified — see TODO entry.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

use crate::postgres::{insert_employee_satellites, upsert_employee_row};
use crate::types::Employee;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("people");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub employees_upserted: u64,
    pub employees_deleted: u64,
    pub change_records_inserted: u64,
    pub requisitions_upserted: u64,
}

pub async fn rebuild_people(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // TRUNCATE … CASCADE so we wipe every table that FKs to
    // employees in one shot. Many out-of-scope tables reference
    // employees (payroll_run_lines, account_team_members,
    // account_notes.actor_id, requisitions.hiring_manager_id, …);
    // a plain DELETE would block on the first non-CASCADE FK.
    // CASCADE accepts that those out-of-scope tables get wiped too —
    // they'll be repopulated by their own rebuilders or stay empty
    // until they ship one. For a fresh-bootstrap rebuild path
    // (the primary use case) every dependent table starts empty
    // anyway, so the cascade is a no-op.
    // CASCADE wipes `requisitions` (FK to employees via
    // hiring_manager_id) too — the requisition.opened branch below
    // repopulates it from audit_log.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["TRUNCATE employees, employee_skills, employee_certifications, requisitions CASCADE"],
        "kind LIKE 'people.employee.%' OR kind LIKE 'people.requisition.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "people.employee.created" | "people.employee.updated" => {
                    let emp: Employee = match serde_json::from_value(ev.payload.clone()) {
                        Ok(e) => e,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping employee event with non-Employee payload \
                                 (likely pre-enrichment id-only)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_employee_row(&mut *conn, &emp, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    // Replace satellites wholesale per upsert event.
                    sqlx::query("DELETE FROM employee_skills WHERE employee_id = $1")
                        .bind(&emp.id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query("DELETE FROM employee_certifications WHERE employee_id = $1")
                        .bind(&emp.id)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    insert_employee_satellites(&mut *conn, &emp)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.employees_upserted += 1;
                    Ok(Applied::Yes)
                }
                "people.employee.change-recorded" => {
                    let rec: crate::workflows::EmployeeChangeRecord =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed change-recorded payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    sqlx::query(
                        "INSERT INTO employee_changes (employee_id, kind, from_value, to_value, effective_date, notes, initiated_by, created_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    )
                    .bind(&rec.employee_id)
                    .bind(&rec.kind)
                    .bind(&rec.from_value)
                    .bind(&rec.to_value)
                    .bind(rec.effective_date)
                    .bind(&rec.notes)
                    .bind(&rec.initiated_by)
                    .bind(ev.ts)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?;
                    report.change_records_inserted += 1;
                    Ok(Applied::Yes)
                }
                "people.requisition.opened" => {
                    let req: crate::requisitions::Requisition =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed requisition.opened payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    crate::requisitions::upsert_requisition(&mut *conn, &req, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.requisitions_upserted += 1;
                    Ok(Applied::Yes)
                }
                "people.employee.deleted" => {
                    let id: Option<String> =
                        ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    if let Some(id) = id {
                        let n = sqlx::query("DELETE FROM employees WHERE id = $1")
                            .bind(&id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.employees_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown people.* event kind");
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
