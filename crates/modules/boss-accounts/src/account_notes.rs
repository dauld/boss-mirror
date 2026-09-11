//! Account notes and interactions log.
//!
//! Owns the `account_notes` table — the "who talked to whom about
//! what" feed for the unified account detail view. Soft-delete by
//! design: rows persist after a UI delete so the operator-tier audit
//! path can still see them. Hard delete is reserved for a CLI escape
//! hatch behind FIDO operator elevation when that lands.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::events::{ACCOUNT_NOTE_DELETED, ACCOUNT_NOTE_POSTED};

/// Payload for `ACCOUNT_NOTE_POSTED`. Pinned by the rebuilder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountNotePostedEvent {
    pub id: String,
    pub account_id: String,
    pub actor_id: String,
    pub kind: AccountNoteKind,
    pub body: String,
    pub occurred_at: DateTime<Utc>,
}

/// Payload for `ACCOUNT_NOTE_DELETED`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountNoteDeletedEvent {
    pub note_id: String,
    pub deleted_by: String,
    pub deleted_at: DateTime<Utc>,
}

/// One row from `account_notes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountNote {
    pub id: String,
    pub account_id: String,
    pub actor_id: String,
    pub kind: AccountNoteKind,
    pub body: String,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Set by a soft-delete; rows where this is `Some` are hidden
    /// from the default GET listing but stay in the heap for audit.
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<String>,
}

/// Categorization of an account note (note, call, meeting, email,
/// interaction, …). Free-text wrapper around a kebab-case string;
/// tenants extend via the Class registry under `(subject_kind='account',
/// member_attribute='note-kind')`. The `account_notes` row stores the
/// code; validation happens at the API boundary against the active Class
/// set per docs/design/class-registry.md. Serializes transparently to
/// the bare string — same lift as catalog's `DocumentKind`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountNoteKind(pub String);

impl AccountNoteKind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountNoteKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AccountNoteKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AccountNoteKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

// Transparent TEXT mapping so `AccountNote`'s derived `FromRow` still
// decodes `kind` straight off the column (the dropped `sqlx::Type`
// derive used to provide this). The registry — not a DB CHECK — gates
// which codes are legal; the column itself is plain TEXT.
impl sqlx::Type<sqlx::Postgres> for AccountNoteKind {
    fn type_info() -> <sqlx::Postgres as sqlx::Database>::TypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &<sqlx::Postgres as sqlx::Database>::TypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for AccountNoteKind {
    fn decode(
        value: <sqlx::Postgres as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for AccountNoteKind {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0.as_str(), buf)
    }
}

/// Request body for `POST /api/people/accounts/{id}/notes`.
///
/// `actor_id` will eventually come from the session. Until session
/// auth lands, the frontend passes it explicitly — same pattern as
/// `AssetEvent.actor_id` and the account team handlers.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountNoteRequest {
    /// Caller-supplied id for the event-sourced note. The simulator
    /// provides a deterministic one (so the emitted
    /// `ACCOUNT_NOTE_POSTED` carries a stable id across replays);
    /// interactive callers omit it and the handler mints a fresh
    /// `cn-<uuid>`.
    #[serde(default)]
    pub id: Option<String>,
    pub kind: AccountNoteKind,
    pub body: String,
    pub actor_id: String,
    /// Optional explicit time of the underlying interaction
    /// (defaults to NOW() at the database). Use this when
    /// back-filling a meeting that happened earlier in the day.
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
}

/// Query string for `DELETE /api/people/accounts/{id}/notes/{note_id}`.
/// Same `actor_id`-via-query pattern as account team unassign.
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteNoteQuery {
    pub actor_id: String,
}

/// Query string for `GET /api/people/accounts/{id}/notes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListNotesQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

#[derive(Clone)]
pub struct AccountNotesState {
    pub pool: Arc<PgPool>,
    /// Audit-log + NATS publisher. `None` disables emit (tests
    /// that only exercise projection writes can pass `None`).
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    /// Class-registry client used to validate note `kind` codes at
    /// write time against `(subject_kind='account',
    /// member_attribute='note-kind')`. `None` skips validation (tests
    /// + the in-memory adapter path). The binary always wires `Some`,
    /// so production fails loud.
    pub classes: Option<Arc<dyn ClassesClient>>,
}

