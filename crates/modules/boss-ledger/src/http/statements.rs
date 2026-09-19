use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

type AccountRow = (Uuid, String, String, String, String, bool, Option<String>);
type TrialBalanceRowTuple = (String, String, String, String, i64, i64);

// --- chart of accounts ----------------------------------------------------

#[derive(Serialize)]
pub struct Account {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub normal_side: String,
    pub is_active: bool,
    /// The parent account's CODE — the key `seeds/chart_of_accounts.toml`
    /// declares a parent by, so `boss tenant export` can write the chart
    /// back in the file's shape (backlog e618f3ac). `None` at the root.
    pub parent: Option<String>,
}

pub(super) async fn list_accounts(State(state): State<Arc<LedgerApiState>>) -> Response {
    let rows: Result<Vec<AccountRow>, _> = sqlx::query_as(
        "SELECT a.id, a.code, a.name, a.kind, a.normal_side, a.is_active, p.code \
         FROM gl_accounts a LEFT JOIN gl_accounts p ON p.id = a.parent_id \
         WHERE a.is_active = true ORDER BY a.code",
    )
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => {
            let accounts: Vec<Account> = rows
                .into_iter()
                .map(
                    |(id, code, name, kind, normal_side, is_active, parent)| Account {
                        id,
                        code,
                        name,
                        kind,
                        normal_side,
                        is_active,
                        parent,
                    },
                )
                .collect();
            Json(accounts).into_response()
        }
        Err(e) => storage_err(e),
    }
}

// --- trial balance --------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct TrialBalanceQuery {
    /// ISO date; filters to entries posted on or before this date. If absent,
    /// returns cumulative totals across the entire ledger.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct TrialBalanceRow {
    pub account_code: String,
    pub account_name: String,
    pub kind: String,
    pub normal_side: String,
    pub debit_total_cents: i64,
    pub credit_total_cents: i64,
    /// Positive numbers represent the balance on the account's normal side
    /// (a positive revenue balance means net credits, the normal side for
    /// revenue accounts). Convention is: credit_total - debit_total for
    /// credit-normal accounts, debit_total - credit_total for debit-normal.
    pub balance_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

#[derive(Serialize)]
pub struct TrialBalanceResponse {
    pub as_of: Option<NaiveDate>,
    pub rows: Vec<TrialBalanceRow>,
    pub total_debits_cents: i64,
    pub total_credits_cents: i64,
    pub balanced: bool,
    pub currency: String,
}

