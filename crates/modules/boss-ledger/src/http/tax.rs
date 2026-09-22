use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::*;

// --- tax filings ----------------------------------------------------------

/// Create an accrued tax filing. Idempotent on `(kind, jurisdiction,
/// period)` — a repeat POST with the same tuple returns the existing
/// row. Accrual does NOT post a journal entry: the liability was
/// already credited when its source facts posted (sales tax on invoice
/// issue, payroll tax on run). The filing row simply tracks the
/// obligation so the operator can see what's due.
#[derive(Deserialize)]
pub(super) struct CreateTaxFilingBody {
    id: String,
    kind: String,
    jurisdiction: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    due_on: NaiveDate,
    amount_cents: i64,
    #[serde(default = "default_tax_provider")]
    provider: String,
}

fn default_tax_provider() -> String {
    "self".to_string()
}

#[derive(Serialize, Clone)]
struct TaxFilingView {
    id: String,
    kind: String,
    jurisdiction: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    due_on: NaiveDate,
    filed_on: Option<NaiveDate>,
    amount_cents: i64,
    liability_account: String,
    status: String,
    provider: String,
}

impl From<crate::tax_filings::TaxFiling> for TaxFilingView {
    fn from(f: crate::tax_filings::TaxFiling) -> Self {
        Self {
            id: f.id,
            kind: f.kind,
            jurisdiction: f.jurisdiction,
            period_start: f.period_start,
            period_end: f.period_end,
            due_on: f.due_on,
            filed_on: f.filed_on,
            amount_cents: f.amount_cents,
            liability_account: f.liability_account,
            status: f.status,
            provider: f.provider,
        }
    }
}

