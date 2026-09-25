//! `POST /api/ledger/tax/batch` — the tenant declares its tax regime:
//! filing kinds and sales-tax rates (backlog 7f163e58; the chart
//! door's shape, 41af5195). Insert-if-absent by kind and by state,
//! operator tier, one `ledger.tax_kind.declared` /
//! `ledger.sales_tax_rate.declared` staged on the outbox per INSERTED
//! row in the insert's own transaction, and an answer that names every
//! kept row and the declared fields it differs on. The work is in
//! `crate::tax_registry`; this is the door. The two GETs read the
//! registry back in the declaration's own shape.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{AccessTier, CurrentUser};

use super::*;
use crate::tax_registry::TaxSeed;

/// Gated to operator-tier callers — the tier `boss tenant publish`
/// signs with — or a sim caller on a sim instance
/// (`boss_policy_client::sim_bypass_allowed`; never the header alone,
/// backlog 85e7f10f).
pub(super) async fn declare_tax_batch(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(seed): Json<TaxSeed>,
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
    match crate::tax_registry::declare_tax(&state.pool, &seed, &stamp).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn list_tax_kinds(State(state): State<Arc<LedgerApiState>>) -> Response {
    match crate::tax_registry::list_tax_kinds(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn list_sales_tax_rates(State(state): State<Arc<LedgerApiState>>) -> Response {
    match crate::tax_registry::list_sales_tax_rates(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => ledger_err(e),
    }
}
