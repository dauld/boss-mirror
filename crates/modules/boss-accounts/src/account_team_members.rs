//! Account account team — canonical source of truth for every
//! employee/account relationship.
//!
//! This module owns the `account_team_members` table.
//!
//! Roles are Classes of `employee` Subjects under
//! `(subject_kind='employee', member_attribute='account_team_role')`.
//! v1 ships five seed rows:
//!   - `territory-rep`
//!   - `customer-success`
//!   - `service-manager`
//!   - `finance-contact`
//!   - `executive-sponsor`
//!
//! One person per (account, role) via UNIQUE constraint.
//!
//! **`territory_rep_id` / `territory-rep` contract.** The join table
//! is the canonical source of truth — `AccountTeamPanel` reads all
//! five roles uniformly from here. `accounts.territory_rep_id` is
//! retained as a denormalised cache (fast-path for scope queries in
//! `scope.rs` / `account_next_actions.rs`) and the sim + API write
//! paths keep both in sync atomically: every account INSERT / UPDATE
//! upserts a `(account_id, territory_rep_id, 'territory-rep')` row.
//!
//! Every assign / reassign / remove auto-posts an `interaction` row
//! to `account_notes` in the same transaction so the account detail
//! view's history tab shows the change automatically.
//!
//! Adding a new role is a Class-registry row, not a code change —
//! same shape as every other employee taxonomy (department, role).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::account_notes::{AccountNoteKind, AccountNotePostedEvent, upsert_note};
use crate::events::{ACCOUNT_NOTE_POSTED, ACCOUNT_TEAM_ASSIGNED, ACCOUNT_TEAM_UNASSIGNED};

/// Payload shape for `ACCOUNT_TEAM_ASSIGNED`. Pinned by the
/// rebuilder; renaming fields requires a versioned event family
/// (per `docs/design/projection-rebuilders.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTeamAssignmentEvent {
    pub id: String,
    pub account_id: String,
    pub employee_id: String,
    pub role: String,
    pub assigned_on: NaiveDate,
    pub notes: Option<String>,
}

/// Payload shape for `ACCOUNT_TEAM_UNASSIGNED`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTeamUnassignmentEvent {
    pub account_id: String,
    pub role: String,
    /// The employee who held the role at the time of removal.
    /// Persisted in the event so an auditor can reconstruct who
    /// was unassigned without having to walk the prior assignment
    /// history.
    pub employee_id: String,
    pub unassigned_at: chrono::DateTime<chrono::Utc>,
}

/// One assignment row from `account_team_members`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountTeamMemberAssignment {
    pub id: String,
    pub account_id: String,
    pub employee_id: String,
    pub role: String,
    pub assigned_on: NaiveDate,
    pub notes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Request body for `POST /api/people/accounts/{id}/account-team`.
/// `role` is a Class code in
/// `(subject_kind='employee', member_attribute='account_team_role')`.
/// The handler validates against the registry at write time.
#[derive(Debug, Clone, Deserialize)]
pub struct AssignAccountTeamRequest {
    pub employee_id: String,
    pub role: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// Employee initiating the assignment. Used as `actor_id` on
    /// the auto-posted account_notes row so the audit trail shows
    /// who made the change. No session auth yet, same pattern as
    /// `AssetEvent.actor_id` — frontend fills this in.
    pub actor_id: String,
}

/// Query string for `DELETE /api/people/accounts/{id}/account-team/{role}`.
/// Same rationale as the POST `actor_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct UnassignQuery {
    pub actor_id: String,
}

#[derive(Clone)]
pub struct AccountTeamState {
    pub pool: Arc<PgPool>,
    /// Audit-log + NATS publisher. `None` is allowed for tests
    /// that don't exercise the audit-chain path (handlers then
    /// write only to the projection).
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    /// Class-registry client used to validate `role` codes at write
    /// time against `(subject_kind='employee',
    /// member_attribute='account_team_role')`. `None` skips
    /// validation (tests + the in-memory adapter path).
    pub classes: Option<Arc<dyn ClassesClient>>,
}

pub fn account_team_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    classes: Option<Arc<dyn ClassesClient>>,
) -> Router {
    let state = AccountTeamState {
        pool: Arc::new(pool),
        publisher,
        clock,
        classes,
    };
    Router::new()
        .route(
            "/api/people/accounts/{account_id}/account-team",
            get(list_account_team).post(assign_account_team),
        )
        .route(
            "/api/people/accounts/{account_id}/account-team/{role}",
            axum::routing::delete(unassign_account_team),
        )
        .route(
            "/api/people/account-account-team/batch",
            axum::routing::post(batch_assign_account_team),
        )
        .with_state(state)
}

/// Pre-shaped assignment for bulk replay insertion. Skips the
/// auto-posted interaction note that the single-assignment endpoint
/// writes — the replay generates its own historical notes already.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchAccountTeamItem {
    pub id: String,
    pub account_id: String,
    pub employee_id: String,
    pub role: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Validate `role` against the Class registry under
