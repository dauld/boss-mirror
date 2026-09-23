//! Employee change endpoints — role/salary/department change tracking.
//!
//! Audit-chain note: `POST /api/people/changes` emits
//! `EMPLOYEE_CHANGE_RECORDED` so the row survives `rebuild_people`
//! (rather than being lost on rebuild via the employees CASCADE).
//!
//! Policy (backlog 8cdad84c, 2026-09-23): both verbs made no policy
//! call, and a change row can carry pay in its values. A read needs
//! `employee` Read and sees the rows that grant's scope covers, with
//! a pay-bearing row's values removed unless `compensation` Read covers
//! it too ([`crate::grants::ChangeReader`]); a write needs `employee`
//! Update, the same grant a PUT of the row needs.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_clock_client::ClockClient;
use boss_core::publisher::DomainPublisher;
use boss_policy::{Action, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::EMPLOYEE_CHANGE_RECORDED;
use crate::grants::ChangeReader;
use crate::workflows::EmployeeChangeRecord;

#[derive(Clone)]
pub struct EmployeeChangesState {
    pub pool: Arc<PgPool>,
    pub publisher: Option<DomainPublisher>,
    pub clock: Arc<dyn ClockClient>,
    /// Row-level authorization; `None` only in tests, where the gate
    /// allows and pay stays hidden ([`crate::grants`]).
    pub policy: Option<Arc<dyn PolicyClient>>,
}

pub fn employee_changes_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: Arc<dyn ClockClient>,
    policy: Option<Arc<dyn PolicyClient>>,
) -> Router {
    let state = EmployeeChangesState {
        pool: Arc::new(pool),
        publisher,
        clock,
        policy,
    };
    Router::new()
        .route("/api/people/changes", get(list_changes).post(create_change))
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EmployeeChange {
    pub employee_id: String,
    pub kind: String,
    pub from_value: Option<String>,
    pub to_value: String,
    pub effective_date: NaiveDate,
    pub notes: Option<String>,
    pub initiated_by: Option<String>,
}

/// One change row as the log is read: the request shape plus the
/// employee's department, which a `department:<d>` grant is judged on
/// and which is never sent. `to_value` is nullable here because the
/// column is — `POST /api/people/{id}/changes` may leave it empty.
#[derive(Serialize, sqlx::FromRow)]
struct ChangeRow {
    employee_id: String,
    kind: String,
    from_value: Option<String>,
    to_value: Option<String>,
    effective_date: NaiveDate,
    notes: Option<String>,
    initiated_by: Option<String>,
    #[serde(skip)]
    employee_department: Option<String>,
}

async fn list_changes(
    State(state): State<EmployeeChangesState>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reader = match ChangeReader::admit(state.policy.as_ref(), user).await {
        Ok(reader) => reader,
        Err(refused) => return refused,
    };
    let rows: Result<Vec<ChangeRow>, _> = sqlx::query_as(
        "SELECT c.employee_id, c.kind, c.from_value, c.to_value, c.effective_date, c.notes, \
                c.initiated_by, e.department AS employee_department \
         FROM employee_changes c LEFT JOIN employees e ON e.id = c.employee_id \
         ORDER BY c.effective_date DESC",
    )
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => reader.respond(&data, |row| {
            (
                &row.employee_id,
                row.employee_department.as_deref(),
                &row.kind,
            )
        }),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_change(
    State(state): State<EmployeeChangesState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<EmployeeChange>,
) -> Response {
    if let Err(refused) = crate::grants::require(
        state.policy.as_ref(),
        &user,
        Action::Update,
        Resource::employee(),
    )
    .await
    {
        return refused;
    }
    // The row's created_at binds the stamp's wall time (the rebuild
    // reproduces it from audit_log.timestamp); the change's business
    // date is `effective_date`, carried on the request.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let rec = EmployeeChangeRecord {
        employee_id: req.employee_id,
        kind: req.kind,
        from_value: req.from_value,
        to_value: Some(req.to_value),
        effective_date: req.effective_date,
        notes: req.notes,
        initiated_by: req.initiated_by,
    };

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let result = sqlx::query(
        "INSERT INTO employee_changes (employee_id, kind, from_value, to_value, effective_date, notes, initiated_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT DO NOTHING",
    )
    .bind(&rec.employee_id)
    .bind(&rec.kind)
    .bind(&rec.from_value)
    .bind(&rec.to_value)
    .bind(rec.effective_date)
    .bind(&rec.notes)
    .bind(&rec.initiated_by)
    .bind(stamp.timestamp)
    .execute(&mut *tx)
    .await;
    let inserted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // OUTBOX (phase 2): the event records with the row — and only
    // when the INSERT actually inserted. Before, the handler
    // published FIRST and wrote after: a failed or conflict-skipped
    // INSERT still shipped an event (an event with no fact), the
    // inverse of the swallowed-write class this arc closes.
    if inserted {
        let event = stamp.event(
            EMPLOYEE_CHANGE_RECORDED,
            serde_json::to_value(&rec).unwrap_or_default(),
        );
        if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }
    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

#[cfg(test)]
mod tests {
    //! The gate refuses BEFORE the handler touches the pool, so these
    //! run on a lazy pool that is never connected: a host under
    //! `.invalid` cannot resolve, so a gate that let a request through
    //! would fail here rather than read any database.
    use super::*;
    use boss_policy::Scope;
    use boss_policy_client::FakePolicyClient;
    use boss_testing::TestRequest;

    fn app(policy: Arc<dyn PolicyClient>) -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://never-connected.invalid/none")
            .expect("a lazy pool does not connect");
        employee_changes_router(
            pool,
            None,
            Arc::new(boss_clock_client::WallClockClient),
            Some(policy),
        )
    }

    fn change() -> serde_json::Value {
        serde_json::json!({
            "employee_id": "emp-002",
            "kind": "promotion",
            "from_value": "8500000",
            "to_value": "9500000",
            "effective_date": "2026-09-23",
        })
    }

    /// Backlog 8cdad84c: the change log answered every caller, and a
    /// promotion row carries pay. No `employee` Read, no rows.
    #[tokio::test]
    async fn the_change_log_refuses_a_reader_without_an_employee_read_grant() {
        TestRequest::get("/api/people/changes")
            .as_user("emp-002", "service-tech")
            .send(&app(Arc::new(FakePolicyClient::deny_all())))
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    /// Recording a change is editing the employee: it needs the Update
    /// grant a PUT of the row needs, and Read alone is not it.
    #[tokio::test]
    async fn recording_a_change_needs_employee_update_not_read() {
        let read_only: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("hr", Action::Read, Resource::employee(), Scope::All)
                .build(),
        );
        TestRequest::post("/api/people/changes")
            .as_user("emp-hr", "hr")
            .json(&change())
            .send(&app(read_only))
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
}
