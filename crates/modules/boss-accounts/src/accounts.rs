//! Accounts API — customer account directory.

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
use std::sync::Arc;

use boss_assets_client::{AssetsClient, AssetsClientError};

#[derive(Clone)]
pub struct AccountsState {
    pub pool: Arc<PgPool>,
    pub publisher: Option<DomainPublisher>,
    pub assets_client: Arc<dyn AssetsClient>,
    /// Authoritative clock. Every handler reads `now` through it so
    /// audit_log timestamps follow the deployment's sim/wall mode
    /// instead of leaking wallclock.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Class-registry client used to validate `account_type` and
    /// `tier` codes at write time against the `account` subject kind.
    /// `None` skips validation (tests + the in-memory adapter path).
    /// The binary always wires `Some`, so production fails loud on an
    /// unregistered code.
    pub classes: Option<Arc<dyn ClassesClient>>,
}

pub fn accounts_router(
    pool: PgPool,
    publisher: Option<DomainPublisher>,
    assets_client: Arc<dyn AssetsClient>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
    classes: Option<Arc<dyn ClassesClient>>,
) -> Router {
    let state = AccountsState {
        pool: Arc::new(pool),
        publisher,
        assets_client,
        clock,
        classes,
    };
    Router::new()
        .route(
            "/api/people/accounts",
            get(list_accounts).post(create_account),
        )
        .route(
            "/api/people/accounts/{id}",
            get(get_account).put(update_account).delete(delete_account),
        )
        .route("/api/people/accounts/{id}/exists", get(account_exists))
        .route(
            "/api/people/accounts/{id}/contacts",
            axum::routing::put(replace_contacts),
        )
        .route("/api/people/accounts/{id}/facts", get(list_account_facts))
        .with_state(state)
}

