use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use super::*;

// --- bank settlements -----------------------------------------------------

/// Create a pending bank settlement. Emits `finance.payment.received` in the
/// same transaction so the two-phase journal posting and the projection row
/// can't drift. Idempotent on `id` — a repeat POST with the same id returns
/// the existing row without double-posting.
#[derive(Deserialize)]
pub(super) struct CreateBankSettlementBody {
    id: String,
    invoice_id: String,
    account_id: String,
    amount_cents: i64,
    currency: String,
    received_on: NaiveDate,
    bank_provider: String,
    payment_method: String,
    #[serde(default)]
    settle_in_days: Option<i64>,
}

#[derive(Serialize)]
struct BankSettlementView {
    id: String,
    invoice_id: String,
    received_on: NaiveDate,
    expected_settle_on: NaiveDate,
    settled_on: Option<NaiveDate>,
    amount_cents: i64,
    bank_provider: String,
    payment_method: String,
    status: String,
}

impl From<crate::bank_settlements::BankSettlement> for BankSettlementView {
    fn from(s: crate::bank_settlements::BankSettlement) -> Self {
        Self {
            id: s.id,
            invoice_id: s.invoice_id,
            received_on: s.received_on,
            expected_settle_on: s.expected_settle_on,
            settled_on: s.settled_on,
            amount_cents: s.amount_cents,
            bank_provider: s.bank_provider,
            payment_method: s.payment_method,
            status: s.status,
        }
    }
}

pub(super) async fn create_bank_settlement(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateBankSettlementBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.amount_cents <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            "amount_cents must be positive".to_string(),
        )
            .into_response();
    }
    let method = match crate::bank_settlements::PaymentMethod::parse(&body.payment_method) {
        Ok(m) => m,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    // Check for existing row first so idempotency skips the fact insert too.
    // Without this a repeated POST would duplicate the finance.payment.received
    // fact (the bank_settlements INSERT itself is already idempotent on id).
    if let Ok(Some(existing)) = crate::bank_settlements::get(&state.pool, &body.id).await {
        return Json(BankSettlementView::from(existing)).into_response();
    }

    // The row and its receive event commit together: `bank_settlements`
    // is a projection of `ledger.payment.received` (+
    // `ledger.payment.settled` for the status flip), so a row that
    // committed without its event would vanish on the next rebuild.
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    let created = match crate::bank_settlements::create_pending_in_tx(
        &mut tx,
        crate::bank_settlements::NewBankSettlement {
            id: &body.id,
            invoice_id: &body.invoice_id,
            received_on: body.received_on,
            amount_cents: body.amount_cents,
            bank_provider: &body.bank_provider,
            payment_method: method,
            settle_in_days: body.settle_in_days,
        },
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return ledger_err(e),
    };
    let settlement = created.settlement;

    // Gate the fact + event on the insert actually happening. The
    // `get` short-circuit above catches the ordinary repeat POST; this
    // catches the concurrent one, where the loser must not append a
    // second receive for a settlement the winner already logged.
    if !created.inserted {
        let _ = tx.rollback().await;
        return Json(BankSettlementView::from(settlement)).into_response();
    }

    // `expected_settle_on` + `bank_provider` ride the payload so the
    // row is reconstructable. The settle window is NOT re-derivable on
    // replay: `settle_in_days` overrides the method default, so
    // recomputing it would make the rebuilt row disagree with what
    // actually happened. This is the fact payload too — the projection
    // rule maps the event through wholesale, so live and rebuilt facts
    // stay byte-identical.
    let payload = serde_json::json!({
        "invoice_id": body.invoice_id,
        "account_id": body.account_id,
        "settlement_id": body.id,
        "received_on": body.received_on,
        "expected_settle_on": settlement.expected_settle_on,
        "amount_cents": body.amount_cents,
        "currency": body.currency,
        "bank_provider": body.bank_provider,
        "payment_method": body.payment_method,
    });
    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.payment.received",
            happened_on: body.received_on,
            payload: &payload,
            source_table: Some("bank_settlements"),
            source_id: Some(&body.id),
            // Matches the emitting service's event source ("ledger") —
            // the projection's created_by fallback — so rebuilt facts
            // are byte-identical to live ones.
            created_by: "ledger",
        },
    )
    .await
    {
        Ok(rec) => rec.id,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.payment.received",
        happened_on: body.received_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    // Outbox phase 2: the audit event commits with the fact + JE.
    {
        let stamp = super::event_stamp(&state, &user).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.payment.received",
            payload.clone(),
        )
        .await
        {
            return ledger_err(e);
        }
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    Json(BankSettlementView::from(settlement)).into_response()
}