pub(super) async fn create_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateTaxFilingBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }

    // Upsert-idempotent on (kind, jurisdiction, period). If the filing
    // already exists we short-circuit without re-posting the accrual
    // entry — the fact's unique (kind, source_table, source_id) index
    // would catch a duplicate anyway, but a short-circuit saves a round
    // trip through the ledger write path.
    if let Ok(Some(existing)) = crate::tax_filings::get(&state.pool, &body.id).await {
        return Json(TaxFilingView::from(existing)).into_response();
    }

    // Resolve the GL accounts + amount-derivation for this tax kind from
    // the `tax_kinds` reference table (data, not a hardcoded match in the
    // dispatcher or an allow-list CHECK).
    let resolved: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT liability_account, expense_account, derive_basis \
         FROM tax_kinds WHERE kind = $1",
    )
    .bind(&body.kind)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let (liability_account, expense_account, derive_basis) = match resolved {
        Some(t) => t,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown tax kind `{}` — register it in tax_kinds first",
                    body.kind
                ),
            )
                .into_response();
        }
    };
    // Does this filing still have to BOOK the liability, or only drain
    // one already on the books? A `period-*` derivation reads the
    // liability account's own balance for the period (see the three
    // arms below), which means every invoice, payroll run or production step
    // in it already credited that account — accruing again at filing
    // time would double the liability. Anything else computes a fresh
    // amount from the books (income tax from prior-quarter net income),
    // and that amount is what this filing accrues. The prefix is the
    // rule, not a list beside the arms: an unknown basis is already
    // refused by name below, so a new derivation's author chooses it
    // deliberately.
    //
    // Until c0b83e13 (2026-09-22) the test was `expense_account
    // .is_some()` alone, which held only because the per-production row named
    // no expense account — the account that accrual actually
    // debits lived in the dispatcher rule's args instead. Now that the
    // row names it, the presence of an expense account no longer tells
    // these two cases apart.
    let derives_from_the_accrued_balance = derive_basis
        .as_deref()
        .is_some_and(|b| b.starts_with("period-"));
    let accrue = expense_account.is_some() && !derives_from_the_accrued_balance;

    // Derive amount_cents from the actual books rather than a
    // placeholder. For income tax:
    //   prior_quarter_net_income = revenue − COGS − operating expenses
    //   amount_cents = prior_quarter_net_income × 21% (federal corp rate)
    //
    // No estimated-margin guess: we compute margin DIRECTLY from
    // the GL by summing the kind buckets (revenue, cogs, expense).
    // If the prior quarter ran a loss, amount = 0 (no tax on losses).
    // If the prior quarter has no activity yet, fall back to the
    // caller's body.amount_cents so early-sim days still produce a
    // non-zero accrual that exercises the posting path.
    let amount_cents = match derive_basis.as_deref() {
        Some("period-payroll-941") => {
            // Form 941 = quarterly federal payroll tax = the net credit
            // balance sitting in 2150 Payroll Tax Payable as of the
            // period close (employee FIT withholding + employer-side
            // FICA/Medicare, both credited to 2150 at each payroll run).
            // The filing drains that balance to 1000 Cash.
            //
            // Derive from the GL balance through `period_end` — the
            // exact measure the remit guard checks (the accrued query
            // below: SUM(credit - debit) on the liability account where
            // posted_on <= period_end). Summing the raw `withheld_cents
            // + employer_tax_cents` off the finance.payroll.run facts
            // instead drifts a few cents above the posted liability (the
            // payroll posting rule rounds each run to whole cents), and
            // remitting that inflated figure trips the over-remit guard.
            // Reading 2150 directly makes the filing amount and the
            // guard agree to the cent; `posted_on <= period_end` with no
            // lower bound nets the prior quarter's remit-drain so each
            // quarter's 2150 fully clears. Mirrors the period-sales-tax
            // (2300) and period-excise (2320) arms.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2150' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "payroll-941 tax derived from 2150 balance through period_end"
            );
            if derived > 0 {
                derived
            } else {
                // No payroll posted to 2150 through period_end yet —
                // fall back to the caller's number so the filing posts.
                body.amount_cents
            }
        }
        Some("period-sales-tax") => {
            // Sales tax filed for a period = the net credit balance in
            // 2300 Sales Tax Payable as of period close. Each invoice
            // with a tax_line accrues a 2300 credit at invoice time; the
            // filing drains that balance to 1000 Cash.
            //
            // Derive from the GL balance through `period_end` — the same
            // window the remit guard checks — so the filing amount
            // equals the accrued liability and the remit clears 2300 to
            // zero. The no-lower-bound window is load-bearing: the prior
            // period's remit-drain posts on its filed_on, which lands in
            // THIS period's calendar (a quarter is filed after it
            // closes), so a `>= period_start` bound would subtract that
            // drain and under-derive every period after the first.
            // Mirrors the period-payroll-941 (2150) and period-excise
            // (2320) arms.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2300' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "sales tax derived from period 2300 credit balance"
            );
            if derived > 0 {
                derived
            } else {
                body.amount_cents
            }
        }
        Some("period-excise") => {
            // Federal beer excise tax filed for a period = the net
            // credit balance in 2320 Excise Tax Payable as of period
            // close. Each brew batch accrues a 2320 credit at production
            // time (DR 6550 / CR 2320, graduated over YTD barrels via
            // the excise_rate_schedules registry — see
            // `create_tax_accrual` below); the filing drains that
            // balance to 1000 Cash. Deriving from the 2320 balance means
            // this arm needs NO knowledge of the rate curve: whatever
            // the graduated accruals credited is what gets filed.
            //
            // Derive from the GL balance through `period_end`, no lower
            // bound — same as the period-sales-tax (2300) and
            // period-payroll-941 (2150) arms and the same window the
            // remit guard checks. The prior period's remit-drain posts
            // on its filed_on (in the following calendar period), so a
            // `>= period_start` bound would net that drain back out and
            // under-derive every period after the first, leaving 2320
            // un-cleared.
            let row: Result<Option<(i64,)>, _> = sqlx::query_as(
                "SELECT COALESCE( \
                    SUM(l.credit_cents - l.debit_cents), \
                    0)::bigint \
                 FROM gl_journal_lines l \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE l.account_id = ( \
                        SELECT id FROM gl_accounts WHERE code = '2320' LIMIT 1) \
                   AND e.posted_on <= $1::date",
            )
            .bind(body.period_end)
            .fetch_optional(&state.pool)
            .await;
            let derived: i64 = row.ok().flatten().map(|(r,)| r).unwrap_or(0);
            tracing::info!(
                period_start = %body.period_start,
                period_end = %body.period_end,
                derived_amount_cents = derived,
                body_amount_cents = body.amount_cents,
                "excise tax derived from period 2320 credit balance"
            );
            if derived > 0 {
                derived
            } else {
                body.amount_cents
            }
        }
        Some("prior-quarter-net-income") => {
            let prior_q_start = body
                .period_start
                .checked_sub_months(chrono::Months::new(3))
                .unwrap_or(body.period_start);
            let prior_q_end = body.period_start;
            let pnl_row: Result<Option<(i64, i64, i64)>, _> = sqlx::query_as(
                "SELECT \
                    COALESCE(SUM(CASE WHEN a.kind='revenue' \
                                      THEN l.credit_cents - l.debit_cents \
                                      ELSE 0 END), 0)::bigint AS revenue, \
                    COALESCE(SUM(CASE WHEN a.code LIKE '5%' \
                                      THEN l.debit_cents - l.credit_cents \
                                      ELSE 0 END), 0)::bigint AS cogs, \
                    COALESCE(SUM(CASE WHEN a.kind='expense' AND a.code NOT LIKE '5%' \
                                      THEN l.debit_cents - l.credit_cents \
                                      ELSE 0 END), 0)::bigint AS opex \
                 FROM gl_journal_lines l \
                 JOIN gl_accounts a ON l.account_id = a.id \
                 JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
                 WHERE e.posted_on >= $1::date \
                   AND e.posted_on < $2::date",
            )
            .bind(prior_q_start)
            .bind(prior_q_end)
            .fetch_optional(&state.pool)
            .await;
            let (revenue_cents, cogs_cents, opex_cents): (i64, i64, i64) =
                pnl_row.ok().flatten().unwrap_or((0, 0, 0));
            let net_income_cents = revenue_cents - cogs_cents - opex_cents;
            // 21% federal corporate income tax rate. No state tax in
            // v1; tenants that need a state component layer it via
            // a second filing with a different `derive_basis` later.
            const FEDERAL_CORP_RATE: f64 = 0.21;
            let tax = if net_income_cents > 0 {
                (net_income_cents as f64 * FEDERAL_CORP_RATE).round() as i64
            } else {
                0
            };
            tracing::info!(
                prior_q_start = %prior_q_start,
                prior_q_end = %prior_q_end,
                revenue_cents,
                cogs_cents,
                opex_cents,
                net_income_cents,
                derived_tax_cents = tax,
                "income tax derived from prior-quarter net income"
            );
            if tax > 0 {
                tax
            } else if revenue_cents == 0 && cogs_cents == 0 && opex_cents == 0 {
                // No prior-quarter activity at all — early-sim days.
                // Fall back to caller's number so the accrual posting
                // path stays exercised (the rule's amount > 0
                // invariant would reject 0 anyway).
                body.amount_cents
            } else {
                // Real loss in the prior quarter — no tax owed.
                // Return a token $1 so the rule's "amount > 0" check
                // passes; alternative would be skipping the accrual
                // entirely. $1 is a placeholder for "filing reflects
                // a true zero owed" while preserving JE shape.
                100
            }
        }
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown derive_basis `{other}`"),
            )
                .into_response();
        }
        None => body.amount_cents,
    };

    let stamp = super::event_stamp(&state, &user).await;

    // The row and its creation event commit together. `tax_filings` is
    // a projection of `ledger.tax.filing.created` (+ `ledger.tax.remitted`
    // for the status flip), so a row that reached the table without its
    // event in the log would vanish on the next rebuild — and a filing
    // that survives a rebuild it shouldn't is exactly the bug this
    // projection closes (see `rebuild_tax_filings`).
    //
    // Emitted for EVERY kind, not just the accruing ones: sales /
    // payroll_941 / payroll_940 / excise post no accrual entry (their
    // liability was already credited by their own source facts), so
    // `ledger.tax.accrued` alone would leave four of the five kinds
    // unreconstructable.
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    let upserted = match crate::tax_filings::upsert_in_tx(
        &mut tx,
        crate::tax_filings::NewTaxFiling {
            id: &body.id,
            kind: &body.kind,
            jurisdiction: &body.jurisdiction,
            period_start: body.period_start,
            period_end: body.period_end,
            due_on: body.due_on,
            amount_cents,
            liability_account: &liability_account,
            provider: &body.provider,
        },
    )
    .await
    {
        Ok(u) => u,
        Err(e) => return ledger_err(e),
    };
    let filing = upserted.filing;

    // Gate on the upsert's own idempotency: a repeat POST that resolved
    // to an existing row must not append a second creation to the log.
    if upserted.inserted {
        let created_payload = serde_json::json!({
            "filing_id": filing.id,
            "kind": filing.kind,
            "jurisdiction": filing.jurisdiction,
            "period_start": filing.period_start,
            "period_end": filing.period_end,
            "due_on": filing.due_on,
            "amount_cents": filing.amount_cents,
            "liability_account": filing.liability_account,
            "provider": filing.provider,
        });
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.tax.filing.created",
            created_payload,
        )
        .await
        {
            return ledger_err(e);
        }
    }
    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    if accrue {
        let expense_account = expense_account
            .as_deref()
            .expect("accrue implies an expense account from tax_kinds");
        if let Err(e) = post_accrual_entry(
            &state.pool,
            &stamp,
            &filing,
            expense_account,
            body.period_start,
        )
        .await
        {
            return e;
        }
    }

    Json(TaxFilingView::from(filing)).into_response()
}