/// Lightweight existence check used by cross-service write guards
/// (boss-commerce's account_id validation, etc). Returns just
/// `{"exists": bool}` so the caller doesn't pay for joining the
/// contacts table.
async fn account_exists(State(state): State<AccountsState>, Path(id): Path<String>) -> Response {
    let row: Result<Option<(String,)>, _> = sqlx::query_as("SELECT id FROM accounts WHERE id = $1")
        .bind(&id)
        .fetch_optional(state.pool.as_ref())
        .await;
    match row {
        Ok(opt) => Json(serde_json::json!({ "exists": opt.is_some() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    /// Identity-first: only `id` is required to create an account. Every
    /// descriptive field below is enriched after the stable identity
    /// exists (a prospect account can be opened from an id alone, then
    /// filled in as the relationship develops). Each is nullable in the
    /// schema and `None` until set.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub director: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub customer_since: Option<NaiveDate>,
    #[serde(default)]
    pub territory_rep_id: Option<String>,
    /// Tenant-extensible account discriminator (brewery:
    /// wholesale-distributor / bar-restaurant / chain-retail / …).
    /// Defaults to `unspecified` so older payloads without the
    /// field still parse — same shape the schema column uses.
    #[serde(default = "default_account_type")]
    pub account_type: String,
}

fn default_account_type() -> String {
    "unspecified".to_string()
}

/// Validate `account_type` against the Class registry under
/// `(subject_kind='account', member_attribute='type')`.
///
/// Same contract as catalog's `check_category`: permissive when no
/// registry is wired (`None` — tests / in-memory), fail-closed (503)
/// when the registry is unreachable, 400 on an unregistered code.
/// `account_type` is `NOT NULL DEFAULT 'unspecified'` so it is always
/// present — every value (including the default) must be a registered
/// Class. Returns `Ok(())` on success, or the response to short-circuit.
async fn check_account_type(
    classes: Option<&Arc<dyn ClassesClient>>,
    account_type: &str,
) -> Result<(), Response> {
    let Some(client) = classes else {
        return Ok(());
    };
    let class_ref = ClassRef::new("account", account_type);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown account type `{account_type}` — register it as a Class first \
                 (subject_kind='account', member_attribute='type')"
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

/// Validate `tier` against the Class registry under
/// `(subject_kind='account', member_attribute='tier')`.
///
/// `tier` is identity-first/nullable: `None` is the untiered state and
/// is always allowed (skip the check). Only a `Some` value is gated.
/// Otherwise identical to `check_account_type` (permissive without a
/// registry, 503 unreachable, 400 unregistered).
async fn check_tier(
    classes: Option<&Arc<dyn ClassesClient>>,
    tier: Option<&str>,
) -> Result<(), Response> {
    let (Some(client), Some(tier)) = (classes, tier) else {
        return Ok(());
    };
    let class_ref = ClassRef::new("account", tier);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown account tier `{tier}` — register it as a Class first \
                 (subject_kind='account', member_attribute='tier')"
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

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountContact {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub role: String,
    pub email: String,
    pub phone: Option<String>,
    pub is_primary: bool,
}

#[derive(Serialize, Deserialize)]
pub struct AccountWithContacts {
    #[serde(flatten)]
    pub account: Account,
    pub contacts: Vec<AccountContact>,
}

/// Upsert the `(account_id, territory-rep)` row in
/// `account_team_members` so the join table mirrors
/// `accounts.territory_rep_id`. Called inside `create_account` +
/// `update_account` in the same transaction so the two never drift.
///
/// Id format `pat-tr-<account_id>` matches the schema's one-shot
/// backfill so rows created by either path are visually consistent.
///
/// Returns the assignment event so the caller can emit it through
/// the publisher AFTER the surrounding transaction commits.
/// Emit-after-commit avoids publishing an event for a write that
/// then rolls back, which would make `audit_log` and the projection
/// disagree.
async fn mirror_territory_rep(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    employee_id: &str,
    customer_since: NaiveDate,
    created_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<crate::account_team_members::AccountTeamAssignmentEvent> {
    let evt = crate::account_team_members::AccountTeamAssignmentEvent {
        id: format!("pat-tr-{account_id}"),
        account_id: account_id.to_string(),
        employee_id: employee_id.to_string(),
        role: "territory-rep".to_string(),
        assigned_on: customer_since,
        notes: None,
    };
    crate::account_team_members::upsert_team_member(&mut **tx, &evt, created_at).await?;
    Ok(evt)
}

async fn list_accounts(State(state): State<AccountsState>) -> Response {
    let rows: Result<Vec<Account>, _> =
        sqlx::query_as("SELECT id, name, director, city, state, tier, customer_since, territory_rep_id, account_type FROM accounts ORDER BY id")
            .fetch_all(state.pool.as_ref())
            .await;

    match rows {
        Ok(accounts) => Json(accounts).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_account(State(state): State<AccountsState>, Path(id): Path<String>) -> Response {
    let account: Result<Option<Account>, _> = sqlx::query_as(
        "SELECT id, name, director, city, state, tier, customer_since, territory_rep_id, account_type FROM accounts WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(state.pool.as_ref())
    .await;

    let account = match account {
        Ok(Some(c)) => c,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let contacts: Result<Vec<AccountContact>, _> = sqlx::query_as(
        "SELECT id, account_id, name, role, email, phone, is_primary FROM account_contacts WHERE account_id = $1 ORDER BY id",
    )
    .bind(&id)
    .fetch_all(state.pool.as_ref())
    .await;

    match contacts {
        Ok(contacts) => Json(AccountWithContacts { account, contacts }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_account(
    State(state): State<AccountsState>,
    _headers: axum::http::HeaderMap,
    Json(body): Json<AccountWithContacts>,
) -> Response {
    // Registry gates before any write: account_type is always present
    // (NOT NULL DEFAULT), tier only when Some.
    if let Err(resp) = check_account_type(state.classes.as_ref(), &body.account.account_type).await
    {
        return resp;
    }
    if let Err(resp) = check_tier(state.classes.as_ref(), body.account.tier.as_deref()).await {
        return resp;
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Check for duplicate
    let existing: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = $1")
            .bind(&body.account.id)
            .fetch_optional(&mut *tx)
            .await;

    match existing {
        Ok(Some(_)) => return (StatusCode::CONFLICT, "account ID already exists").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        _ => {}
    }

    // Identity write-through (subject-model R1, Q1): same tx as the
    // domain row.
    if let Err(e) = boss_subject_kinds::subjects::record_subject_in_tx(
        &mut tx,
        "account",
        &body.account.id,
        body.account.name.as_deref(),
    )
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    let res = sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id, account_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&body.account.id)
    .bind(&body.account.name)
    .bind(&body.account.director)
    .bind(&body.account.city)
    .bind(&body.account.state)
    .bind(&body.account.tier)
    .bind(body.account.customer_since)
    .bind(&body.account.territory_rep_id)
    .bind(&body.account.account_type)
    .execute(&mut *tx)
    .await;

    if let Err(e) = res {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Stamp minted before the child-row writes: their created_at and
    // the events' timestamps must be ONE instant or replay diverges
    // (packet d7b8158e).
    let stamp = crate::events::event_stamp(&state.publisher).await;

    // Identity-first: mirror the territory rep into account_team_members
    // only when one is actually assigned (and we have a date to stamp
    // the assignment). An id-only account has no rep yet — nothing to
    // mirror; the assignment lands when the account is enriched.
    let territory_evt = if let (Some(rep), Some(since)) = (
        body.account.territory_rep_id.as_deref(),
        body.account.customer_since,
    ) {
        match mirror_territory_rep(&mut tx, &body.account.id, rep, since, stamp.timestamp).await {
            Ok(evt) => Some(evt),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        None
    };

    for contact in &body.contacts {
        let res = sqlx::query(
            "INSERT INTO account_contacts (id, account_id, name, role, email, phone, is_primary, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&contact.id)
        .bind(&body.account.id)
        .bind(&contact.name)
        .bind(&contact.role)
        .bind(&contact.email)
        .bind(&contact.phone)
        .bind(contact.is_primary)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await;

        if let Err(e) = res {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    // OUTBOX (phase 2): the created event (full account + contacts
    // state — what the rebuilder consumes) records with the rows,
    // plus the territory-rep mirror so rebuild_accounts can
    // repopulate `account_team_members` — only when a rep was
    // actually assigned at create.
    let event = stamp.event(
        crate::events::ACCOUNT_CREATED,
        serde_json::to_value(&body).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Some(evt) = &territory_evt {
        let event = stamp.event(
            crate::events::ACCOUNT_TEAM_ASSIGNED,
            serde_json::to_value(evt).unwrap_or_default(),
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
        Json(serde_json::json!({ "id": body.account.id })),
    )
        .into_response()
}

async fn update_account(
    State(state): State<AccountsState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(body): Json<AccountWithContacts>,
) -> Response {
    // Registry gates before any write (same as create): account_type
    // is always present, tier only when Some.
    if let Err(resp) = check_account_type(state.classes.as_ref(), &body.account.account_type).await
    {
        return resp;
    }
    if let Err(resp) = check_tier(state.classes.as_ref(), body.account.tier.as_deref()).await {
        return resp;
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Check exists
    let existing: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await;

    match existing {
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        _ => {}
    }

    // Delete old contacts then old account, re-insert both
    if let Err(e) = sqlx::query("DELETE FROM account_contacts WHERE account_id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Err(e) = sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Identity write-through (subject-model R1, Q1): same tx as the
    // domain row.
    if let Err(e) = boss_subject_kinds::subjects::record_subject_in_tx(
        &mut tx,
        "account",
        &body.account.id,
        body.account.name.as_deref(),
    )
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    let res = sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id, account_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&id)
    .bind(&body.account.name)
    .bind(&body.account.director)
    .bind(&body.account.city)
    .bind(&body.account.state)
    .bind(&body.account.tier)
    .bind(body.account.customer_since)
    .bind(&body.account.territory_rep_id)
    .bind(&body.account.account_type)
    .execute(&mut *tx)
    .await;

    if let Err(e) = res {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Identity-first: mirror only when a rep is assigned and dated.
    // Stamp minted before the child-row writes: their created_at and
    // the events' timestamps must be ONE instant or replay diverges
    // (packet d7b8158e).
    let stamp = crate::events::event_stamp(&state.publisher).await;

    let territory_evt = if let (Some(rep), Some(since)) = (
        body.account.territory_rep_id.as_deref(),
        body.account.customer_since,
    ) {
        match mirror_territory_rep(&mut tx, &id, rep, since, stamp.timestamp).await {
            Ok(evt) => Some(evt),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        None
    };

    for contact in &body.contacts {
        let res = sqlx::query(
            "INSERT INTO account_contacts (id, account_id, name, role, email, phone, is_primary, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&contact.id)
        .bind(&id)
        .bind(&contact.name)
        .bind(&contact.role)
        .bind(&contact.email)
        .bind(&contact.phone)
        .bind(contact.is_primary)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await;

        if let Err(e) = res {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    // OUTBOX (phase 2): full account + contacts state (rebuild
    // treats UPDATED the same as CREATED — UPSERT both projections),
    // plus the territory-rep mirror when assigned. Records with the
    // rows.
    let event = stamp.event(
        crate::events::ACCOUNT_UPDATED,
        serde_json::to_value(&body).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Some(evt) = &territory_evt {
        let event = stamp.event(
            crate::events::ACCOUNT_TEAM_ASSIGNED,
            serde_json::to_value(evt).unwrap_or_default(),
        );
        if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Replace a account's entire contact list. Lighter than the full
/// `update_account` handler — it touches `account_contacts` only, not
/// the `accounts` row — so the sim's contact-rotation generator
/// doesn't have to re-send the account metadata on every rotation.
async fn replace_contacts(
    State(state): State<AccountsState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
    Json(contacts): Json<Vec<AccountContact>>,
) -> Response {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Account must exist (prevents orphan contacts).
    let exists: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await;
    match exists {
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        _ => {}
    }

    if let Err(e) = sqlx::query("DELETE FROM account_contacts WHERE account_id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Stamp minted before the contact inserts: created_at and the
    // event's timestamp must be ONE instant or replay diverges
    // (packet d7b8158e).
    let stamp = crate::events::event_stamp(&state.publisher).await;

    for contact in &contacts {
        let res = sqlx::query(
            "INSERT INTO account_contacts (id, account_id, name, role, email, phone, is_primary, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&contact.id)
        .bind(&id)
        .bind(&contact.name)
        .bind(&contact.role)
        .bind(&contact.email)
        .bind(&contact.phone)
        .bind(contact.is_primary)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await;

        if let Err(e) = res {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    // OUTBOX (phase 2): read the account row back INSIDE the
    // transaction so the event carries full AccountWithContacts
    // state (same shape the rebuilder consumes for create/update)
    // and records atomically with the contact rows. The old
    // post-commit read-back silently SKIPPED the event when the
    // read failed — a committed fact with no event.
    let account = match sqlx::query_as::<_, Account>(
        "SELECT id, name, director, city, state, tier, customer_since, \
                territory_rep_id, account_type \
         FROM accounts WHERE id = $1",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(a) => a,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let payload = AccountWithContacts { account, contacts };
    let event = stamp.event(
        crate::events::ACCOUNT_UPDATED,
        serde_json::to_value(&payload).unwrap_or_default(),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn delete_account(
    State(state): State<AccountsState>,
    Path(id): Path<String>,
    _headers: axum::http::HeaderMap,
) -> Response {
    // Cross-service guard: refuse to delete a account that still has
    // unresolved service work on any of its installed devices. If
    // assets is unreachable, fail closed — a account delete is a rare
    // destructive operation and quietly succeeding is worse than a
    // loud 503 telling the operator to try again once assets is back.
    match state.assets_client.open_ticket_count_for_account(&id).await {
        Ok(0) => {}
        Ok(n) => {
            return (
                StatusCode::CONFLICT,
                format!("account {id} has {n} open service ticket(s); close them first"),
            )
                .into_response();
        }
        Err(AssetsClientError::Unreachable(msg)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("cannot verify account is safe to delete: assets unreachable ({msg})"),
            )
                .into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if let Err(e) = sqlx::query("DELETE FROM account_contacts WHERE account_id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let result = sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await;

    match result {
        Ok(r) if r.rows_affected() == 0 => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        _ => {}
    }

    // OUTBOX (phase 2): the deleted event records only after the row
    // actually deleted (the rows_affected NotFound above returns
    // pre-recording).
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let event = stamp.event(
        crate::events::ACCOUNT_DELETED,
        serde_json::json!({ "id": id, "deleted_at": now }),
    );
    if let Err(e) = boss_events::outbox::record_event_in_tx(&mut tx, &event).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Account facts (KB pattern)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
struct AccountFactRow {
    id: String,
    account_id: String,
    fact_kind: String,
    occurred_at: NaiveDate,
    actor_id: Option<String>,
    job_id: Option<String>,
    step_id: Option<String>,
    payload: serde_json::Value,
}

async fn list_account_facts(
    State(state): State<AccountsState>,
    Path(id): Path<String>,
) -> Response {
    let rows = sqlx::query_as::<_, AccountFactRow>(
        "SELECT id, account_id, fact_kind, occurred_at, actor_id, job_id, step_id, payload \
         FROM account_facts WHERE account_id = $1 ORDER BY occurred_at DESC LIMIT 200",
    )
    .bind(&id)
    .fetch_all(&*state.pool)
    .await;

    match rows {
        Ok(facts) => Json(facts).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// There is no `create_fact` endpoint. account_facts is a projection
// accumulated from jobs.* events (per the schema comment
// "accumulated from Jobs/Steps involving accounts (D2)") — writing it
// directly would be an audit-chain bypass for a table that should be
// derived, not written. Restore via a step-effects subscriber if
// facts ever need to land outside the jobs pipeline.
