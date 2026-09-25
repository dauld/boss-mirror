//! WHAT A DEPARTMENT IS — the `departments` table, read.
//!
//! A department is a Subject kind with identity, and its FUNCTION
//! (operations / revenue / support / governance) is Class data on that
//! kind: migration `20260919181324-a-department-is-a-subject.sql`
//! (backlog 5987302f, design 32f18167). A row's `id` IS the code a
//! packet carries in `metadata.department` and the catalog spells as a
//! route's `app`, which is what makes the correspondence checkable at
//! all.
//!
//! WHY THIS PORT EXISTS, AND WHAT IT REPLACED. Until this module,
//! `GET /api/departments` served the active `employee` Classes whose
//! `member_attribute` is `department` — the EMPLOYEE DRAWER, which is
//! the set of values an employee's `department` column may take, not
//! the set of departments the company has. MEASURED 2026-09-19
//! (backlog 80a77466), the live endpoint served nine codes —
//! engineering, finance, hosting, it, marketing, operations, product,
//! sales, support — against the thirteen the roster holds: the overlap
//! was five. Four served codes have no route and no packet
//! (engineering, hosting, operations, product) and eight real
//! departments were missing (distribution, executive, maintenance,
//! people, production, qa, service, warehouse).
//!
//! That mattered on a DATE. `infra/dispatcher/rules/
//! department-retros-weekly.toml` reads this endpoint AT FIRE TIME and
//! opens one `department-retro` per row it answers, and its schedule is
//! anchored on Monday 2026-09-21. Read from the drawer, the first
//! firing would have opened retros for four departments that do not
//! exist as surfaces and none for eight that do — and a wrong retro
//! roster is much harder to unpick after the packets exist than before.
//!
//! The drawer is not wrong, it is a different question, and it is not
//! touched here (splitting it is backlog a45ab09d). What changed is
//! which question this endpoint asks.
//!
//! Port + a Pg adapter + an in-memory double, the `agents` shape. The
//! roster was schema until backlog 7edf0e97 (2026-09-25) gave it a
//! write — [`DepartmentRegistry::publish`], the tenant batch behind
//! `POST /api/departments/batch` — so a tenant declares its roster and
//! publishes it (`crate::department::declare` holds the declaration's
//! shape and the rule).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publish::{FieldChange, PublishMode};
use boss_core::publisher::EventStamp;
use serde::Serialize;

use super::declare::{DepartmentInput, DepartmentRow, DepartmentsBatchOutcome, RowPlan, plan_row};

/// The fact a declaration leaves for each department row it INSERTED:
/// the declaration as inserted plus `declared_by`, the actor the
/// request signed with. Never per batch and never for a kept row — the
/// `class.declared` / `agent.declared` rule (backlog d9409039).
pub const DEPARTMENT_DECLARED: &str = "department.declared";

/// The fact a TAKE leaves for each held row it changed: the row as it
/// now reads, the `changes` (field, from, to) and `updated_by`. A
/// retirement is one of these — `retired false → true` — so when a
/// department was withdrawn, and by whom, is read from the log.
pub const DEPARTMENT_UPDATED: &str = "department.updated";

/// `payload` with the stamp's actor under `key` — read from the stamp
/// so it is the same value `_actor` carries.
fn with_actor(
    payload: serde_json::Value,
    key: &str,
    stamp: &EventStamp,
) -> Result<serde_json::Value, DepartmentsError> {
    match payload {
        serde_json::Value::Object(mut map) => {
            map.insert(
                key.to_string(),
                serde_json::Value::String(stamp.actor().to_string()),
            );
            Ok(serde_json::Value::Object(map))
        }
        other => Err(DepartmentsError::Storage(format!(
            "a department fact's payload is not an object: {other}"
        ))),
    }
}

/// `department.declared` for one inserted row. One builder for both
/// adapters, so the double records exactly what Pg stages.
pub fn declared_event(
    stamp: &EventStamp,
    row: &DepartmentInput,
) -> Result<Event, DepartmentsError> {
    let payload =
        serde_json::to_value(row).map_err(|e| DepartmentsError::Storage(e.to_string()))?;
    Ok(stamp.event(
        DEPARTMENT_DECLARED,
        with_actor(payload, "declared_by", stamp)?,
    ))
}

/// `department.updated` for one row a take changed.
pub fn updated_event(
    stamp: &EventStamp,
    after: &DepartmentInput,
    changes: &[FieldChange],
) -> Result<Event, DepartmentsError> {
    let mut payload = serde_json::to_value(DepartmentRow::from(after))
        .map_err(|e| DepartmentsError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "changes".to_string(),
            serde_json::to_value(changes).map_err(|e| DepartmentsError::Storage(e.to_string()))?,
        );
    }
    Ok(stamp.event(
        DEPARTMENT_UPDATED,
        with_actor(payload, "updated_by", stamp)?,
    ))
}