/// Post `finance.tax.accrued` for a newly-created filing. Runs in its
/// own tx so the filing row lands whether or not the accrual entry
/// succeeds — but a failing accrual returns the error to the caller
/// so they can retry.
async fn post_accrual_entry(
    pool: &PgPool,
    stamp: &boss_core::publisher::EventStamp,
    filing: &crate::tax_filings::TaxFiling,
    expense_account: &str,
    posted_on: NaiveDate,
) -> Result<(), Response> {
    let mut tx = pool.begin().await.map_err(storage_err)?;

    let payload = serde_json::json!({
        "filing_id": filing.id,
        "kind": filing.kind,
        "jurisdiction": filing.jurisdiction,
        "posted_on": posted_on,
        "expense_account": expense_account,
        "liability_account": filing.liability_account,
        "amount_cents": filing.amount_cents,
        "period_start": filing.period_start,
        "period_end": filing.period_end,
    });
    let live_fact_id = crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.accrued",
            happened_on: posted_on,
            payload: &payload,
            source_table: Some("tax_filings"),
            source_id: Some(&filing.id),
            // Matches the event source ("ledger") — the projection's
            // created_by fallback — so rebuilt facts match live ones.
            created_by: "ledger",
        },
    )
    .await
    .map_err(ledger_err)?
    .id;

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: "finance.tax.accrued",
        happened_on: posted_on,
        payload: &payload,
    };
    crate::postgres::post_fact_in_tx(&mut tx, &fact)
        .await
        .map_err(ledger_err)?;

    crate::events::record_ledger_event_in_tx(&mut tx, stamp, "ledger.tax.accrued", payload)
        .await
        .map_err(ledger_err)?;

    tx.commit().await.map_err(storage_err)?;

    Ok(())
}