/// Adapter shape for the `[counterparty.bank-ach]` re-emit. The
/// CounterpartyEngine wraps the trigger under `trigger` at each
/// hop, so by the time bank-ach (which listens on
/// `commerce.invoice.paid`, itself emitted by ar-aging) re-fires,
/// the original step.done.billing payload is buried at
/// `trigger.trigger`. The `step_id` there resolves to invoice id
/// `inv-step-{step_id}` (the commerce-sim-bridge's deterministic
/// minting). Amount / account / currency aren't plumbed through
/// the chain, so the handler reads them from the `invoices`
/// projection in the same DB it owns. Idempotent on the derived
/// id `bs-{invoice_id}`.
#[derive(Deserialize)]
pub(super) struct FromPaidInvoiceBody {
    /// Outer trigger — the `commerce.invoice.paid` event. Its
    /// own `trigger` field carries the original
    /// `step.done.billing` payload (job_id, step_id, kind, …).
    trigger: serde_json::Value,
    /// Static counterparty fields — payment method + bank.
    /// Field names match the brewery's tenant.toml block.
    channel: String,
    bank: String,
    /// Counterparty drain date. When absent (test path), fall
    /// back to today.
    #[serde(default, rename = "_day")]
    day: Option<NaiveDate>,
}

pub(super) async fn create_bank_settlement_from_paid_invoice(
    State(state): State<Arc<LedgerApiState>>,
    user: CurrentUser,
    Json(body): Json<FromPaidInvoiceBody>,
) -> Response {
    // Two legitimate drive shapes converge here, and the
    // deterministic `bs-{invoice_id}` below makes their double
    // delivery idempotent:
    //
    // 1. The sim-internal chain: ar-aging's emit carries
    //    `trigger = <step.done.billing>`; bank-ach's emit then
    //    carries `trigger = <ar-aging emit>` — step_id lives at
    //    `trigger.trigger.step_id`.
    // 2. The system webhook copy: #100 put `commerce.>` into the
    //    durable stream, which woke the (previously dead-air)
    //    forward-invoice-paid-to-webhook rule — the counterparty now
    //    ALSO receives the enriched invoice row itself, whose
    //    top-level `id` IS the invoice id and which carries no
    //    trigger lineage. Rejecting that shape 400'd the first
    //    post-#100 year run at sim-day 37.
    let invoice_id = if let Some(step_id) = body
        .trigger
        .get("trigger")
        .and_then(|t| t.get("step_id"))
        .and_then(|v| v.as_str())
    {
        format!("inv-step-{step_id}")
    } else if let Some(id) = body
        .trigger
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| id.starts_with("inv-"))
    {
        id.to_string()
    } else {
        return (
            StatusCode::BAD_REQUEST,
            "trigger carries neither trigger.step_id (counterparty chain) \
             nor an inv-* id (invoice-paid webhook copy)"
                .to_string(),
        )
            .into_response();
    };

    // Read account_id / amount_cents / currency from the
    // invoices projection. The commerce-sim-bridge already
    // wrote this row when the billing step completed; the
    // ar-aging counterparty's PUT /paid then flipped status.
    let row: Result<(String, i64, String), _> = sqlx::query_as(
        "SELECT account_id, amount_cents, currency \
         FROM invoices WHERE id = $1",
    )
    .bind(&invoice_id)
    .fetch_one(&state.pool)
    .await;
    let (account_id, amount_cents, currency) = match row {
        Ok(r) => r,
        Err(sqlx::Error::RowNotFound) => {
            // Invoice doesn't exist yet — return 200 to keep the
            // sim's error budget at zero; the rebuild path will
            // never see a journal entry for this settlement.
            // Could happen on out-of-order NATS delivery.
            return Json(serde_json::json!({
                "ok": false,
                "skipped": true,
                "invoice_id": invoice_id,
                "reason": "invoice not yet projected",
            }))
            .into_response();
        }
        Err(e) => return storage_err(e),
    };

    let received_on = body
        .day
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());

    let inner = CreateBankSettlementBody {
        // `bs-` prefix + invoice_id keeps the id deterministic
        // across replays so the bank_settlements upsert + fact
        // insert idempotency guards converge on a single row.
        id: format!("bs-{invoice_id}"),
        invoice_id,
        account_id,
        amount_cents,
        currency,
        received_on,
        bank_provider: body.bank,
        payment_method: body.channel,
        // 0-day settle: the counterparty already modeled the
        // bank's processing delay (the spec's `delay`), so the
        // settlement clears on the drain date itself.
        settle_in_days: Some(0),
    };
    create_bank_settlement(State(state), user, Json(inner)).await
}

#[derive(Deserialize)]
pub(super) struct SettleBody {
    /// Date the settlement cleared. Defaults to today.
    #[serde(default)]
    settled_on: Option<NaiveDate>,
}

