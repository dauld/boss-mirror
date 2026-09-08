//! HTTP surface for the ledger. v1d ships read-only endpoints:
//!
//! - `GET /api/ledger/health`
//! - `GET /api/ledger/accounts` — full chart of accounts
//! - `GET /api/ledger/trial-balance?as_of=YYYY-MM-DD` — per-account totals
//! - `GET /api/ledger/entries?account_code=XXXX&limit=N` — drill-down entries
//! - `GET /api/ledger/entries?fact_id=UUID` — entries for a specific fact
//! - `GET /api/ledger/entries/:id` — single entry with lines
//!
//! Posting is synchronous inside domain writes (see `postgres::post_fact_in_tx`),
//! so this service is a pure read layer.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use boss_policy::User;
use sqlx::PgPool;

mod bank_settlements;
mod bills;
mod entries;
mod facts;
mod keg_deposits;
mod payroll;
mod periods;
mod revenue;
mod statements;
mod tax;

use bank_settlements::*;
use bills::*;
use entries::*;
use facts::*;
use keg_deposits::*;
use payroll::*;
use periods::*;
use revenue::*;
use statements::*;
use tax::*;

/// Backend write-gate on `/api/ledger/*`. The `auditor` role is
/// strictly read-only — prior hardening pass only hid the write
/// buttons on the Finance UI; this returns 403 so a hand-crafted
/// curl from an auditor session can't bypass the UI. Matches the
/// `role === 'auditor'` check in `apps/web/src/finance/
/// FinancePage.svelte`.
fn reject_if_auditor(user: &User) -> Option<Response> {
    if user.role == "auditor" {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "auditor role is read-only for /api/ledger/*",
            )
                .into_response(),
        );
    }
    None
}

#[derive(Clone)]
pub struct LedgerApiState {
    pub pool: PgPool,
    /// Domain publisher for upstream `ledger.*` events. `None` in tests
    /// and when the binary launches without `nats_url`. Every
    /// fact-write site emits its corresponding `ledger.<thing>` event
    /// when this is `Some` so `rebuild_facts` has audit_log rows to
    /// project from.
    pub publisher: Option<Arc<boss_core::publisher::DomainPublisher>>,
    /// Authoritative clock. Every handler that stamps a date into a
    /// financial_fact or audit event reads `now` via
    /// `state.clock.now().await`. Production wires
    /// `ReqwestClockClient` pointing at the deployed `boss-clock-api`
    /// (wall mode); demo wires it pointing at the sim-mode
    /// clock-api. Services never inspect headers or env vars to
    /// learn whether time is sim or wall — the Clock decides.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Policy engine for the read gate below. `None` leaves the
    /// surface open, which is what tests want and what production
    /// must never be — `boss-ledger-api` always wires one.
    pub policy: Option<Arc<dyn boss_policy_client::PolicyClient>>,
}

/// Router-wide gate on READING `/api/ledger/*`.
///
/// Everything here is the company's finances: the trial balance, all
/// three statements, every journal entry, tax liability, bills. None
/// of it had any read gate at all — `reject_if_auditor` is a WRITE
/// gate (it stops an auditor session curling a POST past the hidden
/// UI buttons), so a caller who could reach the port could read the
/// whole ledger.
///
/// A layer rather than a per-handler check on purpose. The write gate
/// is called from 21 handlers by hand; that shape works right up until
/// someone adds a route and forgets, and a forgotten read gate is
/// invisible until it is someone else's incident. A layer cannot be
/// forgotten by a new route.
///
/// `/health` is exempt: monitoring probes it without a session, and it
/// returns no financial data.
async fn require_ledger_read(
    State(state): State<Arc<LedgerApiState>>,
    boss_policy_client::CurrentUser(user): boss_policy_client::CurrentUser,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if req.uri().path().ends_with("/health") {
        return next.run(req).await;
    }
    let Some(policy) = state.policy.as_ref() else {
        return next.run(req).await;
    };
    match policy
        .check(
            &user,
            boss_policy_client::Action::Read,
            boss_policy_client::Resource::ledger(),
        )
        .await
    {
        Ok(d) if d.is_allowed() => next.run(req).await,
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "reading /api/ledger/* requires a `ledger` read grant",
        )
            .into_response(),
        Err(e) => {
            // Fail closed. An unreachable policy engine must not open
            // the company's books.
            tracing::warn!(error = %e, "ledger read gate: policy check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "policy engine unavailable").into_response()
        }
    }
}

/// Build the enrichment stamp for in-tx event recording (outbox
/// phase 2): the caller's actor + the authoritative timestamp, with
/// `_simulated` resolved by the publisher's clock probe when one is
/// wired. Fact-write handlers record their `ledger.*` events in the
/// DOMAIN TRANSACTION via this stamp (see
/// `events::record_ledger_event_in_tx`); nothing publishes them
/// post-commit anymore.
pub(crate) async fn event_stamp(
    state: &LedgerApiState,
    user: &boss_policy_client::User,
) -> boss_core::publisher::EventStamp {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    match &state.publisher {
        Some(p) => p.stamp_with_actor(actor).await,
        None => boss_core::publisher::EventStamp::new("ledger", actor),
    }
}