pub(super) async fn trial_balance(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<TrialBalanceQuery>,
) -> Response {
    // One query joining lines to entries (for the as_of filter) and to
    // accounts (for name + normal_side).
    let rows_result: Result<Vec<TrialBalanceRowTuple>, _> = sqlx::query_as(
        "SELECT a.code, a.name, a.kind, a.normal_side, \
                    COALESCE(SUM(d.debit_cents), 0)::bigint, COALESCE(SUM(d.credit_cents), 0)::bigint \
             FROM gl_accounts a \
             LEFT JOIN gl_account_daily d ON d.account_id = a.id \
             WHERE ($1::date IS NULL OR d.posted_on <= $1) \
             GROUP BY a.code, a.name, a.kind, a.normal_side \
             ORDER BY a.code",
    )
    .bind(q.as_of)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows_result {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    let mut total_debits_cents: i64 = 0;
    let mut total_credits_cents: i64 = 0;
    let tb_rows: Vec<TrialBalanceRow> = rows
        .into_iter()
        .map(
            |(code, name, kind, normal_side, debit_total_cents, credit_total_cents)| {
                total_debits_cents += debit_total_cents;
                total_credits_cents += credit_total_cents;
                let balance_cents = if normal_side == "credit" {
                    credit_total_cents - debit_total_cents
                } else {
                    debit_total_cents - credit_total_cents
                };
                TrialBalanceRow {
                    account_code: code,
                    account_name: name,
                    kind,
                    normal_side,
                    debit_total_cents,
                    credit_total_cents,
                    balance_cents,
                    currency: "USD".to_string(),
                }
            },
        )
        .collect();

    Json(TrialBalanceResponse {
        as_of: q.as_of,
        rows: tb_rows,
        total_debits_cents,
        total_credits_cents,
        balanced: total_debits_cents == total_credits_cents,
        currency: "USD".to_string(),
    })
    .into_response()
}

// --- income statement -----------------------------------------------------

/// Chart convention (docs/architecture-decisions.md §Finance &
/// ledger): revenue = 4xxx,
/// COGS = 5xxx, operating expenses = 6xxx. The income statement
/// nets revenue − expenses over a date range; splitting the expense
/// side into COGS and operating expenses gives us gross profit as
/// an intermediate line (standard P&L shape).
const COGS_CODE_PREFIX: &str = "5";
const OPEX_CODE_PREFIX: &str = "6";

type StatementRowTuple = (String, String, String, i64, i64);

#[derive(Deserialize)]
pub(super) struct IncomeStatementQuery {
    /// ISO date; inclusive lower bound on entry posted_on. Defaults
    /// to the start of the current calendar year.
    from: Option<NaiveDate>,
    /// ISO date; inclusive upper bound on entry posted_on. Defaults
    /// to today.
    to: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct StatementLine {
    pub account_code: String,
    pub account_name: String,
    pub amount_cents: i64,
}

#[derive(Serialize)]
pub struct IncomeStatementResponse {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub revenue: Vec<StatementLine>,
    pub total_revenue_cents: i64,
    pub cogs: Vec<StatementLine>,
    pub total_cogs_cents: i64,
    pub gross_profit_cents: i64,
    pub operating_expenses: Vec<StatementLine>,
    pub total_operating_expenses_cents: i64,
    pub net_income_cents: i64,
    pub currency: String,
}

pub(super) async fn income_statement(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<IncomeStatementQuery>,
) -> Response {
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    let from = q.from.unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(today.year_ce().1 as i32, 1, 1).unwrap_or(today)
    });
    let to = q.to.unwrap_or(today);

    // Pull every revenue + expense account's activity in the period.
    // Revenue is credit-normal: amount = credit - debit. Expenses are
    // debit-normal: amount = debit - credit. The SQL returns totals;
    // we compute the signed amount per-row in Rust below.
    let rows_result: Result<Vec<StatementRowTuple>, _> = sqlx::query_as(
        "SELECT a.code, a.name, a.kind, \
                COALESCE(SUM(d.debit_cents), 0)::bigint, \
                COALESCE(SUM(d.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         LEFT JOIN gl_account_daily d ON d.account_id = a.id \
         WHERE a.kind IN ('revenue', 'expense') \
           AND ($1::date IS NULL OR d.posted_on >= $1) \
           AND ($2::date IS NULL OR d.posted_on <= $2) \
         GROUP BY a.code, a.name, a.kind \
         ORDER BY a.code",
    )
    .bind(from)
    .bind(to)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows_result {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    let mut revenue: Vec<StatementLine> = Vec::new();
    let mut cogs: Vec<StatementLine> = Vec::new();
    let mut opex: Vec<StatementLine> = Vec::new();

    for (code, name, kind, debit_total, credit_total) in rows {
        if kind == "revenue" {
            let amount = credit_total - debit_total;
            if amount == 0 {
                continue;
            }
            revenue.push(StatementLine {
                account_code: code,
                account_name: name,
                amount_cents: amount,
            });
        } else {
            let amount = debit_total - credit_total;
            if amount == 0 {
                continue;
            }
            let line = StatementLine {
                account_code: code.clone(),
                account_name: name,
                amount_cents: amount,
            };
            if code.starts_with(COGS_CODE_PREFIX) {
                cogs.push(line);
            } else if code.starts_with(OPEX_CODE_PREFIX) {
                opex.push(line);
            } else {
                // Unknown expense bucket — lump under operating so it
                // still lands in the report instead of silently
                // disappearing. Log-worthy if it ever fires.
                opex.push(line);
            }
        }
    }

    let total_revenue_cents: i64 = revenue.iter().map(|l| l.amount_cents).sum();
    let total_cogs_cents: i64 = cogs.iter().map(|l| l.amount_cents).sum();
    let total_operating_expenses_cents: i64 = opex.iter().map(|l| l.amount_cents).sum();
    let gross_profit_cents = total_revenue_cents - total_cogs_cents;
    let net_income_cents = gross_profit_cents - total_operating_expenses_cents;

    Json(IncomeStatementResponse {
        from,
        to,
        revenue,
        total_revenue_cents,
        cogs,
        total_cogs_cents,
        gross_profit_cents,
        operating_expenses: opex,
        total_operating_expenses_cents,
        net_income_cents,
        currency: "USD".to_string(),
    })
    .into_response()
}

// --- balance sheet --------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct BalanceSheetQuery {
    /// ISO date; balances are cumulative through this date inclusive.
    /// Defaults to today.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct BalanceSheetResponse {
    pub as_of: NaiveDate,
    pub assets: Vec<StatementLine>,
    pub total_assets_cents: i64,
    pub liabilities: Vec<StatementLine>,
    pub total_liabilities_cents: i64,
    /// Equity accounts plus a synthesized "Current-period net income"
    /// row so the sheet balances without requiring a year-end close.
    pub equity: Vec<StatementLine>,
    pub total_equity_cents: i64,
    /// `total_assets_cents - (total_liabilities_cents + total_equity_cents)`.
    /// Should be zero on a healthy ledger; surfaced so a drift in the
    /// chart or a miscoded account is visible at a glance.
    pub imbalance_cents: i64,
    pub balanced: bool,
    pub currency: String,
}

