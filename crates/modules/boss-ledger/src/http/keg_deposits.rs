use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::Deserialize;
use uuid::Uuid;

use super::*;

// --- keg deposit settlements (93f936b9: full balance-sheet keg model) ------

/// Body for `POST /api/ledger/keg-deposit-settlements` — one reconciled
/// keg fleet, both legs at once. Posted by the dispatcher's
/// `ledger.keg_deposit.settle` handler when a `keg-return` packet
/// closes `completed`; the handler reads every field off the packet's
/// own steps (log-fleet-out: `kegs_out` + `deposit_cents` +
/// completion date; receive-returns: `kegs_returned` + `kegs_lost` +
/// completion date).
///
/// Two facts land in one transaction, each with its own date:
///
///   finance.keg_deposit.charged   @ shipped_on
///       DR 1000 Cash / CR 2400 Keg Deposits Payable
///   finance.keg_deposit.released  @ returned_on
///       DR 2400 / CR 1000 refund + CR 4150 forfeiture
///
/// so the ledger timeline carries the in-field window even though both
/// legs post at reconciliation. `job_id` is the idempotency root: the
/// facts key on `keg-charge-<job_id>` / `keg-release-<job_id>` through
/// the financial_facts `(kind, source_table, source_id)` unique index,
/// so a redelivered POST is a no-op 200.
///
/// Validation failures are 422, not 400: they are deterministic
/// request-data errors (a fleet whose counts don't conserve will not
/// conserve on redelivery either), and the dispatcher's house contract
/// Terms a 422 immediately instead of burning the NAK budget.
#[derive(Deserialize)]
pub(super) struct KegDepositSettlementBody {
    job_id: String,
    #[serde(default)]
    account_id: Option<String>,
    kegs_out: i64,
    kegs_returned: i64,
    kegs_lost: i64,
    deposit_cents: i64,
    shipped_on: NaiveDate,
    returned_on: NaiveDate,
}

fn unprocessable(msg: String) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response()
}

pub(super) async fn create_keg_deposit_settlement(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<KegDepositSettlementBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.job_id.is_empty() {
        return unprocessable("job_id must be non-empty".into());
    }
    if body.deposit_cents <= 0 {
        return unprocessable(format!(
            "deposit_cents must be positive (got {})",
            body.deposit_cents
        ));
    }
    if body.kegs_out <= 0 || body.kegs_returned < 0 || body.kegs_lost < 0 {
        return unprocessable(format!(
            "keg counts out of range: kegs_out={} kegs_returned={} kegs_lost={}",
            body.kegs_out, body.kegs_returned, body.kegs_lost
        ));
    }
    // The fleet conservation invariant, enforced at the door. The
    // posting rule re-checks it in-tx (the rebuild path has only the
    // rule), but by then the answer is a 400 the dispatcher would
    // retry; here it is a 422 it Terms on.
    if body.kegs_returned + body.kegs_lost != body.kegs_out {
        return unprocessable(format!(
            "conservation violated: kegs_returned ({}) + kegs_lost ({}) != kegs_out ({})",
            body.kegs_returned, body.kegs_lost, body.kegs_out
        ));
    }
    // A fleet cannot come back before it went out. (Same-day is legal:
    // replay order between the pair is pinned by `recorded_at` — see
    // the clock_timestamp stamp in `post_keg_fact` — so the charge
    // always re-posts before its release even when the dates tie.)
    if body.returned_on < body.shipped_on {
        return unprocessable(format!(
            "returned_on ({}) precedes shipped_on ({})",
            body.returned_on, body.shipped_on
        ));
    }

    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    let account_id = body.account_id.as_deref().unwrap_or("");
    let charge_id = format!("keg-charge-{}", body.job_id);
    let release_id = format!("keg-release-{}", body.job_id);

    // Charge leg — the deposit went on the books when the fleet
    // shipped. Payload carries `charge_id` + `shipped_on` because the
    // `ledger.keg_deposit.charged` projection rule extracts them
    // (`/charge_id`, `/shipped_on`) to reproduce this fact from
    // audit_log on rebuild.
    let charge_payload = serde_json::json!({
        "charge_id": charge_id,
        "job_id": body.job_id,
        "account_id": account_id,
        "kegs_out": body.kegs_out,
        "deposit_cents": body.deposit_cents,
        "shipped_on": body.shipped_on,
    });
    let charge_fact_id = match post_keg_fact(
        &mut tx,
        &stamp,
        "finance.keg_deposit.charged",
        "ledger.keg_deposit.charged",
        body.shipped_on,
        &charge_id,
        &charge_payload,
    )
    .await
    {
        Ok(id) => id,
        Err(r) => return r,
    };

    // Release leg — refund + forfeiture split, dated when the returns
    // were received.
    let release_payload = serde_json::json!({
        "release_id": release_id,
        "job_id": body.job_id,
        "account_id": account_id,
        "kegs_out": body.kegs_out,
        "kegs_returned": body.kegs_returned,
        "kegs_lost": body.kegs_lost,
        "deposit_cents": body.deposit_cents,
        "returned_on": body.returned_on,
    });
    let release_fact_id = match post_keg_fact(
        &mut tx,
        &stamp,
        "finance.keg_deposit.released",
        "ledger.keg_deposit.released",
        body.returned_on,
        &release_id,
        &release_payload,
    )
    .await
    {
        Ok(id) => id,
        Err(r) => return r,
    };

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "job_id": body.job_id,
            "charge_fact_id": charge_fact_id,
            "release_fact_id": release_fact_id,
        })),
    )
        .into_response()
}

/// Record + post one keg-deposit fact and, when this call inserted it,
/// the audit event the rebuild projects it back from. Same
/// record → post → event-gated-on-inserted shape as
/// `create_tax_accrual`.
async fn post_keg_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    stamp: &boss_core::publisher::EventStamp,
    fact_kind: &'static str,
    event_kind: &'static str,
    happened_on: NaiveDate,
    source_id: &str,
    payload: &serde_json::Value,
) -> Result<Uuid, Response> {
    let recorded = crate::events::record_fact_in_tx(
        tx,
        crate::events::FactWrite {
            kind: fact_kind,
            happened_on,
            payload,
            source_table: Some("keg_deposit_settlements"),
            source_id: Some(source_id),
            created_by: "ledger",
        },
    )
    .await
    .map_err(ledger_err)?;

    if recorded.inserted {
        // Pin the pair's replay order. `recorded_at` defaults to NOW(),
        // which is transaction-fixed — both legs of a settlement would
        // tie, and on a same-day fleet (happened_on also equal) the
        // rebuild's ORDER BY would fall through to the arbitrary id
        // tie-break, letting the release replay before the charge it
        // drains. `clock_timestamp()` advances within the transaction,
        // so charge < release in `recorded_at` always, and replay
        // re-posts them in the order they were booked.
        sqlx::query("UPDATE financial_facts SET recorded_at = clock_timestamp() WHERE id = $1")
            .bind(recorded.id)
            .execute(&mut **tx)
            .await
            .map_err(storage_err)?;
    }

    let fact = crate::types::FactRef {
        id: recorded.id,
        kind: fact_kind,
        happened_on,
        payload,
    };
    crate::postgres::post_fact_in_tx(tx, &fact)
        .await
        .map_err(ledger_err)?;

    if recorded.inserted {
        crate::events::record_ledger_event_in_tx(tx, stamp, event_kind, payload.clone())
            .await
            .map_err(ledger_err)?;
    }
    Ok(recorded.id)
}