/// `(subject_kind='employee', member_attribute='account_team_role')`.
/// `Ok(())` when no classes client is configured (in-memory / tests).
async fn validate_role(classes: Option<&Arc<dyn ClassesClient>>, role: &str) -> Result<(), String> {
    let Some(c) = classes else { return Ok(()) };
    let cref = ClassRef::new("employee", role);
    match c.class_exists(&cref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "role `{role}` is not an active Class in the registry"
        )),
        Err(e) => Err(format!("classes registry: {e}")),
    }
}

async fn batch_assign_account_team(
    State(state): State<AccountTeamState>,
    _headers: axum::http::HeaderMap,
    Json(items): Json<Vec<BatchAccountTeamItem>>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let assigned_on = now.date_naive();
    let mut inserted = 0usize;
    for item in &items {
        if let Err(e) = validate_role(state.classes.as_ref(), &item.role).await {
            return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response();
        }
        let evt = AccountTeamAssignmentEvent {
            id: item.id.clone(),
            account_id: item.account_id.clone(),
            employee_id: item.employee_id.clone(),
            role: item.role.clone(),
            assigned_on,
            notes: item.notes.clone(),
        };
        // OUTBOX (phase 2): each assignment records with its row, in
        // its own transaction (preserving this endpoint's
        // committed-prefix semantics on a mid-batch failure).
        let mut tx = match state.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        // Stamp minted before the row write: created_at and the
        // event's timestamp must be ONE instant or replay diverges.
        let stamp = crate::events::event_stamp(&state.publisher).await;
        if let Err(e) = upsert_team_member(&mut *tx, &evt, stamp.timestamp).await {
            return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
        }
        let event = stamp.event(
            ACCOUNT_TEAM_ASSIGNED,
            serde_json::to_value(&evt).unwrap_or_default(),
        );
        if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
        if let Err(e) = tx.commit().await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        inserted += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "inserted": inserted})),
    )
        .into_response()
}

/// Single canonical UPSERT for `account_team_members`. Used by
/// every write path so the SQL stays in one place; the rebuilder
/// uses a near-identical query against a transaction.
///
/// `created_at` is the event's recorded instant — the live caller
/// passes `stamp.timestamp`, the rebuilder passes `ev.ts`. On
/// conflict the column keeps the first insert's instant, live and
/// replayed alike (packet d7b8158e).
pub(crate) async fn upsert_team_member<'e, E>(
    executor: E,
    evt: &AccountTeamAssignmentEvent,
    created_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO account_team_members \
            (id, account_id, employee_id, role, assigned_on, notes, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (account_id, role) DO UPDATE SET \
            employee_id = EXCLUDED.employee_id, \
            assigned_on = EXCLUDED.assigned_on, \
            notes = EXCLUDED.notes",
    )
    .bind(&evt.id)
    .bind(&evt.account_id)
    .bind(&evt.employee_id)
    .bind(&evt.role)
    .bind(evt.assigned_on)
    .bind(&evt.notes)
    .bind(created_at)
    .execute(executor)
    .await
    .map(|_| ())
}