pub(super) async fn balance_sheet(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<BalanceSheetQuery>,
) -> Response {
    let as_of = q
        .as_of
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());

    // One query, kind-bucketed: walk every account, sum its
    // debit + credit totals as of the date, then categorize by
    // account.kind in Rust. A single unified query (no
    // "current-fiscal-year" date filter) avoids the
    // can-the-filter-be-wrong category of bug: every revenue +
    // expense account contributes to unclosed-period income
    // regardless of date, and period close (when implemented) zeroes
    // those accounts as part of its closing entries so subsequent BS
    // calls see only post-close activity.
    let rows: Result<Vec<StatementRowTuple>, _> = sqlx::query_as(
        "SELECT a.code, a.name, a.kind, \
                COALESCE(SUM(d.debit_cents), 0)::bigint, \
                COALESCE(SUM(d.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         LEFT JOIN gl_account_daily d ON d.account_id = a.id \
         WHERE ($1::date IS NULL OR d.posted_on <= $1) \
         GROUP BY a.code, a.name, a.kind \
         ORDER BY a.code",
    )
    .bind(as_of)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    let mut assets: Vec<StatementLine> = Vec::new();
    let mut liabilities: Vec<StatementLine> = Vec::new();
    let mut equity: Vec<StatementLine> = Vec::new();
    let mut unclosed_revenue_cents: i64 = 0;
    let mut unclosed_expense_cents: i64 = 0;

    for (code, name, kind, debit_total, credit_total) in rows {
        match kind.as_str() {
            "asset" => {
                let balance = debit_total - credit_total;
                if balance != 0 {
                    assets.push(StatementLine {
                        account_code: code,
                        account_name: name,
                        amount_cents: balance,
                    });
                }
            }
            "liability" => {
                let balance = credit_total - debit_total;
                if balance != 0 {
                    liabilities.push(StatementLine {
                        account_code: code,
                        account_name: name,
                        amount_cents: balance,
                    });
                }
            }
            "equity" => {
                let balance = credit_total - debit_total;
                // Equity lines always rendered (even at 0) so
                // the section header isn't empty on a fresh
                // tenant — matches the prior endpoint shape.
                equity.push(StatementLine {
                    account_code: code,
                    account_name: name,
                    amount_cents: balance,
                });
            }
            "revenue" => {
                unclosed_revenue_cents += credit_total - debit_total;
            }
            "expense" => {
                unclosed_expense_cents += debit_total - credit_total;
            }
            _ => {}
        }
    }

    // Unclosed revenue − unclosed expense = net income that
    // hasn't been swept into retained earnings yet. Synthesized
    // as a single equity line so A = L + E holds before the
    // first period close runs.
    let unclosed_net_income_cents = unclosed_revenue_cents - unclosed_expense_cents;
    if unclosed_net_income_cents != 0 {
        equity.push(StatementLine {
            account_code: "3100".to_string(),
            account_name: "Current-period net income".to_string(),
            amount_cents: unclosed_net_income_cents,
        });
    }

    let total_assets_cents: i64 = assets.iter().map(|l| l.amount_cents).sum();
    let total_liabilities_cents: i64 = liabilities.iter().map(|l| l.amount_cents).sum();
    let total_equity_cents: i64 = equity.iter().map(|l| l.amount_cents).sum();
    let imbalance_cents = total_assets_cents - (total_liabilities_cents + total_equity_cents);

    Json(BalanceSheetResponse {
        as_of,
        assets,
        total_assets_cents,
        liabilities,
        total_liabilities_cents,
        equity,
        total_equity_cents,
        imbalance_cents,
        balanced: imbalance_cents == 0,
        currency: "USD".to_string(),
    })
    .into_response()
}