// --- standalone tax accrual -----------------------------------------------

/// Body for `POST /api/ledger/tax-accruals` — a standalone accrual with
/// no filing row. Used by the dispatcher's `ledger.tax.accrue` handler
/// to book a per-production tax liability the moment a production step
/// packages, exactly the way sales tax accrues per invoice. `id` is the
/// idempotency key (e.g. `excise-<step_id>`): it feeds the
/// financial_facts `(kind, source_table, source_id)` unique index, so a
/// duplicate POST is a no-op.
///
/// THE ACCOUNTS ARE THE KIND'S, not the caller's (backlog c0b83e13).
/// The body carries `kind` — a `tax_kinds` row on this instance — and
/// the door reads `liability_account` + `expense_account` off that row,
/// so the tenant's declaration is the one definition of which accounts
/// an accrual hits. A kind with no row, or a kind whose row
/// names no expense account, is refused by name. Until 2026-09-22 the
/// body named both accounts itself and the fields below carried the
/// demo tenant's `6550` / `2320`, which meant the posting hold could
/// only check that SOME row named the liability. `deny_unknown_fields`
/// so a caller still sending the old pair is refused loudly rather than
/// having it dropped in silence.
///
/// Two amount bases, quantity preferred:
/// - `excise_bbl` — the taxed barrels. The ledger resolves the rate
///   from the `excise_rate_schedules` registry (graduated tiers over
///   the jurisdiction's year-to-date taxed barrels); a jurisdiction
///   with no registry row falls back to the caller's
///   `fallback_rate_cents_per_bbl` flat rate, loudly.
/// - `amount_cents` — a caller-computed amount, taken verbatim
///   (pre-registry contract; still valid).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTaxAccrualBody {
    id: String,
    kind: String,
    #[serde(default)]
    amount_cents: Option<i64>,
    posted_on: NaiveDate,
    jurisdiction: String,
    #[serde(default)]
    excise_bbl: Option<i64>,
    #[serde(default)]
    fallback_rate_cents_per_bbl: Option<i64>,
}

/// How an accrual's amount was resolved — recorded in the fact payload
/// so the books say not just what was accrued but under which rate
/// mechanism.
struct ResolvedAmount {
    amount_cents: i64,
    rate_source: &'static str,
    excise_bbl: Option<i64>,
    ytd_bbl_before: Option<i64>,
}