pub fn account_notes_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    classes: Option<Arc<dyn ClassesClient>>,
) -> Router {
    let state = AccountNotesState {
        pool: Arc::new(pool),
        publisher,
        clock,
        classes,
    };
    Router::new()
        .route(
            "/api/people/accounts/{account_id}/notes",
            get(list_notes).post(create_note),
        )
        .route(
            "/api/people/accounts/{account_id}/notes/{note_id}",
            axum::routing::delete(delete_note),
        )
        .with_state(state)
}

/// Validate a note `kind` against the Class registry under
/// `(subject_kind='account', member_attribute='note-kind')`.
///
/// Same contract as catalog's `check_category` / `check_document_kinds`:
/// permissive when no registry is wired (`None` — tests / in-memory),
/// fail-closed (503) when the registry is unreachable, 400 on an
/// unregistered code. Returns `Ok(())` on success, or the response to
/// short-circuit on rejection.
async fn check_note_kind(
    classes: Option<&Arc<dyn ClassesClient>>,
    kind: &AccountNoteKind,
) -> Result<(), Response> {
    let Some(client) = classes else {
        return Ok(());
    };
    let class_ref = ClassRef::new("account", kind.as_str());
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown account note kind `{}` — register it as a Class first \
                 (subject_kind='account', member_attribute='note-kind')",
                kind.as_str()
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Single canonical INSERT for `account_notes`. Used by every
/// write path including the rebuilder. ON CONFLICT DO NOTHING so
/// a re-replay of the same `id` is idempotent.
///
/// `created_at` is the event's recorded instant — the live caller
/// passes `stamp.timestamp`, the rebuilder passes `ev.ts` (the same
/// value read back from audit_log). Leaving the column to its
/// DEFAULT NOW() stamped write-time live and replay-time on rebuild
/// (packet d7b8158e).
pub(crate) async fn upsert_note<'e, E>(
    executor: E,
    evt: &AccountNotePostedEvent,
    created_at: DateTime<Utc>,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO account_notes \
            (id, account_id, actor_id, kind, body, occurred_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&evt.id)
    .bind(&evt.account_id)
    .bind(&evt.actor_id)
    .bind(evt.kind.as_str())
    .bind(&evt.body)
    .bind(evt.occurred_at)
    .bind(created_at)
    .execute(executor)
    .await
    .map(|r| r.rows_affected())
}