// --- cash flow statement (indirect method) --------------------------------
//
// Indirect method: start from net income over the period, adjust for
// non-cash items and working-capital changes to arrive at cash from
// operations. Classify remaining activity into investing and
// financing sections. Cross-check: the three section subtotals sum to
// the net change in the cash account balance over the same period.
//
// Boss v1 notes:
// - Depreciation/amortization accounts don't exist yet. The non-cash
//   adjustments bucket ships empty. When depreciation lands as part of
//   cost-accounting, add it here (add-back to net income for ops, and
//   expose it as a separate "non-cash charges" subtotal).
// - No fixed-asset purchases, investments, debt issuance, or equity
//   issuance happen today. Investing + financing sections ship empty
//   but present, so the shape is stable for consumers. Populating
//   them is a drop-in SQL change once the accounts carry activity.
// - Working-capital classification uses code prefixes: AR = 1100,
//   AP = 2100, Inventory = 1200. More granular classification (per
//   account, configurable) lands with ASC 606 / fiscal-year start.

#[derive(Deserialize)]
pub(super) struct CashFlowQuery {
    /// ISO date; inclusive lower bound on activity. Defaults to the
    /// start of the current calendar year.
    from: Option<NaiveDate>,
    /// ISO date; inclusive upper bound on activity. Defaults to today.
    to: Option<NaiveDate>,
    /// Presentation method. Absent or `"indirect"` → the GL-attribution
    /// statement (operating/investing/financing sections derived from
    /// the journal). `"direct"` → the cash-events statement: the four
    /// real-world cash buckets summed straight off `financial_facts`.
    /// Both reconcile against the same cash-pool delta.
    method: Option<String>,
}

#[derive(Serialize)]
pub struct CashFlowStatementResponse {
    pub from: NaiveDate,
    pub to: NaiveDate,

    /// Net income over the period — same computation as the income
    /// statement handler. This is the starting point for the indirect
    /// method's operating section.
    pub net_income_cents: i64,

    /// Direct-method operating activities — one line per offset
    /// account that touched Cash via an operating-classified JE.
    /// Each line carries the signed cash impact (positive = cash
    /// inflow from that account, negative = outflow).
    pub operating_activities: Vec<StatementLine>,

    /// Legacy: indirect-method working-capital adjustments.
    /// Empty in direct-method mode (always returned, kept for
    /// wire compat with older SPA versions).
    pub working_capital_adjustments: Vec<StatementLine>,

    /// Non-cash adjustments (depreciation, amortization, impairments).
    /// Empty until the depreciation track ships; kept as a section
    /// so the frontend has a stable shape.
    pub non_cash_adjustments: Vec<StatementLine>,

    pub cash_from_operations_cents: i64,

    /// Investing activities (fixed-asset purchases, investments).
    /// Empty in v1.
    pub investing_activities: Vec<StatementLine>,
    pub cash_from_investing_cents: i64,

    /// Financing activities (debt, equity, dividends). Empty in v1.
    pub financing_activities: Vec<StatementLine>,
    pub cash_from_financing_cents: i64,

    pub net_change_in_cash_cents: i64,
    pub cash_start_cents: i64,
    pub cash_end_cents: i64,

    /// Cross-check: the three sections should sum to the actual
    /// change in the cash account balance over the same period.
    /// Non-zero indicates an account outside the working-capital
    /// buckets is driving cash in a way we haven't classified yet —
    /// worth surfacing so a bad posting rule or missing mapping is
    /// visible on first glance.
    pub reconciliation_gap_cents: i64,
    pub reconciled: bool,

    pub currency: String,
}