async fn list_account_team(
    State(state): State<AccountTeamState>,
    Path(account_id): Path<String>,
) -> Response {
    let rows: Result<Vec<AccountTeamMemberAssignment>, _> = sqlx::query_as(
        "SELECT id, account_id, employee_id, role, assigned_on, notes, created_at \
         FROM account_team_members \
         WHERE account_id = $1 \
         ORDER BY assigned_on DESC",
    )
    .bind(&account_id)
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn assign_account_team(
    State(state): State<AccountTeamState>,
    Path(account_id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<AssignAccountTeamRequest>,
) -> Response {
    if let Err(e) = validate_role(state.classes.as_ref(), &req.role).await {
        return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response();
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Capture the prior assignment (if any) for the auto-note body.
    let prior: Option<(String,)> = match sqlx::query_as(
        "SELECT employee_id FROM account_team_members \
         WHERE account_id = $1 AND role = $2",
    )
    .bind(&account_id)
    .bind(&req.role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(opt) => opt,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let id = format!("cat-{}", Uuid::new_v4().simple());
    let now = boss_clock_client::now_from(&state.clock).await;
    let evt = AccountTeamAssignmentEvent {
        id: id.clone(),
        account_id: account_id.clone(),
        employee_id: req.employee_id.clone(),
        role: req.role.clone(),
        assigned_on: now.date_naive(),
        notes: req.notes.clone(),
    };

    // Stamp minted before the row writes: created_at on both rows
    // and the events' timestamps must be ONE instant or replay
    // diverges.
    let stamp = crate::events::event_stamp(&state.publisher).await;

    // UPSERT on (account_id, role) so a re-assign cleanly replaces
    // the prior CS without an explicit DELETE round-trip.
    if let Err(e) = upsert_team_member(&mut *tx, &evt, stamp.timestamp).await {
        return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
    }

    // Auto-post an interaction note describing the change. Same
    // transaction so the team change and the audit-trail entry
    // succeed or fail together.
    let body = match prior {
        Some((prev,)) if prev != req.employee_id => format!(
            "Account team: {} reassigned from {} to {}",
            req.role, prev, req.employee_id
        ),
        Some(_) => format!(
            "Account team: {} re-confirmed for {}",
            req.role, req.employee_id
        ),
        None => format!("Account team: {} assigned to {}", req.role, req.employee_id),
    };
    let note_evt = AccountNotePostedEvent {
        id: format!("cn-{}", Uuid::new_v4().simple()),
        account_id: account_id.clone(),
        actor_id: req.actor_id.clone(),
        kind: AccountNoteKind::new("interaction"),
        body,
        occurred_at: now,
    };
    if let Err(e) = upsert_note(&mut *tx, &note_evt, stamp.timestamp).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // OUTBOX (phase 2): the assignment + its auto-posted note record
    // in the SAME transaction as their rows — team change, audit
    // note, and both events succeed or fail together.
    for (kind, payload) in [
        (
            ACCOUNT_TEAM_ASSIGNED,
            serde_json::to_value(&evt).unwrap_or_default(),
        ),
        (
            ACCOUNT_NOTE_POSTED,
            serde_json::to_value(&note_evt).unwrap_or_default(),
        ),
    ] {
        let event = stamp.event(kind, payload);
        if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({"ok": true, "id": id})),
    )
        .into_response()
}

async fn unassign_account_team(
    State(state): State<AccountTeamState>,
    Path((account_id, role)): Path<(String, String)>,
    _headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UnassignQuery>,
) -> Response {
    if let Err(e) = validate_role(state.classes.as_ref(), &role).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e, "role": role })),
        )
            .into_response();
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let removed: Option<(String,)> = match sqlx::query_as(
        "DELETE FROM account_team_members \
         WHERE account_id = $1 AND role = $2 \
         RETURNING employee_id",
    )
    .bind(&account_id)
    .bind(&role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(opt) => opt,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let Some((removed_employee,)) = removed else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let now = boss_clock_client::now_from(&state.clock).await;
    let body = format!(
        "Account team: {} unassigned (was {})",
        role, removed_employee
    );
    let note_evt = AccountNotePostedEvent {
        id: format!("cn-{}", Uuid::new_v4().simple()),
        account_id: account_id.clone(),
        actor_id: query.actor_id.clone(),
        kind: AccountNoteKind::new("interaction"),
        body,
        occurred_at: now,
    };
    // Stamp minted before the note insert: created_at and the
    // events' timestamps must be ONE instant or replay diverges.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = upsert_note(&mut *tx, &note_evt, stamp.timestamp).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // OUTBOX (phase 2): the unassignment + its auto-posted note
    // record in the SAME transaction as the delete + note rows.
    let team_evt = AccountTeamUnassignmentEvent {
        account_id,
        role: role.clone(),
        employee_id: removed_employee,
        unassigned_at: now,
    };
    for (kind, payload) in [
        (
            ACCOUNT_TEAM_UNASSIGNED,
            serde_json::to_value(&team_evt).unwrap_or_default(),
        ),
        (
            ACCOUNT_NOTE_POSTED,
            serde_json::to_value(&note_evt).unwrap_or_default(),
        ),
    ] {
        let event = stamp.event(kind, payload);
        if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_request_deserializes() {
        let json = r#"{
            "employee_id":"emp-007",
            "role":"customer-success",
            "actor_id":"emp-001"
        }"#;
        let req: AssignAccountTeamRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.employee_id, "emp-007");
        assert_eq!(req.role, "customer-success");
        assert_eq!(req.actor_id, "emp-001");
        assert!(req.notes.is_none());
    }

    #[test]
    fn assign_request_with_notes_deserializes() {
        let json = r#"{
            "employee_id":"emp-007",
            "role":"customer-success",
            "notes":"Took over from emp-003 after team reshuffle",
            "actor_id":"emp-001"
        }"#;
        let req: AssignAccountTeamRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.notes.as_deref(),
            Some("Took over from emp-003 after team reshuffle")
        );
    }

    #[test]
    fn assign_request_missing_actor_id_fails() {
        // actor_id is mandatory because every team change needs an
        // identifiable initiator for the auto-posted audit note.
        let json = r#"{"employee_id":"emp-007","role":"customer-success"}"#;
        let result: Result<AssignAccountTeamRequest, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing actor_id should be rejected at the type layer"
        );
    }

    // `role` is a free string at the type layer, so the JSON parser
    // accepts any value; an unknown role is rejected at write time
    // against the Class registry — covered by HTTP integration tests
    // that wire a real ClassesClient.
}
