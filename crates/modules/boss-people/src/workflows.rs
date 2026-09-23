//! HR workflow endpoints — onboarding, offboarding, and employee changes.
//!
//! Audit-chain note: write paths route through `DomainPublisher`
//! so every status flip + change-log row lands in `audit_log`.
//! `rebuild_people` consumes both `people.employee.updated` (full
//! Employee row state) and `people.employee.change-recorded` (the
//! per-flip audit-trail row), so a `boss-rebuild-all` cycle
//! reproduces both the projection AND the change history.
//!
//! Both the status flip and the `employee_changes` row are emitted as
//! audit_log events, not written straight to the projection — a raw
//! `UPDATE employees SET status` + `INSERT INTO employee_changes`
//! would be silently wiped on the next rebuild (CASCADE through the FK
//! to `employees`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use boss_core::publisher::DomainPublisher;
use boss_policy::{Action, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::EMPLOYEE_CHANGE_RECORDED;
use crate::grants::ChangeReader;
use crate::port::PeopleRepository;

#[derive(Clone)]
pub struct WorkflowState {
    pub pool: Arc<PgPool>,
    /// Canonical Employee writer. Used to mutate the projection row
    /// in lockstep with the audit_log emit so both stay in sync.
    pub people: Arc<dyn PeopleRepository>,
    /// Audit_log + NATS bus. `None` is allowed for tests that don't
    /// exercise the audit-chain path (the handler then writes to the
    /// projection only).
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    /// Gates the per-employee change log the way
    /// `/api/people/changes` is gated (backlog 8cdad84c). `None` only
    /// in tests: the gate allows and pay stays hidden.
    pub policy: Option<Arc<dyn PolicyClient>>,
}

pub fn workflow_router(
    pool: PgPool,
    people: Arc<dyn PeopleRepository>,
    publisher: Option<DomainPublisher>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
    policy: Option<Arc<dyn PolicyClient>>,
) -> Router {
    let state = WorkflowState {
        pool: Arc::new(pool),
        people,
        publisher,
        clock,
        policy,
    };
    Router::new()
        .route("/api/people/{id}/status", put(update_status))
        .route("/api/people/{id}/changes", get(list_changes))
        .route("/api/people/{id}/changes", post(record_change))
        .route("/api/people/{id}/onboard", post(start_onboarding))
        .route("/api/people/{id}/offboard", post(start_offboarding))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpdateStatus {
    status: String,
    notes: Option<String>,
    initiated_by: Option<String>,
}

#[derive(Deserialize)]
struct RecordChange {
    kind: String,
    from_value: Option<String>,
    to_value: Option<String>,
    effective_date: NaiveDate,
    notes: Option<String>,
    initiated_by: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct EmployeeChange {
    id: i64,
    employee_id: String,
    kind: String,
    from_value: Option<String>,
    to_value: Option<String>,
    effective_date: NaiveDate,
    notes: Option<String>,
    initiated_by: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    /// What a `department:<d>` grant is judged on; never sent.
    #[serde(skip)]
    employee_department: Option<String>,
}

/// Audit-trail event payload for `EMPLOYEE_CHANGE_RECORDED`. The
/// rebuilder deserialises this directly to repopulate the
/// `employee_changes` projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeChangeRecord {
    pub employee_id: String,
    pub kind: String,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub effective_date: NaiveDate,
    pub notes: Option<String>,
    pub initiated_by: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn update_status(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(body): Json<UpdateStatus>,
) -> Response {
    // Read the canonical Employee — we need its full state to emit
    // the `EMPLOYEE_UPDATED` event with a complete row payload (the
    // rebuilder requires it for upsert).
    let mut emp = match state.people.employee_by_id(&id).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "employee not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // `status` is a free String validated against the Class registry
    // at write time. The handler just records the transition; the
    // Postgres adapter rejects unknown values via
    // validate_employee_class("status", code).
    let from_status = emp.status.clone();
    let new_status = body.status.clone();
    emp.status = Some(new_status);

    let now = boss_clock_client::now_from(&state.clock).await;
    // OUTBOX (phase 2): the adapter records people.employee.updated
    // in the update transaction. Row-touch columns bind the stamp's
    // wall time; `effective_date` (the business day of the change)
    // keeps the authoritative clock's date.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = state
        .people
        .update_employee_at(&id, &emp, stamp.timestamp, &stamp)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let change_kind = derive_change_kind(from_status.as_deref().unwrap_or(""), &body.status);

    if let Err(e) = record_change_inner(
        &state,
        EmployeeChangeRecord {
            employee_id: id.clone(),
            kind: change_kind.to_string(),
            from_value: from_status,
            to_value: Some(body.status.clone()),
            effective_date: now.date_naive(),
            notes: body.notes,
            initiated_by: body.initiated_by,
        },
    )
    .await
    {
        return e;
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

async fn list_changes(
    State(state): State<WorkflowState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    // One employee's history is the same log `/api/people/changes`
    // serves, read the same way (backlog 8cdad84c).
    let reader = match ChangeReader::admit(state.policy.as_ref(), user).await {
        Ok(reader) => reader,
        Err(refused) => return refused,
    };
    let rows: Result<Vec<EmployeeChange>, _> = sqlx::query_as(
        "SELECT c.id, c.employee_id, c.kind, c.from_value, c.to_value, c.effective_date, \
                c.notes, c.initiated_by, c.created_at, e.department AS employee_department \
         FROM employee_changes c LEFT JOIN employees e ON e.id = c.employee_id \
         WHERE c.employee_id = $1 ORDER BY c.created_at DESC",
    )
    .bind(&id)
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(changes) => reader.respond(&changes, |row| {
            (
                &row.employee_id,
                row.employee_department.as_deref(),
                &row.kind,
            )
        }),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn record_change(
    State(state): State<WorkflowState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<RecordChange>,
) -> Response {
    // Recording a change is editing the employee: the Update grant a
    // PUT of the row needs (backlog 8cdad84c).
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
    if let Err(e) = record_change_inner(
        &state,
        EmployeeChangeRecord {
            employee_id: id,
            kind: body.kind,
            from_value: body.from_value,
            to_value: body.to_value,
            effective_date: body.effective_date,
            notes: body.notes,
            initiated_by: body.initiated_by,
        },
    )
    .await
    {
        return e;
    }
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn start_onboarding(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
) -> Response {
    update_employee_status(
        &state,
        &id,
        "active",
        "onboard",
        boss_clock_client::now_from(&state.clock).await,
    )
    .await
}

async fn start_offboarding(
    State(state): State<WorkflowState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
) -> Response {
    update_employee_status(
        &state,
        &id,
        "terminated",
        "offboard",
        boss_clock_client::now_from(&state.clock).await,
    )
    .await
}

async fn update_employee_status(
    state: &WorkflowState,
    employee_id: &str,
    new_status: &str,
    change_kind: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Response {
    let mut emp = match state.people.employee_by_id(employee_id).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "employee not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Registry validates at write time (see above).
    let from_status = emp.status.clone();
    emp.status = Some(new_status.to_string());

    // OUTBOX (phase 2): the adapter records people.employee.updated
    // in the update transaction. Row-touch columns bind the stamp's
    // wall time; `effective_date` keeps the caller's clock date.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = state
        .people
        .update_employee_at(employee_id, &emp, stamp.timestamp, &stamp)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Err(resp) = record_change_inner(
        state,
        EmployeeChangeRecord {
            employee_id: employee_id.to_string(),
            kind: change_kind.to_string(),
            from_value: from_status,
            to_value: Some(new_status.to_string()),
            effective_date: now.date_naive(),
            notes: None,
            initiated_by: None,
        },
    )
    .await
    {
        return resp;
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

/// Single canonical write path for `employee_changes`. OUTBOX
/// (phase 2): the projection row and its event record in ONE
/// transaction. Before, this emitted the event FIRST and wrote
/// after — a failed INSERT still shipped an event (an event with
/// no fact), the inverse of the swallowed-write class this arc
/// closes. Returns the response only on failure (success
/// continues — caller may chain further work).
async fn record_change_inner(
    state: &WorkflowState,
    rec: EmployeeChangeRecord,
) -> Result<(), Response> {
    // Stamp first: the row's created_at binds the stamp's wall time,
    // which is also the event's record stamp — the rebuild reproduces
    // the column from audit_log.timestamp.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?;
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
    .bind(stamp.timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response())?;
    let event = stamp.event(
        EMPLOYEE_CHANGE_RECORDED,
        serde_json::to_value(&rec).unwrap_or_default(),
    );
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e).into_response())?;
    tx.commit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?;
    Ok(())
}

/// Map a `(from, to)` status pair to the change-log `kind`. Pulled
/// out of the handlers so the same derivation runs for the
/// `update_status` path AND the rebuilder's regression coverage.
fn derive_change_kind(from: &str, to: &str) -> &'static str {
    match (from, to) {
        (_, "terminated") => "offboard",
        (_, "on-leave") => "leave-start",
        ("on-leave", "active") => "leave-end",
        (_, "active") => "onboard",
        _ => "role-change",
    }
}

#[cfg(test)]
mod tests {
    //! The per-employee change log is gated like `/api/people/changes`
    //! (backlog 8cdad84c). The gate refuses before the pool is touched,
    //! so the pool is lazy and pointed at a host that cannot resolve.
    use super::*;
    use boss_policy::Scope;
    use boss_policy_client::FakePolicyClient;
    use boss_testing::TestRequest;

    fn app(policy: Arc<dyn PolicyClient>) -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://never-connected.invalid/none")
            .expect("a lazy pool does not connect");
        workflow_router(
            pool,
            Arc::new(crate::InMemoryPeople::new(vec![])),
            None,
            Arc::new(boss_clock_client::WallClockClient),
            Some(policy),
        )
    }

    #[tokio::test]
    async fn one_employees_change_log_refuses_a_reader_without_an_employee_read_grant() {
        TestRequest::get("/api/people/emp-002/changes")
            .as_user("emp-002", "service-tech")
            .send(&app(Arc::new(FakePolicyClient::deny_all())))
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn recording_one_employees_change_needs_employee_update_not_read() {
        let read_only: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("hr", Action::Read, Resource::employee(), Scope::All)
                .build(),
        );
        TestRequest::post("/api/people/emp-002/changes")
            .as_user("emp-hr", "hr")
            .json(&serde_json::json!({
                "kind": "promotion",
                "to_value": "9500000",
                "effective_date": "2026-09-23",
            }))
            .send(&app(read_only))
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
}