pub(super) async fn cash_flow_statement(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<CashFlowQuery>,
) -> Response {
    // `?method=direct` selects the cash-events presentation; anything
    // else (absent, "indirect") keeps the GL-attribution statement
    // below. Dispatched here rather than via a separate route so the
    // `from`/`to` window + clock-sourced defaults stay identical
    // across both presentations.
    if q.method.as_deref() == Some("direct") {
        return cash_flow_direct(&state, &q).await;
    }
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    let from = q.from.unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(today.year_ce().1 as i32, 1, 1).unwrap_or(today)
    });
    let to = q.to.unwrap_or(today);
    let day_before_from = from - chrono::Duration::days(1);

    // --- DIRECT METHOD ---
    //
    // Sum actual cash movements straight from the GL — every cash JE
    // attributed to the account on the OTHER side of the entry.
    // No reliance on net-income inferences; every line ties back to
    // specific journal entries an auditor can trace. The
    // reconciliation gap surfaces missing JEs immediately:
    //   beginning_cash + Σ(operating + investing + financing) =? ending_cash
    //
    // For multi-line JEs (rare in BOSS), each offset account gets a
    // share of the cash change proportional to its share of the
    // total offset side. 2-line JEs (the common case) get 100%
    // attribution to the single offset.
    // Cash pool: 1000 Cash + 1010 Cash in Transit. Movements
    // BETWEEN pool accounts (e.g. bank settlement transferring
    // 1010 → 1000) are internal transfers, not true cash flows
    // — excluded from operating/investing/financing. The original
    // economic transaction (DR 1010 / CR 1100 when a customer pays
    // an invoice) shows through naturally because we sum the
    // net cash POOL change per JE and attribute to non-pool offsets.
    let cash_pool: &[&str] = &["1000", "1010"];
    let cash_movements: Result<Vec<(String, String, String, i64)>, _> = sqlx::query_as(
        // Reads the pre-attributed gl_account_daily.cash_flow_cents rollup
        // (net cash attributed per offset account per day) instead of
        // re-running the per-entry pool->offset attribution over the whole
        // journal on every request. The pool accounts carry 0 here, so
        // they fall out; the per-entry proportional split + truncating
        // rounding happened when the rollup was built (live + rebuild).
        "SELECT a.code, a.kind, a.name, \
                COALESCE(SUM(d.cash_flow_cents), 0)::bigint AS attributed_cash \
         FROM gl_accounts a \
         JOIN gl_account_daily d ON d.account_id = a.id \
         WHERE d.posted_on BETWEEN $1 AND $2 \
         GROUP BY a.code, a.kind, a.name \
         ORDER BY a.code",
    )
    .bind(from)
    .bind(to)
    .fetch_all(&state.pool)
    .await;
    let cash_movements = match cash_movements {
        Ok(r) => r,
        Err(e) => return storage_err(e),
    };

    // Classify each (offset_code, offset_kind, attributed_cash)
    // into operating / investing / financing based on the offset
    // account's code prefix. Sub-categories aren't separately
    // labeled here (the line itself names the offset account, so
    // operators see "1100 Accounts Receivable: +$X" without
    // needing a separate sub-bucket).
    let mut operating: Vec<StatementLine> = Vec::new();
    let mut investing: Vec<StatementLine> = Vec::new();
    let mut financing: Vec<StatementLine> = Vec::new();
    for (code, _kind, name, attributed_cash) in &cash_movements {
        if *attributed_cash == 0 {
            continue;
        }
        let line = StatementLine {
            account_code: code.clone(),
            account_name: name.clone(),
            amount_cents: *attributed_cash,
        };
        // Codes follow the standard chart convention:
        //   1xxx assets, 2xxx liabilities, 3xxx equity,
        //   4xxx revenue, 5xxx COGS, 6xxx opex.
        // Investing = fixed assets + long-lived investments (15xx, 16xx).
        // Financing = long-term debt (25xx) + equity (3xxx).
        // Everything else (AR, inventory, AP, taxes, opex, revenue)
        // is operating.
        let bucket: &mut Vec<StatementLine> = if code.starts_with("15") || code.starts_with("16") {
            &mut investing
        } else if code.starts_with("25") || code.starts_with('3') {
            &mut financing
        } else {
            &mut operating
        };
        bucket.push(line);
    }
    let cash_from_operations_cents: i64 = operating.iter().map(|l| l.amount_cents).sum();
    let cash_from_investing_cents: i64 = investing.iter().map(|l| l.amount_cents).sum();
    let cash_from_financing_cents: i64 = financing.iter().map(|l| l.amount_cents).sum();

    // Beginning + ending cash balances for reconciliation.
    let start_balances = match account_balances_as_of(&state.pool, day_before_from).await {
        Ok(b) => b,
        Err(e) => return storage_err(e),
    };
    let end_balances = match account_balances_as_of(&state.pool, to).await {
        Ok(b) => b,
        Err(e) => return storage_err(e),
    };
    // Reconciliation uses the cash POOL (1000 + 1010), not just
    // 1000. Excluding 1010 leaves transfers-in-flight stranded
    // and produces phantom reconciliation gaps.
    let cash_start_cents: i64 = cash_pool
        .iter()
        .map(|c| start_balances.balance_for(c))
        .sum();
    let cash_end_cents: i64 = cash_pool.iter().map(|c| end_balances.balance_for(c)).sum();
    let actual_cash_delta = cash_end_cents - cash_start_cents;
    let calculated_cash_delta =
        cash_from_operations_cents + cash_from_investing_cents + cash_from_financing_cents;
    let reconciliation_gap_cents = actual_cash_delta - calculated_cash_delta;

    // Net income from the income-statement P&L over the same
    // period. Previously hardcoded to 0 with a
    // "direct method has no net income" comment, which was
    // misleading — operators reading the cash flow alongside
    // P&L need net income for context even when the direct
    // method computes operating cash without it. Compute as
    // (revenue credits − expense debits) over the queried
    // period. Equivalent to what income_statement() returns; we
    // recompute here rather than nest endpoint calls.
    let net_income_cents: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM( \
             CASE \
                 WHEN a.code LIKE '4%' THEN d.credit_cents - d.debit_cents \
                 WHEN a.code LIKE '5%' OR a.code LIKE '6%' THEN -(d.debit_cents - d.credit_cents) \
                 ELSE 0 \
             END \
         ), 0)::BIGINT \
         FROM gl_account_daily d \
         JOIN gl_accounts a ON a.id = d.account_id \
         WHERE d.posted_on BETWEEN $1 AND $2 \
           AND (a.code LIKE '4%' OR a.code LIKE '5%' OR a.code LIKE '6%')",
    )
    .bind(from)
    .bind(to)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let working_capital_adjustments: Vec<StatementLine> = Vec::new();
    let non_cash_adjustments: Vec<StatementLine> = Vec::new();
    let investing_activities = investing;
    let financing_activities = financing;

    Json(CashFlowStatementResponse {
        from,
        to,
        net_income_cents,
        operating_activities: operating,
        working_capital_adjustments,
        non_cash_adjustments,
        cash_from_operations_cents,
        investing_activities,
        cash_from_investing_cents,
        financing_activities,
        cash_from_financing_cents,
        net_change_in_cash_cents: calculated_cash_delta,
        cash_start_cents,
        cash_end_cents,
        reconciliation_gap_cents,
        reconciled: reconciliation_gap_cents == 0,
        currency: "USD".to_string(),
    })
    .into_response()
}