pub(super) async fn settle_bank_settlement(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<SettleBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let stamp = super::event_stamp(&state, &user).await;
    match settle_one(&state.pool, &stamp, &id, body.settled_on).await {
        Ok(view) => Json(view).into_response(),
        Err(SettleFailure::NotFound) => {
            (StatusCode::NOT_FOUND, "not found".to_string()).into_response()
        }
        Err(SettleFailure::AlreadyFinal(status)) => (
            StatusCode::CONFLICT,
            format!("settlement is already {status}"),
        )
            .into_response(),
        Err(SettleFailure::Storage(e)) => storage_err(e),
        Err(SettleFailure::Ledger(e)) => ledger_err(e),
    }
}

#[derive(Deserialize)]
pub(super) struct SweepQuery {
    /// Date to sweep "up to". Defaults to today.
    #[serde(default)]
    as_of: Option<NaiveDate>,
}

#[derive(Serialize)]
struct SweepResponse {
    swept: usize,
    ids: Vec<String>,
}

pub(super) async fn sweep_bank_settlements(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<SweepQuery>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let as_of = q
        .as_of
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let due = match crate::bank_settlements::list_due_pending(&state.pool, as_of).await {
        Ok(v) => v,
        Err(e) => return ledger_err(e),
    };
    let stamp = super::event_stamp(&state, &user).await;
    let mut swept = Vec::with_capacity(due.len());
    for row in due {
        match settle_one(&state.pool, &stamp, &row.id, Some(as_of)).await {
            Ok(_) => swept.push(row.id),
            Err(SettleFailure::AlreadyFinal(_)) => {}
            Err(SettleFailure::NotFound) => {}
            Err(SettleFailure::Storage(e)) => return storage_err(e),
            Err(SettleFailure::Ledger(e)) => return ledger_err(e),
        }
    }
    Json(SweepResponse {
        swept: swept.len(),
        ids: swept,
    })
    .into_response()
}

enum SettleFailure {
    NotFound,
    AlreadyFinal(String),
    Storage(sqlx::Error),
    Ledger(crate::error::LedgerError),
}

async fn settle_one(
    pool: &PgPool,
    stamp: &boss_core::publisher::EventStamp,
    id: &str,
    settled_on: Option<NaiveDate>,
) -> Result<BankSettlementView, SettleFailure> {
    let existing = crate::bank_settlements::get(pool, id)
        .await
        .map_err(SettleFailure::Ledger)?
        .ok_or(SettleFailure::NotFound)?;
    if existing.status != "pending" {
        return Err(SettleFailure::AlreadyFinal(existing.status));
    }
    let on = settled_on.unwrap_or_else(|| stamp.timestamp.date_naive());

    let settled = crate::bank_settlements::mark_settled(pool, id, on)
        .await
        .map_err(SettleFailure::Ledger)?;

    let mut tx = pool.begin().await.map_err(SettleFailure::Storage)?;

    let payload = serde_json::json!({
        "invoice_id": settled.invoice_id,
        "settlement_id": settled.id,
        "settled_on": on,
        "amount_cents": settled.amount_cents,
        "bank_provider": settled.bank_provider,
        "payment_method": settled.payment_method,
    });
    let live_fact_id = crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.payment.settled",
            happened_on: on,
            payload: &payload,
            source_table: Some("bank_settlements"),
            source_id: Some(&settled.id),
            created_by: "ledger",
        },
    )
    .await
    .map_err(SettleFailure::Ledger)?
    .id;

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.payment.settled",
        happened_on: on,
        payload: &payload,
    };
    crate::postgres::post_fact_in_tx(&mut tx, &fact)
        .await
        .map_err(SettleFailure::Ledger)?;

    crate::events::record_ledger_event_in_tx(&mut tx, stamp, "ledger.payment.settled", payload)
        .await
        .map_err(SettleFailure::Ledger)?;

    tx.commit().await.map_err(SettleFailure::Storage)?;

    Ok(BankSettlementView::from(settled))
}

#[derive(Deserialize)]
pub(super) struct ListBankSettlementsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    invoice_id: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(super) async fn list_bank_settlements(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListBankSettlementsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, invoice_id, received_on, expected_settle_on, settled_on, \
                amount_cents, bank_provider, payment_method, status \
         FROM bank_settlements \
         WHERE ($1::TEXT IS NULL OR status = $1) \
           AND ($2::TEXT IS NULL OR invoice_id = $2) \
         ORDER BY received_on DESC, id \
         LIMIT $3",
    )
    .bind(q.status.as_deref())
    .bind(q.invoice_id.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };
    let items: Vec<BankSettlementView> = rows
        .into_iter()
        .map(|row| BankSettlementView {
            id: row.get("id"),
            invoice_id: row.get("invoice_id"),
            received_on: row.get("received_on"),
            expected_settle_on: row.get("expected_settle_on"),
            settled_on: row.get("settled_on"),
            amount_cents: row.get("amount_cents"),
            bank_provider: row.get("bank_provider"),
            payment_method: row.get("payment_method"),
            status: row.get("status"),
        })
        .collect();
    Json(items).into_response()
}
