//! `POST /api/ledger/accounts/batch` — the tenant declares its chart of
//! accounts (backlog 41af5195; design 18cf4272). The batch-door shape
//! the classes / locations / agents doors landed on (#418): a JSON
//! array of rows, insert-if-absent by code, operator tier, one
//! `ledger.account.declared` staged on the outbox per INSERTED row in
//! the insert's own transaction, and an answer that names every kept
//! row and how the declaration differs from it. The work is in
//! `crate::chart`; this is the door.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{AccessTier, CurrentUser};

use super::*;
use crate::chart::AccountInput;

/// Gated to operator-tier callers — the tier `boss tenant publish`
/// signs with (`automation:tenant-seed`) — or a sim caller on a sim
/// instance (`boss_policy_client::sim_bypass_allowed`; never the
/// header alone, backlog 85e7f10f). The read layer below already holds
/// the `ledger` read grant; this is the write gate, and an auditor
/// (never operator tier) is refused here too.
pub(super) async fn declare_accounts_batch(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<AccountInput>>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let sim = boss_policy_client::sim_bypass_allowed(&user);
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }
    let stamp = super::event_stamp(&state, &user).await;
    match crate::chart::declare_accounts(&state.pool, &rows, &stamp).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => ledger_err(e),
    }
}