// --- cash flow statement (direct method) ----------------------------------
//
// Direct method: instead of inferring operating cash from net income
// (the indirect presentation above), sum the actual cash MOVEMENTS the
// business records as financial facts. Four buckets, one fact-kind each:
//
//   cash_in_from_customers  ← Σ payload.amount_cents  WHERE kind = finance.payment.received
//   cash_out_to_vendors     ← Σ payload.amount_cents  WHERE kind = finance.bill.paid
//   cash_out_to_employees   ← Σ payload.net_cents     WHERE kind = finance.payroll.run
//   cash_out_to_authorities ← Σ payload.amount_cents  WHERE kind = finance.tax.remitted
//
// Field names verified against the posting rules + emit sites:
//   - finance.payment.received reads `amount_cents` (rules.rs payment_received).
//   - finance.bill.paid reads `amount_cents` (rules.rs bill_paid).
//   - finance.payroll.run carries `net_cents` = what actually leaves the
//     bank (rules.rs payroll_run credits 1000 by net; http.rs/payroll.rs
//     stamp `net_cents` onto the fact payload). `gross_cents` would be
//     labor expense, not cash out — net is the right field here.
//   - finance.tax.remitted reads `amount_cents` (rules.rs tax_remitted).
//
// net_change_in_cash = cash_in − (vendors + employees + authorities).
//
// Reconciliation against the GL (structural correctness check).
// The four buckets settle into the Cash POOL — account 1000 Cash plus
// 1010 Cash in Transit — not into 1000 alone. The reason is the
// two-phase collection path: finance.payment.received debits 1010
// (clearing), and a later finance.payment.settled sweeps 1010 → 1000.
// So `cash_in_from_customers` is a 1010 inflow while the three outflows
// credit 1000, and the only delta the four buckets jointly reconcile to
// is the (1000 + 1010) pool delta over the same window. (The task sketch
// names 1000 as the target and flags 1010 as "the clearing"; reconciling
// against the pool is that statement taken to its structural conclusion —
// matching the indirect handler, which reconciles against the pool for
// the identical reason.) We surface BOTH deltas plus the residual gap:
//   - gl_cash_pool_delta_cents : (1000+1010) net change — the reconcile target
//   - gl_cash_1000_delta_cents : 1000-only net change — context line
//   - reconciliation_gap_cents : pool_delta − net_change_in_cash
// A non-zero gap means cash moved the pool via a path outside the four
// buckets: the same-day `finance.invoice.paid` collection shortcut
// (DR 1000 / CR 1100) or an admin `finance.manual.entry` touching cash.
// Surfaced rather than hidden so a missing mapping is visible at a glance,
// exactly as the indirect statement's gap does.

