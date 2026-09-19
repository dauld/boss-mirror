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
//! Port + a Pg adapter, the `sensors` shape. There is no write door:
//! the roster is schema today, the way `144-estate-subjects.sql`'s is,
//! and becomes tenant-published data when departments do.

use async_trait::async_trait;
use serde::Serialize;

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
}

#[cfg(feature = "postgres")]
mod postgres {
    use async_trait::async_trait;
    use sqlx::{PgPool, Row};

    use super::{Department, DepartmentRegistry, DepartmentsError};

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
    }
}

#[cfg(feature = "postgres")]
pub use postgres::PgDepartments;