async fn list_notes(
    State(state): State<AccountNotesState>,
    Path(account_id): Path<String>,
    Query(q): Query<ListNotesQuery>,
) -> Response {
    let limit = q.limit.clamp(1, 200);
    let rows: Result<Vec<AccountNote>, _> = sqlx::query_as(
        "SELECT id, account_id, actor_id, kind, body, occurred_at, \
                created_at, deleted_at, deleted_by \
         FROM account_notes \
         WHERE account_id = $1 AND deleted_at IS NULL \
         ORDER BY occurred_at DESC \
         LIMIT $2",
    )
    .bind(&account_id)
    .bind(limit)
    .fetch_all(state.pool.as_ref())
    .await;

    match rows {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_note(
    State(state): State<AccountNotesState>,
    Path(account_id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(req): Json<CreateAccountNoteRequest>,
) -> Response {
    if let Err(resp) = check_note_kind(state.classes.as_ref(), &req.kind).await {
        return resp;
    }
    let id = req
        .id
        .clone()
        .unwrap_or_else(|| format!("cn-{}", Uuid::new_v4().simple()));
    let now = boss_clock_client::now_from(&state.clock).await;
    let evt = AccountNotePostedEvent {
        id: id.clone(),
        account_id,
        actor_id: req.actor_id,
        kind: req.kind,
        body: req.body,
        occurred_at: req.occurred_at.unwrap_or(now),
    };
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Stamp minted before the row write: the row's created_at and
    // the event's timestamp must be ONE instant or replay diverges.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let inserted = match upsert_note(&mut *tx, &evt, stamp.timestamp).await {
        Ok(n) => n > 0,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    };
    // OUTBOX (phase 2): the posted event records with the row — and
    // only when the INSERT actually inserted (the ON CONFLICT (id)
    // DO NOTHING guard doubles as the event gate, so a replayed
    // create records nothing).
    if inserted {
        let event = stamp.event(
            ACCOUNT_NOTE_POSTED,
            serde_json::to_value(&evt).unwrap_or_default(),
        );
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

async fn delete_note(
    State(state): State<AccountNotesState>,
    Path((_account_id, note_id)): Path<(String, String)>,
    _headers: axum::http::HeaderMap,
    Query(query): Query<DeleteNoteQuery>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let evt = AccountNoteDeletedEvent {
        note_id: note_id.clone(),
        deleted_by: query.actor_id.clone(),
        deleted_at: now,
    };
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let n = match soft_delete_note(&mut *tx, &evt).await {
        Ok(n) => n,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if n == 0 {
        return StatusCode::NOT_FOUND.into_response();
    }
    // OUTBOX (phase 2): the deleted event records with the flip (the
    // NotFound above returns pre-recording; an already-deleted note
    // matches 0 rows and records nothing).
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let event = stamp.event(
        ACCOUNT_NOTE_DELETED,
        serde_json::to_value(&evt).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Soft-delete projection write. Shared with the rebuilder so the
/// stamp shape stays in one place.
pub(crate) async fn soft_delete_note<'e, E>(
    executor: E,
    evt: &AccountNoteDeletedEvent,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let r = sqlx::query(
        "UPDATE account_notes \
         SET deleted_at = $2, deleted_by = $3 \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(&evt.note_id)
    .bind(evt.deleted_at)
    .bind(&evt.deleted_by)
    .execute(executor)
    .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_json() {
        // The newtype serializes transparently to the bare kebab-case
        // string and parses any string back (the closed enum is gone;
        // the Class registry — not the type — gates legal codes).
        for code in ["note", "call", "meeting", "email", "interaction"] {
            let kind = AccountNoteKind::new(code);
            let s = serde_json::to_string(&kind).unwrap();
            assert_eq!(s, format!("\"{code}\""), "serializes to the bare string");
            let back: AccountNoteKind = serde_json::from_str(&s).unwrap();
            assert_eq!(back, kind, "kind {kind:?} should roundtrip");
            assert_eq!(back.as_str(), code);
        }
    }

    #[test]
    fn kind_accepts_arbitrary_codes() {
        // The parser accepts any string, so a tenant-specific note
        // kind doesn't force a core fork. Validation happens at the
        // write-time Class-registry gate (`check_note_kind`), covered
        // by the HTTP integration tests.
        let json = r#""voicemail""#;
        let kind: AccountNoteKind = serde_json::from_str(json).unwrap();
        assert_eq!(kind.as_str(), "voicemail");
    }

    #[test]
    fn create_request_minimal_deserializes() {
        let json = r#"{
            "kind":"note",
            "body":"Talked to Dr. Lee about the LUM-3D",
            "actor_id":"emp-007"
        }"#;
        let req: CreateAccountNoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.kind, AccountNoteKind::new("note"));
        assert_eq!(req.body, "Talked to Dr. Lee about the LUM-3D");
        assert_eq!(req.actor_id, "emp-007");
        assert!(req.occurred_at.is_none());
    }

    #[test]
    fn create_request_with_occurred_at_deserializes() {
        let json = r#"{
            "kind":"call",
            "body":"Quarterly review call",
            "actor_id":"emp-007",
            "occurred_at":"2026-04-10T14:30:00Z"
        }"#;
        let req: CreateAccountNoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.kind, AccountNoteKind::new("call"));
        assert!(req.occurred_at.is_some());
    }

    #[test]
    fn create_request_missing_kind_rejected() {
        // `kind` stays REQUIRED — a note must declare what it is.
        let json = r#"{"body":"hi","actor_id":"emp-007"}"#;
        let result: Result<CreateAccountNoteRequest, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing kind must be rejected — a note always has a kind"
        );
    }

    #[test]
    fn missing_author_id_rejected() {
        let json = r#"{"kind":"note","body":"hi"}"#;
        let result: Result<CreateAccountNoteRequest, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "actor_id is required so every note has an identifiable origin"
        );
    }
}