#[derive(Serialize)]
pub struct DirectCashFlowResponse {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub method: &'static str,

    /// Σ `finance.payment.received` `amount_cents` over the window.
    /// Positive = cash collected from customers.
    pub cash_in_from_customers_cents: i64,
    /// Σ `finance.bill.paid` `amount_cents` over the window. Positive
    /// = cash paid out to vendors.
    pub cash_out_to_vendors_cents: i64,
    /// Σ `finance.payroll.run` `net_cents` over the window. Positive =
    /// net wages paid to employees (gross − withholdings).
    pub cash_out_to_employees_cents: i64,
    /// Σ `finance.tax.remitted` `amount_cents` over the window.
    /// Positive = cash paid out to tax authorities.
    pub cash_out_to_authorities_cents: i64,

    /// `cash_in_from_customers − (vendors + employees + authorities)`.
    pub net_change_in_cash_cents: i64,

    /// Net change in the cash POOL (1000 Cash + 1010 Cash in Transit)
    /// over the window, straight from the GL. This is the figure the
    /// four buckets reconcile against (see module comment).
    pub gl_cash_pool_delta_cents: i64,
    /// Net change in account 1000 alone — context only. Differs from
    /// the pool delta by the float sitting in 1010 at the window edges.
    pub gl_cash_1000_delta_cents: i64,
    /// `gl_cash_pool_delta_cents − net_change_in_cash_cents`. Non-zero
    /// = cash moved the pool outside the four tracked fact kinds
    /// (same-day `finance.invoice.paid`, manual cash entries).
    pub reconciliation_gap_cents: i64,
    pub reconciled: bool,

    pub currency: String,
}