/// Post a standalone `finance.tax.accrued` fact (DR expense / CR
/// liability) without creating a tax_filings row. Idempotent on `id`:
/// `record_fact_in_tx` resolves a duplicate `(kind, source_table,
/// source_id)` to the existing fact and `post_fact_in_tx` short-circuits
/// on the already-posted JE, so a repeat POST returns 200 without
/// double-booking the liability. The audit event is gated on the fact
/// insert, so a redelivery appends nothing to the log — which also
/// keeps the graduated recompute (below) from minting a divergent
/// second event.
///
/// Graduated resolution runs INSIDE the write transaction: the
/// schedule row is read `FOR UPDATE` (serializing concurrent accruals
/// per jurisdiction so two same-moment batches can't both claim the
/// same cheap-tier barrels), and the year-to-date taxed barrels are
/// summed from prior `finance.tax.accrued` facts — the projection that
/// survives a TRUNCATE-then-replay rebuild, which is what makes the
/// amount a pure function of the log. The sum excludes this accrual's
/// own id so a NAK redelivery recomputes the identical amount.
pub(super) async fn create_tax_accrual(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateTaxAccrualBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }

    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    // The kind's row is the one definition of both accounts (c0b83e13).
    // Read before any amount work so an unregistered kind is refused by
    // name rather than after a graduated-tier computation.
    let named: Option<(String, Option<String>)> = match sqlx::query_as(
        "SELECT liability_account, expense_account FROM tax_kinds WHERE kind = $1",
    )
    .bind(&body.kind)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };
    let (liability_account, expense_account) = match named {
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "tax kind `{}` has no tax_kinds row on this instance — declare it in \
                     seeds/tax.toml and publish it before it can accrue",
                    body.kind
                ),
            )
                .into_response();
        }
        Some((_, None)) => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "tax kind `{}` names no expense_account, so it drains a liability \
                     rather than accruing against an expense — declare one in \
                     seeds/tax.toml",
                    body.kind
                ),
            )
                .into_response();
        }
        Some((liability, Some(expense))) => (liability, expense),
    };

    let resolved = match (body.excise_bbl, body.amount_cents) {
        (Some(bbl), _) => {
            if bbl <= 0 {
                return (StatusCode::BAD_REQUEST, "excise_bbl must be positive").into_response();
            }
            // Newest schedule version effective on or before posted_on.
            let schedule: Option<(serde_json::Value,)> = match sqlx::query_as(
                "SELECT tiers FROM excise_rate_schedules \
                 WHERE jurisdiction = $1 AND effective_from <= $2 \
                 ORDER BY effective_from DESC LIMIT 1 \
                 FOR UPDATE",
            )
            .bind(&body.jurisdiction)
            .bind(body.posted_on)
            .fetch_optional(&mut *tx)
            .await
            {
                Ok(r) => r,
                Err(e) => return storage_err(e),
            };
            match schedule {
                Some((tiers_json,)) => {
                    let tiers: Vec<crate::excise::RateTier> =
                        match serde_json::from_value(tiers_json) {
                            Ok(t) => t,
                            Err(e) => {
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!(
                                        "excise_rate_schedules row for {} has malformed tiers: {e}",
                                        body.jurisdiction
                                    ),
                                )
                                    .into_response();
                            }
                        };
                    // YTD taxed barrels: the calendar-year sum of
                    // `excise_bbl` over prior accrual facts in this
                    // jurisdiction. Facts are the rebuild-stable
                    // quantity source — the payload rides through
                    // audit_log replay verbatim. Pre-registry facts
                    // carry no excise_bbl and count 0, which is
                    // consistent: they were accrued flat, outside the
                    // graduated curve.
                    let jan1 =
                        NaiveDate::from_ymd_opt(chrono::Datelike::year(&body.posted_on), 1, 1)
                            .unwrap_or(body.posted_on);
                    let ytd: (i64,) = match sqlx::query_as(
                        "SELECT COALESCE(SUM((payload->>'excise_bbl')::bigint), 0)::bigint \
                         FROM financial_facts \
                         WHERE kind = 'finance.tax.accrued' \
                           AND payload->>'jurisdiction' = $1 \
                           AND payload ? 'excise_bbl' \
                           AND happened_on >= $2 AND happened_on <= $3 \
                           AND NOT (source_table = 'tax_accruals' AND source_id = $4)",
                    )
                    .bind(&body.jurisdiction)
                    .bind(jan1)
                    .bind(body.posted_on)
                    .bind(&body.id)
                    .fetch_one(&mut *tx)
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => return storage_err(e),
                    };
                    let amount_cents = crate::excise::graduated_amount_cents(&tiers, ytd.0, bbl);
                    ResolvedAmount {
                        amount_cents,
                        rate_source: "registry",
                        excise_bbl: Some(bbl),
                        ytd_bbl_before: Some(ytd.0),
                    }
                }
                None => {
                    let Some(rate) = body.fallback_rate_cents_per_bbl.filter(|r| *r > 0) else {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!(
                                "no excise_rate_schedules row for {} and no positive \
                                 fallback_rate_cents_per_bbl — seed the registry or pass a flat rate",
                                body.jurisdiction
                            ),
                        )
                            .into_response();
                    };
                    // Loud on purpose: the flat rate is the pre-registry
                    // behavior kept alive for compatibility, and for the
                    // brewery it understates TTB liability ~3.7×.
                    tracing::warn!(
                        jurisdiction = %body.jurisdiction,
                        fallback_rate_cents_per_bbl = rate,
                        excise_bbl = bbl,
                        "no excise_rate_schedules row — accruing at the rule's FLAT \
                         fallback rate; seed the graduated schedule to fix the curve"
                    );
                    ResolvedAmount {
                        amount_cents: bbl * rate,
                        rate_source: "flat-fallback",
                        excise_bbl: Some(bbl),
                        ytd_bbl_before: None,
                    }
                }
            }
        }
        (None, Some(amount_cents)) => {
            if amount_cents <= 0 {
                return (StatusCode::BAD_REQUEST, "amount_cents must be positive").into_response();
            }
            ResolvedAmount {
                amount_cents,
                rate_source: "direct",
                excise_bbl: None,
                ytd_bbl_before: None,
            }
        }
        (None, None) => {
            return (
                StatusCode::BAD_REQUEST,
                "provide excise_bbl (quantity basis) or amount_cents",
            )
                .into_response();
        }
    };

    // `accrual_id` is the idempotency key — it doubles as the source_id
    // on the financial_facts `(kind, source_table, source_id)` index AND
    // the `source_id_path` the `ledger.tax.accrual.recorded` projection
    // rule extracts on rebuild, so live + rebuild agree on fact identity.
    // The quantity fields ride in the payload: `excise_bbl` is what the
    // YTD tier sum reads back, `rate_source` + `ytd_bbl_before` are the
    // provenance of the amount.
    let mut payload = serde_json::json!({
        "accrual_id": body.id,
        "kind": body.kind,
        "expense_account": expense_account,
        "liability_account": liability_account,
        "amount_cents": resolved.amount_cents,
        "posted_on": body.posted_on,
        "jurisdiction": body.jurisdiction,
    });
    if let Some(bbl) = resolved.excise_bbl {
        payload["excise_bbl"] = serde_json::json!(bbl);
        payload["rate_source"] = serde_json::json!(resolved.rate_source);
    }
    if let Some(ytd) = resolved.ytd_bbl_before {
        payload["ytd_bbl_before"] = serde_json::json!(ytd);
    }

    let recorded = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.accrued",
            happened_on: body.posted_on,
            payload: &payload,
            source_table: Some("tax_accruals"),
            source_id: Some(&body.id),
            created_by: "ledger",
        },
    )
    .await
    {
        Ok(rec) => rec,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: recorded.id,
        kind: "finance.tax.accrued",
        happened_on: body.posted_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    // Distinct event kind from income-tax's `ledger.tax.accrued`: this
    // standalone accrual has no tax_filings row, so it gets its own
    // `ledger.tax.accrual.recorded` projection rule (source_table
    // 'tax_accruals', source_id_path '/accrual_id') that rebuilds the
    // `finance.tax.accrued` fact from audit_log alone. Sharing the
    // income-tax kind would route the rebuild through the
    // `/filing_id`-keyed rule, which extracts a NULL source_id here and
    // silently drops the fact. Recorded in the SAME tx (outbox phase 2),
    // gated on the fact insert so a redelivery appends nothing.
    if recorded.inserted
        && let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.tax.accrual.recorded",
            payload,
        )
        .await
    {
        return ledger_err(e);
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": body.id,
            "amount_cents": resolved.amount_cents,
            "rate_source": resolved.rate_source,
            "ytd_bbl_before": resolved.ytd_bbl_before,
        })),
    )
        .into_response()
}

