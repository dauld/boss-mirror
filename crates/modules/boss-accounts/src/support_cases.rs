//! Support case endpoints — inbound account cases (phone / email /
//! chat) worked by Support Specialists.
//!
//! Separate from device-event-driven service tickets because
//! support cases *originate* from account contact, not device
//! telemetry. A case may cover billing, training, account
//! questions, or device issues that aren't yet formal service
//! tickets.
//!
//! Pattern: self-contained module owning its own state, types,
//! router, and Postgres handlers (matches requisitions.rs, etc.).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use boss_core::publisher::DomainPublisher;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::events::{SUPPORT_CASE_OPENED, SUPPORT_CASE_UPDATED};

/// Payload for `SUPPORT_CASE_UPDATED`. Every field is `Option<>`
/// so an unchanged column round-trips as `None` (the rebuilder
/// applies the same COALESCE semantics as the handler).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportCaseUpdateEvent {
    pub id: String,
    pub status: Option<String>,
    pub assignee_id: Option<String>,
    pub resolved_on: Option<NaiveDate>,
    pub resolution_notes: Option<String>,
    pub csat: Option<i16>,
}

#[derive(Clone)]
pub struct SupportCasesState {
    pub pool: Arc<PgPool>,
    /// Audit-log + NATS publisher. `None` disables emit (tests
    /// that only exercise projection writes can pass `None`).
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
}

pub fn support_cases_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
) -> Router {
    let state = SupportCasesState {
        pool: Arc::new(pool),
        publisher,
        clock,
    };
    Router::new()
        .route(
            "/api/people/support-cases",
            get(list_cases).post(create_case),
        )
        .route("/api/people/support-cases/{id}", put(update_case))
        .with_state(state)
}

/// Full row shape for reads + create bodies.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SupportCase {
    pub id: String,
    pub account_id: String,
    pub channel: String,
    pub category: String,
    pub subject: String,
    pub body: String,
    pub opened_on: NaiveDate,
    pub assignee_id: Option<String>,
    pub status: String,
    pub resolved_on: Option<NaiveDate>,
    pub resolution_notes: Option<String>,
    pub csat: Option<i16>,
}

/// Query params for list filtering. None = all.
#[derive(Debug, Deserialize)]
struct ListFilter {
    status: Option<String>,
    account_id: Option<String>,
    limit: Option<i64>,
}

/// PUT body — any field may be omitted to leave it unchanged.
#[derive(Debug, Deserialize)]
struct UpdateCaseInput {
    status: Option<String>,
    assignee_id: Option<String>,
    resolved_on: Option<NaiveDate>,
    resolution_notes: Option<String>,
    csat: Option<i16>,
}

