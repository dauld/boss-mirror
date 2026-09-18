//! The two registry doors a tenant's rule files land through (backlog
//! a40541cb):
//!
//! - `GET  /api/ledger/posting-rules` — every `gl_posting_rules` row.
//! - `POST /api/ledger/posting-rules/batch` — `{tenant_id?, rules:
//!   [PostingRuleInput]}`, insert-if-absent by (fact_kind, version);
//!   `source` is `tenant:<tenant_id>` when given, NULL otherwise.
//! - `GET  /api/ledger/fact-projection-rules` — every
//!   `gl_fact_projection_rules` row, `when` included.
//! - `POST /api/ledger/fact-projection-rules/batch` — `{rules:
//!   [ProjectionRule]}`, insert-if-absent by (event_kind, when).
//!
//! Every row is validated with the same check the TOML loader ran
//! (`validate_posting_rule` / `validate_projection_rule`), and the
//! whole batch is refused (422, naming the row) before any row lands:
//! a registry with a half-admitted tenant is worse than a refusal. A
//! row that lands records its `.declared` fact on the outbox in the
//! same transaction; a kept row records nothing.
//!
//! A projection whose event family the platform stream does not
//! ingest (`boss_nats::durable::stream_subjects`) is refused the same
//! way, naming the family (backlog 94f20e76): a live subscriber
//! filtering on such a family hears nothing and errors never — dead
//! air — and only a rebuild would ever project the rule. The two
//! `products.*` rows the schema seeds (40-ledger.sql) predate the check
//! and never came through this door; they stay because boss-products
//! writes their facts in-tx and the rule only reproduces them on
//! rebuild. A NEW rule on `products.*` is refused here until the
//! family is streamed.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use serde::Deserialize;

use super::{LedgerApiState, ledger_err, reject_if_auditor, storage_err};
use crate::posting_rules::{
    PostingRuleInput, ProjectionRule, list_posting_rules, list_projection_rules,
    publish_posting_rules_in_tx, publish_projection_rules_in_tx, validate_posting_rule,
    validate_projection_rule,
};

pub(super) async fn list_posting_rules_handler(
    State(state): State<Arc<LedgerApiState>>,
) -> Response {
    match list_posting_rules(&state.pool).await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => ledger_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct PostingRulesBatch {
    /// The publishing tenant's `[meta] tenant_id`; the rows land with
    /// `source = tenant:<id>`. Absent → a platform row (`source` NULL).
    #[serde(default)]
    tenant_id: Option<String>,
    rules: Vec<PostingRuleInput>,
}

pub(super) async fn publish_posting_rules_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<PostingRulesBatch>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if let Some(why) = body
        .rules
        .iter()
        .find_map(|r| validate_posting_rule(r).err())
    {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    let source = body
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| format!("tenant:{t}"));
    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    let out =
        match publish_posting_rules_in_tx(&mut tx, &stamp, source.as_deref(), &body.rules).await {
            Ok(o) => o,
            Err(e) => return ledger_err(e),
        };
    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }
    Json(out).into_response()
}

pub(super) async fn list_projection_rules_handler(
    State(state): State<Arc<LedgerApiState>>,
) -> Response {
    match list_projection_rules(&state.pool).await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => ledger_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectionRulesBatch {
    rules: Vec<ProjectionRule>,
}

pub(super) async fn publish_projection_rules_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ProjectionRulesBatch>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if let Some(why) = body
        .rules
        .iter()
        .find_map(|r| validate_projection_rule(r).err())
    {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    let out = match publish_projection_rules_in_tx(&mut tx, &stamp, &body.rules).await {
        Ok(o) => o,
        Err(e) => return ledger_err(e),
    };
    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }
    Json(out).into_response()
}