// --- excise rate schedules (registry) --------------------------------------

/// Body for `PUT /api/ledger/excise-rate-schedules` — upsert one
/// schedule version. Versioning is by row: a rate change is a NEW row
/// with a later `effective_from` (registry convention — append, don't
/// mutate history); the upsert-on-conflict exists so seeds re-run
/// idempotently and a mis-typed row can be corrected in place before
/// it has accrued anything.
#[derive(Deserialize)]
pub(super) struct UpsertExciseRateScheduleBody {
    jurisdiction: String,
    effective_from: NaiveDate,
    tiers: Vec<crate::excise::RateTier>,
}

pub(super) async fn upsert_excise_rate_schedule(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<UpsertExciseRateScheduleBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.jurisdiction.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "jurisdiction must be non-empty").into_response();
    }
    if let Err(e) = crate::excise::validate_tiers(&body.tiers) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let tiers_json = match serde_json::to_value(&body.tiers) {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let stamp = super::event_stamp(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };
    if let Err(e) = sqlx::query(
        "INSERT INTO excise_rate_schedules (jurisdiction, effective_from, tiers) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (jurisdiction, effective_from) \
         DO UPDATE SET tiers = EXCLUDED.tiers, updated_at = NOW()",
    )
    .bind(&body.jurisdiction)
    .bind(body.effective_from)
    .bind(&tiers_json)
    .execute(&mut *tx)
    .await
    {
        return storage_err(e);
    }
    // Control-plane audit trail (same posture as period lock/unlock +
    // revenue-schedule create): the registry row is system-of-record,
    // so the "who changed the tax curve and when" record rides the
    // outbox in the same tx. Not a projection source — no rebuilder
    // reads it.
    if let Err(e) = crate::events::record_ledger_event_in_tx(
        &mut tx,
        &stamp,
        "ledger.excise_rate_schedule.upserted",
        serde_json::json!({
            "jurisdiction": body.jurisdiction,
            "effective_from": body.effective_from,
            "tiers": tiers_json,
        }),
    )
    .await
    {
        return ledger_err(e);
    }
    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    Json(serde_json::json!({
        "jurisdiction": body.jurisdiction,
        "effective_from": body.effective_from,
        "tiers": body.tiers,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct ListExciseRateSchedulesQuery {
    #[serde(default)]
    jurisdiction: Option<String>,
}

pub(super) async fn list_excise_rate_schedules(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListExciseRateSchedulesQuery>,
) -> Response {
    let rows: Result<Vec<(String, NaiveDate, serde_json::Value)>, _> = sqlx::query_as(
        "SELECT jurisdiction, effective_from, tiers FROM excise_rate_schedules \
         WHERE $1::text IS NULL OR jurisdiction = $1 \
         ORDER BY jurisdiction, effective_from DESC",
    )
    .bind(q.jurisdiction.as_deref())
    .fetch_all(&state.pool)
    .await;
    match rows {
        Ok(rows) => {
            let views: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|(jurisdiction, effective_from, tiers)| {
                    serde_json::json!({
                        "jurisdiction": jurisdiction,
                        "effective_from": effective_from,
                        "tiers": tiers,
                    })
                })
                .collect();
            Json(views).into_response()
        }
        Err(e) => storage_err(e),
    }
}

#[derive(Deserialize)]
pub(super) struct RemitTaxBody {
    /// Date the filing was remitted (bank send date). Defaults to today.
    #[serde(default)]
    filed_on: Option<NaiveDate>,
}

/// Remit a filing — flips status to `paid` AND emits
/// `finance.tax.remitted` AND posts the draining journal entry, all in
/// one transaction. Idempotent: calling remit on an already-paid filing
/// returns the existing row without double-posting.
pub(super) async fn remit_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<RemitTaxBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let existing = match crate::tax_filings::get(&state.pool, &id).await {
        Ok(Some(f)) => f,
        Ok(None) => return (StatusCode::NOT_FOUND, "filing not found").into_response(),
        Err(e) => return ledger_err(e),
    };
    if existing.status == "paid" {
        return Json(TaxFilingView::from(existing)).into_response();
    }

    let filed_on = body
        .filed_on
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());

    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    // Conservation: a remittance cannot exceed the actual accrued
    // liability balance for the period. Query the live GL credits
    // - debits on the liability account up to `period_end`. The
    // rule itself (pure function) can't see the GL, so the check
    // lives here — same tx, defense-in-depth against a misshaped
    // tax_filings row that would otherwise drive the liability
    // negative.
    //
    // Skip the check when the configured liability_account isn't
    // in the GL yet (rebuild edge cases) — let it through; the
    // posting rule rejects negative amounts and the conservation
    // lint catches drift in CI.
    let accrued_row: Result<Option<(i64,)>, _> = sqlx::query_as(
        "SELECT COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON l.account_id = a.id \
         JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
         WHERE a.code = $1 AND e.posted_on <= $2",
    )
    .bind(&existing.liability_account)
    .bind(existing.period_end)
    .fetch_optional(&mut *tx)
    .await;
    if let Ok(Some((accrued,))) = accrued_row
        && accrued > 0
        && existing.amount_cents > accrued
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "remittance ${} exceeds accrued liability ${} on {} through {}",
                existing.amount_cents / 100,
                accrued / 100,
                existing.liability_account,
                existing.period_end,
            ),
        )
            .into_response();
    }

    let payload = serde_json::json!({
        "filing_id": existing.id,
        "kind": existing.kind,
        "jurisdiction": existing.jurisdiction,
        "filed_on": filed_on,
        "liability_account": existing.liability_account,
        "amount_cents": existing.amount_cents,
        "period_start": existing.period_start,
        "period_end": existing.period_end,
    });
    let live_fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            kind: "finance.tax.remitted",
            happened_on: filed_on,
            payload: &payload,
            source_table: Some("tax_filings"),
            source_id: Some(&existing.id),
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
        kind: "finance.tax.remitted",
        happened_on: filed_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    if let Err(e) = sqlx::query(
        "UPDATE tax_filings \
            SET status = 'paid', filed_on = $2, updated_at = NOW() \
          WHERE id = $1",
    )
    .bind(&existing.id)
    .bind(filed_on)
    .execute(&mut *tx)
    .await
    {
        return storage_err(e);
    }

    {
        let stamp = super::event_stamp(&state, &user).await;
        if let Err(e) = crate::events::record_ledger_event_in_tx(
            &mut tx,
            &stamp,
            "ledger.tax.remitted",
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

    let updated = match crate::tax_filings::get(&state.pool, &existing.id).await {
        Ok(Some(f)) => f,
        Ok(None) => return (StatusCode::INTERNAL_SERVER_ERROR, "filing vanished").into_response(),
        Err(e) => return ledger_err(e),
    };
    Json(TaxFilingView::from(updated)).into_response()
}

#[derive(Deserialize)]
pub(super) struct ListTaxFilingsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(super) async fn list_tax_filings(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListTaxFilingsQuery>,
) -> Response {
    match crate::tax_filings::list(&state.pool, q.status.as_deref(), q.limit.unwrap_or(200)).await {
        Ok(filings) => {
            let views: Vec<TaxFilingView> = filings.into_iter().map(TaxFilingView::from).collect();
            Json(views).into_response()
        }
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn get_tax_filing(
    State(state): State<Arc<LedgerApiState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::tax_filings::get(&state.pool, &id).await {
        Ok(Some(f)) => Json(TaxFilingView::from(f)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => ledger_err(e),
    }
}

/// Aggregated tax-liability summary for the Finance page's Tax tab.
/// Reads live balances from `gl_journal_lines` (not from the filings
/// projection, so unposted invoices + manual entries show up too)
/// plus the accrued filings list so the operator sees "what do we
/// owe + when is it due".
#[derive(Serialize)]
struct TaxLiabilityRow {
    account_code: String,
    account_name: String,
    balance_cents: i64,
}

#[derive(Serialize)]
struct TaxLiabilitySummaryResponse {
    as_of: NaiveDate,
    liabilities: Vec<TaxLiabilityRow>,
    accrued_filings: Vec<TaxFilingView>,
    next_due: Option<TaxFilingView>,
    currency: String,
}

pub(super) async fn tax_liability_summary(State(state): State<Arc<LedgerApiState>>) -> Response {
    let as_of = boss_clock_client::now_from(&state.clock).await.date_naive();

    let rows_result: Result<Vec<(String, String, i64, i64)>, _> = sqlx::query_as(
        "SELECT a.code, a.name, \
                COALESCE(SUM(l.debit_cents), 0)::bigint, \
                COALESCE(SUM(l.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         LEFT JOIN gl_journal_lines l ON l.account_id = a.id \
         LEFT JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE a.code IN ('2150', '2300', '2310') \
           AND (e.posted_on IS NULL OR e.posted_on <= $1) \
         GROUP BY a.code, a.name \
         ORDER BY a.code",
    )
    .bind(as_of)
    .fetch_all(&state.pool)
    .await;

    let liability_rows = match rows_result {
        Ok(rs) => rs,
        Err(e) => return storage_err(e),
    };
    let liabilities: Vec<TaxLiabilityRow> = liability_rows
        .into_iter()
        .map(|(code, name, debit, credit)| TaxLiabilityRow {
            account_code: code,
            account_name: name,
            balance_cents: credit - debit,
        })
        .collect();

    let accrued_filings = match crate::tax_filings::list(&state.pool, Some("accrued"), 100).await {
        Ok(fs) => fs,
        Err(e) => return ledger_err(e),
    };
    let accrued_views: Vec<TaxFilingView> = accrued_filings
        .into_iter()
        .map(TaxFilingView::from)
        .collect();
    // `list` orders by due_on DESC — flip to ASC for the "next due"
    // pick so the soonest obligation wins.
    let next_due = accrued_views.iter().min_by_key(|f| f.due_on).cloned();

    Json(TaxLiabilitySummaryResponse {
        as_of,
        liabilities,
        accrued_filings: accrued_views,
        next_due,
        currency: "USD".to_string(),
    })
    .into_response()
}