async fn cash_flow_direct(state: &LedgerApiState, q: &CashFlowQuery) -> Response {
    // Source the window the same way the indirect handler does:
    // clock-sourced `today` (NOT CURRENT_DATE), default `from` = Jan 1
    // of the current calendar year, default `to` = today.
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    let from = q.from.unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(today.year_ce().1 as i32, 1, 1).unwrap_or(today)
    });
    let to = q.to.unwrap_or(today);

    // One pass over financial_facts: bucket the four cash-event kinds
    // by their respective payload amount field, filtered to the window
    // on `happened_on`. `(payload->>'field')::bigint` coerces the JSONB
    // text to an integer; COALESCE guards an empty bucket → 0. A
    // malformed/absent field on a matched fact would surface as a cast
    // error (storage_err) rather than silently zeroing — facts are
    // written by the rules above, so the field is always present in
    // practice.
    let buckets: Result<(i64, i64, i64, i64), _> = sqlx::query_as(
        "SELECT \
            COALESCE(SUM(CASE WHEN kind = 'finance.payment.received' \
                              THEN (payload->>'amount_cents')::bigint END), 0)::bigint, \
            COALESCE(SUM(CASE WHEN kind = 'finance.bill.paid' \
                              THEN (payload->>'amount_cents')::bigint END), 0)::bigint, \
            COALESCE(SUM(CASE WHEN kind = 'finance.payroll.run' \
                              THEN (payload->>'net_cents')::bigint END), 0)::bigint, \
            COALESCE(SUM(CASE WHEN kind = 'finance.tax.remitted' \
                              THEN (payload->>'amount_cents')::bigint END), 0)::bigint \
         FROM financial_facts \
         WHERE happened_on BETWEEN $1 AND $2 \
           AND kind IN ('finance.payment.received', 'finance.bill.paid', \
                        'finance.payroll.run', 'finance.tax.remitted')",
    )
    .bind(from)
    .bind(to)
    .fetch_one(&state.pool)
    .await;
    let (
        cash_in_from_customers,
        cash_out_to_vendors,
        cash_out_to_employees,
        cash_out_to_authorities,
    ) = match buckets {
        Ok(b) => b,
        Err(e) => return storage_err(e),
    };

    let net_change_in_cash = cash_in_from_customers
        - cash_out_to_vendors
        - cash_out_to_employees
        - cash_out_to_authorities;

    // GL reconciliation. Pool delta = (1000 + 1010) net change over the
    // same window; 1000-only delta as a context line. Both are
    // debit-normal cash accounts, so the signed change is
    // SUM(debit − credit) over entries posted in [from, to].
    let day_before_from = from - chrono::Duration::days(1);
    let start_balances = match account_balances_as_of(&state.pool, day_before_from).await {
        Ok(b) => b,
        Err(e) => return storage_err(e),
    };
    let end_balances = match account_balances_as_of(&state.pool, to).await {
        Ok(b) => b,
        Err(e) => return storage_err(e),
    };
    let pool_start = start_balances.balance_for("1000") + start_balances.balance_for("1010");
    let pool_end = end_balances.balance_for("1000") + end_balances.balance_for("1010");
    let gl_cash_pool_delta_cents = pool_end - pool_start;
    let gl_cash_1000_delta_cents =
        end_balances.balance_for("1000") - start_balances.balance_for("1000");
    let reconciliation_gap_cents = gl_cash_pool_delta_cents - net_change_in_cash;

    Json(DirectCashFlowResponse {
        from,
        to,
        method: "direct",
        cash_in_from_customers_cents: cash_in_from_customers,
        cash_out_to_vendors_cents: cash_out_to_vendors,
        cash_out_to_employees_cents: cash_out_to_employees,
        cash_out_to_authorities_cents: cash_out_to_authorities,
        net_change_in_cash_cents: net_change_in_cash,
        gl_cash_pool_delta_cents,
        gl_cash_1000_delta_cents,
        reconciliation_gap_cents,
        reconciled: reconciliation_gap_cents == 0,
        currency: "USD".to_string(),
    })
    .into_response()
}

/// Signed account balances (asset/liability/equity) as of a given
/// date. Used by the cash-flow handler to compute working-capital
/// changes between two points in time.
struct AccountBalances {
    by_code: std::collections::HashMap<String, i64>,
}

impl AccountBalances {
    fn balance_for(&self, code: &str) -> i64 {
        self.by_code.get(code).copied().unwrap_or(0)
    }
}

async fn account_balances_as_of(
    pool: &sqlx::PgPool,
    as_of: NaiveDate,
) -> Result<AccountBalances, sqlx::Error> {
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, a.kind, \
                COALESCE(SUM(d.debit_cents), 0)::bigint, \
                COALESCE(SUM(d.credit_cents), 0)::bigint \
         FROM gl_accounts a \
         LEFT JOIN gl_account_daily d ON d.account_id = a.id \
         WHERE a.kind IN ('asset', 'liability', 'equity') \
           AND ($1::date IS NULL OR d.posted_on <= $1) \
         GROUP BY a.code, a.kind",
    )
    .bind(as_of)
    .fetch_all(pool)
    .await?;

    let mut by_code = std::collections::HashMap::new();
    for (code, kind, debit, credit) in rows {
        let balance = match kind.as_str() {
            "asset" => debit - credit,
            "liability" | "equity" => credit - debit,
            _ => 0,
        };
        by_code.insert(code, balance);
    }
    Ok(AccountBalances { by_code })
}