pub fn router(state: LedgerApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/ledger/health", get(health))
        .route("/api/ledger/accounts", get(list_accounts))
        .route("/api/ledger/trial-balance", get(trial_balance))
        .route("/api/ledger/income-statement", get(income_statement))
        .route("/api/ledger/balance-sheet", get(balance_sheet))
        .route("/api/ledger/cash-flow", get(cash_flow_statement))
        .route("/api/ledger/entries", get(list_entries))
        .route("/api/ledger/entries/{id}", get(get_entry))
        .route(
            "/api/ledger/periods",
            get(list_periods_handler).post(create_period_handler),
        )
        .route(
            "/api/ledger/periods/{id}/lock",
            axum::routing::post(lock_handler),
        )
        .route(
            "/api/ledger/periods/{id}/unlock",
            axum::routing::post(unlock_handler),
        )
        .route(
            "/api/ledger/periods/{id}/close",
            axum::routing::post(close_period_handler),
        )
        .route(
            "/api/ledger/journal-entries",
            axum::routing::post(create_manual_entry),
        )
        .route(
            "/api/ledger/financial-facts/sum",
            axum::routing::post(financial_facts_sum_handler),
        )
        .route(
            "/api/ledger/financial-facts/{id}/supersede",
            axum::routing::post(supersede_fact_handler),
        )
        .route(
            "/api/ledger/bank-settlements",
            get(list_bank_settlements).post(create_bank_settlement),
        )
        .route(
            "/api/ledger/bank-settlements/from-paid-invoice",
            axum::routing::post(create_bank_settlement_from_paid_invoice),
        )
        .route(
            "/api/ledger/bank-settlements/{id}/settle",
            axum::routing::post(settle_bank_settlement),
        )
        .route(
            "/api/ledger/bank-settlements/sweep",
            axum::routing::post(sweep_bank_settlements),
        )
        .route(
            "/api/ledger/payroll-runs",
            get(list_payroll_runs).post(create_payroll_run),
        )
        .route(
            "/api/ledger/payroll-runs/synthesize",
            axum::routing::post(synthesize_payroll_run),
        )
        .route("/api/ledger/payroll-runs/{id}", get(get_payroll_run))
        .route(
            "/api/ledger/tax-filings",
            get(list_tax_filings).post(create_tax_filing),
        )
        .route("/api/ledger/tax-filings/{id}", get(get_tax_filing))
        .route(
            "/api/ledger/tax-filings/{id}/remit",
            axum::routing::post(remit_tax_filing),
        )
        .route(
            "/api/ledger/tax-accruals",
            axum::routing::post(create_tax_accrual),
        )
        .route(
            "/api/ledger/keg-deposit-settlements",
            axum::routing::post(create_keg_deposit_settlement),
        )
        // Graduated excise rates as registry data (brewery-fidelity Q4):
        // jurisdiction-keyed, effective-dated tier schedules the accrual
        // endpoint resolves instead of trusting a flat rule arg.
        .route(
            "/api/ledger/excise-rate-schedules",
            get(list_excise_rate_schedules).put(upsert_excise_rate_schedule),
        )
        .route("/api/ledger/tax-liability", get(tax_liability_summary))
        .route(
            "/api/ledger/revenue-schedules",
            axum::routing::post(create_revenue_schedule),
        )
        .route(
            "/api/ledger/deferred-revenue-runoff",
            get(deferred_revenue_runoff),
        )
        .route(
            "/api/ledger/cogs-recognized",
            axum::routing::post(cogs_recognized_handler),
        )
        .route(
            "/api/ledger/inventory-transferred",
            axum::routing::post(inventory_transferred_handler),
        )
        .route(
            "/api/ledger/inventory-capitalized",
            axum::routing::post(inventory_capitalized_handler),
        )
        // General accounts-payable bills (rent, utilities, …) — routed to a
        // GL debit account by `bill_category`, decoupled from inventory POs.
        .route("/api/ledger/bills", get(list_bills).post(create_bill))
        .route(
            "/api/ledger/bills/pay-run",
            axum::routing::post(batch_pay_bills),
        )
        .route("/api/ledger/bills/{id}/pay", axum::routing::post(pay_bill))
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            require_ledger_read,
        ))
        .with_state(shared)
}

// --- health ---------------------------------------------------------------

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-ledger-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// --- shared helpers -------------------------------------------------------

fn default_currency() -> String {
    "USD".to_string()
}

fn ledger_err(e: crate::error::LedgerError) -> Response {
    use crate::error::LedgerError;
    let status = match &e {
        LedgerError::UnknownAccount(_)
        | LedgerError::InvalidPayload { .. }
        | LedgerError::Unbalanced { .. }
        | LedgerError::LockedPeriod { .. }
        | LedgerError::UnknownFactKind(_) => StatusCode::BAD_REQUEST,
        LedgerError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string()).into_response()
}

fn storage_err(e: sqlx::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}