/// One department as the registry declares it. `code` and
/// `display_name` are the names the wire has always used (the
/// `retro.open` handler reads exactly those two), so the roster could
/// change underneath without a consumer changing; `function` is the
/// taxonomy the kind carries and is new with the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Department {
    pub code: String,
    pub display_name: String,
    pub function: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DepartmentsError {
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait DepartmentRegistry: Send + Sync {
    /// Every department the company has, in the registry's own
    /// `sort_order` then by code. A retired row is not a department.
    /// `Err` is the registry not answering — a different fact from "no
    /// departments", and answered as one, because an empty roster
    /// would quietly cancel every retro rather than fail.
    async fn list(&self) -> Result<Vec<Department>, DepartmentsError>;

    /// Land a tenant's declarations in one transaction, row by row as
    /// [`plan_row`] decides: an absent code is inserted (with its
    /// `department` Subject identity row, so a Job may be about it) and
    /// records [`DEPARTMENT_DECLARED`]; a held row that differs is KEPT
    /// under the default and named, and under [`PublishMode::Take`]
    /// UPDATED on every declared field, recording
    /// [`DEPARTMENT_UPDATED`]. Rows arrive validated
    /// (`declare::validate_batch`); the Class check on `function` is
    /// the door's. A code the tenant does not declare is never touched.
    async fn publish(
        &self,
        rows: &[DepartmentInput],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<DepartmentsBatchOutcome, DepartmentsError>;
}

/// The port-level double: the Pg adapter's rules over a map, every
/// fact recorded in order — what a Pg deployment finds on the outbox.
#[derive(Default)]
pub struct InMemoryDepartments {
    rows: Mutex<BTreeMap<String, DepartmentRow>>,
    events: Mutex<Vec<Event>>,
}

impl InMemoryDepartments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `row` as though a migration had seeded it.
    pub fn with(self, row: &DepartmentInput) -> Self {
        if let Ok(mut rows) = self.rows.lock() {
            rows.insert(row.code.clone(), DepartmentRow::from(row));
        }
        self
    }

    /// Every fact recorded through this adapter, in order.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

fn poisoned<T>(_: T) -> DepartmentsError {
    DepartmentsError::Storage("in-memory departments lock poisoned".into())
}

#[async_trait]
impl DepartmentRegistry for InMemoryDepartments {
    async fn list(&self) -> Result<Vec<Department>, DepartmentsError> {
        let rows = self.rows.lock().map_err(poisoned)?;
        let mut live: Vec<&DepartmentRow> = rows.values().filter(|r| !r.retired).collect();
        live.sort_by(|a, b| (a.sort_order, &a.code).cmp(&(b.sort_order, &b.code)));
        Ok(live
            .into_iter()
            .map(|r| Department {
                code: r.code.clone(),
                display_name: r.display_name.clone(),
                function: r.function.clone(),
            })
            .collect())
    }

    async fn publish(
        &self,
        declared: &[DepartmentInput],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<DepartmentsBatchOutcome, DepartmentsError> {
        let mut rows = self.rows.lock().map_err(poisoned)?;
        let mut staged = Vec::new();
        let mut out = DepartmentsBatchOutcome {
            received: declared.len(),
            ..Default::default()
        };
        for d in declared {
            let plan = plan_row(rows.get(&d.code), d, mode);
            match &plan {
                RowPlan::Insert => staged.push(declared_event(stamp, d)?),
                RowPlan::Take(changes) => staged.push(updated_event(stamp, d, changes)?),
                RowPlan::Keep(_) | RowPlan::Unchanged => {}
            }
            if matches!(plan, RowPlan::Insert | RowPlan::Take(_)) {
                rows.insert(d.code.clone(), DepartmentRow::from(d));
            }
            out.record(&d.code, &plan);
        }
        self.events.lock().map_err(poisoned)?.extend(staged);
        Ok(out)
    }
}

#[cfg(feature = "postgres")]
mod postgres {
    use async_trait::async_trait;
    use boss_core::publish::PublishMode;
    use boss_core::publisher::EventStamp;
    use sqlx::{PgPool, Row};

    use super::super::declare::{
        DepartmentInput, DepartmentRow, DepartmentsBatchOutcome, RowPlan, plan_row,
    };
    use super::{Department, DepartmentRegistry, DepartmentsError, declared_event, updated_event};

    pub struct PgDepartments {
        pool: PgPool,
    }

    impl PgDepartments {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    fn storage(e: sqlx::Error) -> DepartmentsError {
        DepartmentsError::Storage(e.to_string())
    }

    #[async_trait]
    impl DepartmentRegistry for PgDepartments {
        async fn list(&self) -> Result<Vec<Department>, DepartmentsError> {
            let rows = sqlx::query(
                "SELECT id, label, function FROM departments \
                 WHERE retired_at IS NULL ORDER BY sort_order, id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter()
                .map(|r| {
                    Ok(Department {
                        code: r.try_get("id").map_err(storage)?,
                        display_name: r.try_get("label").map_err(storage)?,
                        function: r.try_get("function").map_err(storage)?,
                    })
                })
                .collect()
        }

        async fn publish(
            &self,
            declared: &[DepartmentInput],
            mode: PublishMode,
            stamp: &EventStamp,
        ) -> Result<DepartmentsBatchOutcome, DepartmentsError> {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let mut out = DepartmentsBatchOutcome {
                received: declared.len(),
                ..Default::default()
            };
            for d in declared {
                // What the registry holds, read inside the transaction
                // and locked, so the plan and the write see one row.
                let held = sqlx::query(
                    "SELECT id, label, function, sort_order, retired_at IS NOT NULL AS retired \
                     FROM departments WHERE id = $1 FOR UPDATE",
                )
                .bind(&d.code)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?
                .map(|r| -> Result<DepartmentRow, DepartmentsError> {
                    Ok(DepartmentRow {
                        code: r.try_get("id").map_err(storage)?,
                        display_name: r.try_get("label").map_err(storage)?,
                        function: r.try_get("function").map_err(storage)?,
                        sort_order: r.try_get("sort_order").map_err(storage)?,
                        retired: r.try_get("retired").map_err(storage)?,
                    })
                })
                .transpose()?;
                let mut plan = plan_row(held.as_ref(), d, mode);
                match &plan {
                    RowPlan::Insert => {
                        let landed = sqlx::query(
                            "INSERT INTO departments (id, label, function, sort_order, retired_at) \
                             VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN $6::timestamptz END) \
                             ON CONFLICT (id) DO NOTHING",
                        )
                        .bind(&d.code)
                        .bind(&d.display_name)
                        .bind(&d.function)
                        .bind(d.sort_order)
                        .bind(d.retired)
                        // The fact's own instant, so the row's stamp
                        // and the event that records it read the same
                        // time (and no wall clock is read in SQL).
                        .bind(stamp.timestamp)
                        .execute(&mut *tx)
                        .await
                        .map_err(storage)?
                        .rows_affected();
                        if landed == 1 {
                            // The identity row the migration gave every
                            // seeded department: without it a Job about
                            // this department fails the existence gate.
                            sqlx::query(
                                "INSERT INTO subjects (kind, id, label) \
                                 VALUES ('department', $1, $2) ON CONFLICT (kind, id) DO NOTHING",
                            )
                            .bind(&d.code)
                            .bind(&d.display_name)
                            .execute(&mut *tx)
                            .await
                            .map_err(storage)?;
                            boss_events::outbox::record_event_in_tx(
                                &mut tx,
                                &declared_event(stamp, d)?,
                            )
                            .await
                            .map_err(DepartmentsError::Storage)?;
                        } else {
                            // A concurrent insert won between the read
                            // and the write: nothing of ours landed, so
                            // nothing is recorded, and the row is named
                            // rather than counted as ours.
                            plan = RowPlan::Keep(vec!["inserted concurrently".into()]);
                        }
                    }
                    RowPlan::Take(changes) => {
                        // The retirement stamp is set once and kept: a
                        // repeat take of `retired = true` plans as
                        // Unchanged and never reaches here, and an
                        // un-retire clears it.
                        sqlx::query(
                            "UPDATE departments SET label = $2, function = $3, sort_order = $4, \
                             retired_at = CASE WHEN $5 THEN COALESCE(retired_at, $6::timestamptz) END \
                             WHERE id = $1",
                        )
                        .bind(&d.code)
                        .bind(&d.display_name)
                        .bind(&d.function)
                        .bind(d.sort_order)
                        .bind(d.retired)
                        .bind(stamp.timestamp)
                        .execute(&mut *tx)
                        .await
                        .map_err(storage)?;
                        boss_events::outbox::record_event_in_tx(
                            &mut tx,
                            &updated_event(stamp, d, changes)?,
                        )
                        .await
                        .map_err(DepartmentsError::Storage)?;
                    }
                    RowPlan::Keep(_) | RowPlan::Unchanged => {}
                }
                out.record(&d.code, &plan);
            }
            tx.commit().await.map_err(storage)?;
            Ok(out)
        }
    }
}

#[cfg(feature = "postgres")]
pub use postgres::PgDepartments;