async fn list_cases(
    State(state): State<SupportCasesState>,
    Query(filter): Query<ListFilter>,
) -> Response {
    let limit = filter.limit.unwrap_or(500).clamp(1, 5000);

    let rows: Result<Vec<SupportCase>, _> = match (filter.status, filter.account_id) {
        (Some(status), Some(account_id)) => {
            sqlx::query_as(
                "SELECT id, account_id, channel, category, subject, body, opened_on,
                    assignee_id, status, resolved_on, resolution_notes, csat
             FROM support_cases
             WHERE status = $1 AND account_id = $2
             ORDER BY opened_on DESC LIMIT $3",
            )
            .bind(status)
            .bind(account_id)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (Some(status), None) => {
            sqlx::query_as(
                "SELECT id, account_id, channel, category, subject, body, opened_on,
                    assignee_id, status, resolved_on, resolution_notes, csat
             FROM support_cases
             WHERE status = $1
             ORDER BY opened_on DESC LIMIT $2",
            )
            .bind(status)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (None, Some(account_id)) => {
            sqlx::query_as(
                "SELECT id, account_id, channel, category, subject, body, opened_on,
                    assignee_id, status, resolved_on, resolution_notes, csat
             FROM support_cases
             WHERE account_id = $1
             ORDER BY opened_on DESC LIMIT $2",
            )
            .bind(account_id)
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
        (None, None) => {
            sqlx::query_as(
                "SELECT id, account_id, channel, category, subject, body, opened_on,
                    assignee_id, status, resolved_on, resolution_notes, csat
             FROM support_cases
             ORDER BY opened_on DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(state.pool.as_ref())
            .await
        }
    };

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_case(
    State(state): State<SupportCasesState>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<SupportCase>,
) -> Response {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Stamp minted before the row write: created_at and the event's
    // timestamp must be ONE instant or replay diverges.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let inserted = match upsert_case(&mut *tx, &req, stamp.timestamp).await {
        Ok(n) => n > 0,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // OUTBOX (phase 2): the opened event records with the row — and
    // only when the INSERT actually inserted. The ON CONFLICT (id)
    // DO NOTHING idempotency guard doubles as the event gate: a
    // replayed create collapses and records nothing (before, every
    // replay published a duplicate opened event).
    if inserted {
        let event = stamp.event(
            SUPPORT_CASE_OPENED,
            serde_json::to_value(&req).unwrap_or_default(),
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

/// Single canonical INSERT for `support_cases`. Used by both the
/// handler and the rebuilder. ON CONFLICT DO NOTHING so re-replay
/// of the same `id` is idempotent.
///
/// `created_at` is the event's recorded instant — the live caller
/// passes `stamp.timestamp`, the rebuilder passes `ev.ts` — so the
/// replayed row matches the live one byte-for-byte instead of
/// falling to the column DEFAULT NOW() (packet d7b8158e).
pub(crate) async fn upsert_case<'e, E>(
    executor: E,
    case: &SupportCase,
    created_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO support_cases (
            id, account_id, channel, category, subject, body, opened_on,
            assignee_id, status, resolved_on, resolution_notes, csat, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&case.id)
    .bind(&case.account_id)
    .bind(&case.channel)
    .bind(&case.category)
    .bind(&case.subject)
    .bind(&case.body)
    .bind(case.opened_on)
    .bind(&case.assignee_id)
    .bind(&case.status)
    .bind(case.resolved_on)
    .bind(&case.resolution_notes)
    .bind(case.csat)
    .bind(created_at)
    .execute(executor)
    .await
    .map(|r| r.rows_affected())
}

async fn update_case(
    State(state): State<SupportCasesState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<UpdateCaseInput>,
) -> Response {
    let evt = SupportCaseUpdateEvent {
        id: id.clone(),
        status: req.status,
        assignee_id: req.assignee_id,
        resolved_on: req.resolved_on,
        resolution_notes: req.resolution_notes,
        csat: req.csat,
    };
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let n = match apply_case_update(&mut *tx, &evt).await {
        Ok(n) => n,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if n == 0 {
        return (StatusCode::NOT_FOUND, format!("no support case {id}")).into_response();
    }
    // OUTBOX (phase 2): the updated event records with the update
    // (the NotFound above returns pre-recording).
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let event = stamp.event(
        SUPPORT_CASE_UPDATED,
        serde_json::to_value(&evt).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

/// Apply a partial update to support_cases via the COALESCE-style
/// "keep existing when None" semantic. Shared with the rebuilder.
pub(crate) async fn apply_case_update<'e, E>(
    executor: E,
    evt: &SupportCaseUpdateEvent,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let r = sqlx::query(
        "UPDATE support_cases SET
            status           = COALESCE($2, status),
            assignee_id      = COALESCE($3, assignee_id),
            resolved_on      = COALESCE($4, resolved_on),
            resolution_notes = COALESCE($5, resolution_notes),
            csat             = COALESCE($6, csat)
         WHERE id = $1",
    )
    .bind(&evt.id)
    .bind(&evt.status)
    .bind(&evt.assignee_id)
    .bind(evt.resolved_on)
    .bind(&evt.resolution_notes)
    .bind(evt.csat)
    .execute(executor)
    .await?;
    Ok(r.rows_affected())
}
