//! Posting rules — pure functions from financial facts to balanced journal
//! entry drafts.
//!
//! A `RuleSet` is the versioned interpretation of the fact log. RuleSet v1
//! covers the four fact kinds that v1a started emitting:
//!
//! - `finance.invoice.issued` — AR: debit A/R, credit revenue by category
//! - `finance.invoice.paid` — AR collection (same-day settle shortcut):
//!   debit Cash, credit A/R. Kept for callers that don't need the
//!   two-phase bank float; new emitters should prefer the pair below.
//! - `finance.payment.received` — two-phase payment part 1: debit Cash in
//!   Transit, credit A/R. The bank hasn't cleared the funds yet.
//! - `finance.payment.settled` — two-phase payment part 2: debit Cash,
//!   credit Cash in Transit. The funds have landed in the operating
//!   account. Emitted by the bank-clearing generator when a pending
//!   settlement's expected date arrives.
//! - `finance.bill.approved` — AP: debit Inventory (or expense), credit A/P
//! - `finance.bill.paid` — AP payment: debit A/P, credit Cash
//! - `finance.payroll.run` — biweekly payroll: compound entry with
//!   DR 6100 gross + DR 6400 employer taxes, CR 2150 withholdings +
//!   employer taxes, CR 1000 net paid. The payload is aggregated
//!   per-run (per-employee detail lives in `payroll_run_lines`).
//! - `finance.tax.accrued` — tax authorities generator books an
//!   estimated liability for tax kinds that don't have a continuous
//!   source of credits (income tax). DR an expense account (6500
//!   today) / CR a liability account (2310). Paired with
//!   `finance.tax.remitted` on the filing's due date so the liability
//!   never carries a stale balance.
//! - `finance.tax.remitted` — tax authorities generator drains a tax
//!   liability (2300 sales, 2310 income, 2150 payroll) back into 1000
//!   Cash when a filing's due date arrives. Payload carries the
//!   filing id + jurisdiction + liability account so the memo and
//!   entry stay traceable.
//! - `finance.keg_deposit.charged` — a keg fleet ships with a
//!   refundable deposit: DR 1000 Cash / CR 2400 Keg Deposits Payable.
//!   The liability stands while the fleet is in the field.
//! - `finance.keg_deposit.released` — the fleet reconciles: DR 2400
//!   drains the FULL deposit; the returned share refunds (CR 1000) and
//!   the lost share forfeits to income (CR 4150). Requires
//!   `kegs_returned + kegs_lost == kegs_out` — the fleet conservation
//!   invariant — so a malformed count can never book a partial drain.
//! - `finance.manual.entry` — pass-through: admin-authored adjusting or
//!   reversing entries carry their own lines in the payload
//!
//! Adding a rule means advancing to the next RuleSet version and writing a
//! new row in `gl_rule_versions`. Old entries keep their original version
//! stamp so locked periods stay stable.
//!
//! Amounts are integer cents throughout. Rules read `amount_cents` from
//! `financial_facts.payload` and emit cents into `JournalLineDraft`.

use crate::error::LedgerError;
use crate::types::{FactRef, JournalEntryDraft, JournalLineDraft};

/// A versioned posting interpretation. Same fact in + same version → same
/// entry out, by construction (the whole trait must be pure).
pub trait RuleSet: Send + Sync {
    fn version(&self) -> i32;
    fn evaluate(&self, fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError>;
}

/// Fact kinds recorded in `financial_facts` for dedup/audit provenance
/// but that deliberately drive NO journal entry. The posting path
/// (`post_fact_in_tx`) early-returns on these BEFORE evaluating a
/// RuleSet, so they never reach the `UnknownFactKind` arm and never emit
/// a (would-be empty / double-posting) JE.
///
/// `finance.inventory.received` is the goods-receipt proof-fact: it
/// gates a redelivered `on_hand += qty` and records the receipt, but its
/// DR-1300 rides the idempotent bill-approval path, so a GL leg here
/// would double-post. It is reconstructable from the log (the inert pass
/// in `rebuild_facts.rs`) yet stays GL-inert — hence both off the
/// `gl_fact_projection_rules` registry AND skipped here.
pub fn is_gl_inert(kind: &str) -> bool {
    matches!(kind, "finance.inventory.received")
}

/// Top-level evaluation — delegates to the ruleset and validates the
/// balance invariant. Callers only use this function, not the trait
/// directly.
pub fn evaluate(
    ruleset: &dyn RuleSet,
    fact: &FactRef<'_>,
) -> Result<JournalEntryDraft, LedgerError> {
    let draft = ruleset.evaluate(fact)?;
    if !draft.is_balanced() {
        return Err(LedgerError::Unbalanced {
            kind: fact.kind.to_string(),
            debits: draft.total_debits(),
            credits: draft.total_credits(),
        });
    }
    Ok(draft)
}

/// The active BOSS RuleSet — a single ruleset covering every
/// financial fact kind: deferred-revenue-aware invoice issuance +
/// periodic recognition (`finance.revenue.recognized`), AR/AP
/// lifecycle, payroll, tax accrual + remittance, COGS, inventory
/// transfer, and period close.
///
/// A future rule-shape change introduces a sibling RuleSet
/// alongside this one and bumps `version()`; the
/// `gl_journal_entries.rule_version_id` column lets historical
/// rows stay pinned to whatever ruleset produced them.
pub struct BossRuleSet;

impl RuleSet for BossRuleSet {
    fn version(&self) -> i32 {
        1
    }

    fn evaluate(&self, fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
        match fact.kind {
            "finance.invoice.issued" => invoice_issued(fact),
            "finance.invoice.paid" => invoice_paid(fact),
            "finance.invoice.written_off" => invoice_written_off(fact),
            "finance.payment.received" => payment_received(fact),
            "finance.payment.settled" => payment_settled(fact),
            "finance.bill.approved" => bill_approved(fact),
            "finance.bill.paid" => bill_paid(fact),
            "finance.cogs.recognized" => cogs_recognized(fact),
            "finance.inventory.transferred" => inventory_transferred(fact),
            // Goods-receipt capitalization (DR 1300 raw / CR 2110 GR-IR).
            // Same caller-specified-account value-movement shape as a
            // transfer, so it shares the rule; the distinct fact kind keeps
            // the goods-receipt provenance separate from inter-tier transfers.
            "finance.inventory.capitalized" => inventory_transferred(fact),
            "finance.payroll.run" => payroll_run(fact),
            "finance.tax.accrued" => tax_accrued(fact),
            "finance.tax.remitted" => tax_remitted(fact),
            "finance.keg_deposit.charged" => keg_deposit_charged(fact),
            "finance.keg_deposit.released" => keg_deposit_released(fact),
            "finance.revenue.recognized" => revenue_recognized(fact),
            "finance.manual.entry" => manual_entry(fact),
            "finance.period.closed" => period_closed(fact),
            other => Err(LedgerError::UnknownFactKind(other.to_string())),
        }
    }
}

// --- Rule implementations -------------------------------------------------

/// Revenue category → revenue account code.
///
/// Loaded once at first call from a TOML map. The embedded
/// `seeds/revenue_accounts.toml` ships the default brewery +
/// device-shop mappings; per-tenant overrides land via the
/// `BOSS_LEDGER_REVENUE_ACCOUNTS_TOML` env var pointing at a
/// sibling file (replaces — does not merge with — the default).
/// Same data-as-data shape as D1 (step_types.toml) + D2
/// (phase_two_models.toml).
const REVENUE_ACCOUNTS_TOML: &str = include_str!("../seeds/revenue_accounts.toml");

fn revenue_accounts() -> &'static std::collections::HashMap<String, &'static str> {
    static CACHE: std::sync::OnceLock<std::collections::HashMap<String, &'static str>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        // Prefer the env-supplied override; fall back to the embedded default.
        let body = match std::env::var("BOSS_LEDGER_REVENUE_ACCOUNTS_TOML") {
            Ok(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "BOSS_LEDGER_REVENUE_ACCOUNTS_TOML unreadable; falling back to embedded defaults"
                );
                REVENUE_ACCOUNTS_TOML.to_string()
            }),
            Err(_) => REVENUE_ACCOUNTS_TOML.to_string(),
        };
        let parsed: std::collections::HashMap<String, String> =
            toml::from_str(&body).expect("revenue_accounts.toml must parse");
        parsed
            .into_iter()
            .map(|(k, v)| (k, &*Box::leak(v.into_boxed_str())))
            .collect()
    })
}

/// Public view of the revenue-category → account-code map. Built
/// off the same TOML the rules consume — commerce reads this to
/// invert the mapping for per-category COGS rollups. Callers must
/// NOT mutate the returned map; the lifetime is for the rest of the
/// process.
pub fn revenue_accounts_map() -> &'static std::collections::HashMap<String, &'static str> {
    revenue_accounts()
}

fn revenue_account_for(category: &str) -> Result<&'static str, LedgerError> {
    revenue_accounts()
        .get(category)
        .copied()
        .ok_or_else(|| LedgerError::InvalidPayload {
            kind: "finance.invoice.issued".to_string(),
            reason: format!("unknown revenue category `{category}`"),
        })
}

/// Bill category → GL debit account-code map. Same TOML-backed,
/// `OnceLock`-cached, env-overridable shape as `revenue_accounts()`.
/// Routes vendor-bill spend: asset categories capitalize
/// (inventory→1300, equipment→1500), expense categories hit the P&L
/// (rent→6200, utilities/insurance/…→6300). The embedded
/// `seeds/bill_accounts.toml` ships the defaults; per-tenant
/// overrides via `BOSS_LEDGER_BILL_ACCOUNTS_TOML` (replaces, not merges).
const BILL_ACCOUNTS_TOML: &str = include_str!("../seeds/bill_accounts.toml");

fn bill_accounts() -> &'static std::collections::HashMap<String, &'static str> {
    static CACHE: std::sync::OnceLock<std::collections::HashMap<String, &'static str>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let body = match std::env::var("BOSS_LEDGER_BILL_ACCOUNTS_TOML") {
            Ok(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "BOSS_LEDGER_BILL_ACCOUNTS_TOML unreadable; falling back to embedded defaults"
                );
                BILL_ACCOUNTS_TOML.to_string()
            }),
            Err(_) => BILL_ACCOUNTS_TOML.to_string(),
        };
        let parsed: std::collections::HashMap<String, String> =
            toml::from_str(&body).expect("bill_accounts.toml must parse");
        parsed
            .into_iter()
            .map(|(k, v)| (k, &*Box::leak(v.into_boxed_str())))
            .collect()
    })
}

fn bill_account_for(category: &str) -> Result<&'static str, LedgerError> {
    bill_accounts()
        .get(category)
        .copied()
        .ok_or_else(|| LedgerError::InvalidPayload {
            kind: "finance.bill.approved".to_string(),
            reason: format!("unknown bill category `{category}`"),
        })
}

/// Tax kind → liability account code. Same TOML-backed shape as
/// `revenue_account_for`; sim-bridge + ledger both resolve through
/// this single source of truth so the snapshot the bridge emits and
/// the account the rule posts to can never drift. Per-tenant
/// overrides via `BOSS_LEDGER_TAX_LIABILITY_ACCOUNTS_TOML`.
const TAX_LIABILITY_ACCOUNTS_TOML: &str = include_str!("../seeds/tax_liability_accounts.toml");

fn tax_liability_accounts() -> &'static std::collections::HashMap<String, &'static str> {
    static CACHE: std::sync::OnceLock<std::collections::HashMap<String, &'static str>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let body = match std::env::var("BOSS_LEDGER_TAX_LIABILITY_ACCOUNTS_TOML") {
            Ok(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "BOSS_LEDGER_TAX_LIABILITY_ACCOUNTS_TOML unreadable; falling back to embedded defaults"
                );
                TAX_LIABILITY_ACCOUNTS_TOML.to_string()
            }),
            Err(_) => TAX_LIABILITY_ACCOUNTS_TOML.to_string(),
        };
        let parsed: std::collections::HashMap<String, String> =
            toml::from_str(&body).expect("tax_liability_accounts.toml must parse");
        parsed
            .into_iter()
            .map(|(k, v)| (k, &*Box::leak(v.into_boxed_str())))
            .collect()
    })
}

/// Public so the sim-bridge can resolve through the same source of
/// truth the ledger's posting rules use; the bridge stamps the
/// account onto the snapshot payload and the rule cross-checks it.
pub fn liability_account_for(tax_kind: &str) -> Option<&'static str> {
    tax_liability_accounts().get(tax_kind).copied()
}

fn invoice_paid(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: invoice_memo(fact, "Invoice paid"),
        lines: vec![
            JournalLineDraft::debit("1000", amount, 0),
            JournalLineDraft::credit("1100", amount, 1),
        ],
    })
}

/// Write-off rule. Counterpart for `finance.invoice.written_off`
/// (projected from `commerce.invoice.written_off`, emitted by the
/// brewery `[counterparty.bad-debt-writeoff]` ~60 sim-days after
/// the past-due transition).
///
/// Posts DR 6700 Bad Debt Expense / CR 1100 A/R for the invoice's
/// `amount_cents`. Together with the original
/// `finance.invoice.issued` JE (DR 1100 / CR 4xxx) this drops the
/// receivable to zero on the GL while booking the uncollected
/// revenue as an expense — the standard real-world bookkeeping
/// gesture for uncollectable A/R.
fn invoice_written_off(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: invoice_memo(fact, "Invoice written off (uncollectable)"),
        lines: vec![
            JournalLineDraft::debit("6700", amount, 0),
            JournalLineDraft::credit("1100", amount, 1),
        ],
    })
}

fn payment_received(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Two-phase payment part 1. The account's payment has been recorded
    // but the funds haven't cleared through the bank. Debit Cash in
    // Transit (1010) rather than Cash — that flip is the bank-clearing
    // generator's job when it emits `finance.payment.settled`.
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: invoice_memo(fact, "Payment received (in transit)"),
        lines: vec![
            JournalLineDraft::debit("1010", amount, 0),
            JournalLineDraft::credit("1100", amount, 1),
        ],
    })
}

fn payment_settled(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Two-phase payment part 2. The bank has cleared the funds; flip
    // them from Cash in Transit into Cash. `settlement_id` on the
    // payload lets the memo carry the bank_settlements row back.
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    let memo = fact
        .payload
        .get("settlement_id")
        .and_then(|v| v.as_str())
        .map(|id| format!("Payment settled: {id}"))
        .or_else(|| invoice_memo(fact, "Payment settled"));
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo,
        lines: vec![
            JournalLineDraft::debit("1000", amount, 0),
            JournalLineDraft::credit("1010", amount, 1),
        ],
    })
}

fn payroll_run(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Compound journal entry aggregated per run. Payload shape:
    //   {
    //     "run_id": "pr-...",
    //     "gross_cents":        ...,  -- sum of employee gross
    //     "withheld_cents":     ...,  -- sum of employee withholdings
    //     "employer_tax_cents": ...,  -- employer-side tax + benefit cost
    //     "net_cents":          ...,  -- what actually leaves the bank
    //     "employee_count":     ...,
    //     "run_date":           "YYYY-MM-DD"
    //   }
    //
    // Entry:
    //   DR 6100 Payroll Expense      = gross_cents
    //   DR 6400 Payroll Taxes & Benef = employer_tax_cents
    //   CR 2150 Payroll Liability    = withheld_cents + employer_tax_cents
    //   CR 1000 Cash                 = net_cents
    //
    // Balance invariant: gross_cents + employer_tax_cents on the debit
    // side must equal (withheld + employer_tax) + net on the credit side.
    // Because net = gross - withheld by construction, this reduces to
    // gross + employer_tax == gross - withheld + withheld + employer_tax,
    // which is always true. The top-level `evaluate` still enforces
    // balance for defense-in-depth.
    let gross = cents_from_payload(fact.payload.get("gross_cents"))
        .ok_or_else(|| payload_err(fact.kind, "gross_cents missing"))?;
    let withheld = cents_from_payload(fact.payload.get("withheld_cents"))
        .ok_or_else(|| payload_err(fact.kind, "withheld_cents missing"))?;
    let employer_tax = cents_from_payload(fact.payload.get("employer_tax_cents"))
        .ok_or_else(|| payload_err(fact.kind, "employer_tax_cents missing"))?;
    if gross < 0 || withheld < 0 || employer_tax < 0 {
        return Err(payload_err(fact.kind, "amounts must be non-negative"));
    }
    if withheld > gross {
        return Err(payload_err(
            fact.kind,
            "withheld_cents cannot exceed gross_cents",
        ));
    }
    let net = gross - withheld;
    let liability = withheld + employer_tax;

    let mut lines = Vec::with_capacity(4);
    // Debits first (gross labor + employer-side taxes).
    lines.push(JournalLineDraft::debit("6100", gross, 0));
    if employer_tax > 0 {
        lines.push(JournalLineDraft::debit("6400", employer_tax, 1));
    }
    // Credits — liability holds what's owed to the tax authorities /
    // benefit providers; cash moves what actually paid the employees.
    if liability > 0 {
        lines.push(JournalLineDraft::credit("2150", liability, 2));
    }
    lines.push(JournalLineDraft::credit("1000", net, 3));

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: payroll_memo(fact),
        lines,
    })
}

fn payroll_memo(fact: &FactRef<'_>) -> Option<String> {
    let run_id = fact
        .payload
        .get("run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let count = fact
        .payload
        .get("employee_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some(format!("Payroll run {run_id} ({count} employees)"))
}

fn tax_accrued(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Book an estimated tax liability against an expense account.
    // Payload:
    //   {
    //     "filing_id":         "tf-income-US-FEDERAL-2026-Q1",
    //     "kind":              "income" | "sales" | "payroll_941" | "payroll_940",
    //     "jurisdiction":      "US-FEDERAL",
    //     "expense_account":   "6500",          -- must be debit-normal
    //     "liability_account": "2310",          -- must be credit-normal
    //     "amount_cents":      ...,
    //   }
    //
    // Entry:
    //   DR expense_account  amount_cents
    //   CR liability_account  amount_cents
    //
    // Used by the tax-authorities generator for income tax, which has
    // no continuous source of credits to the liability (sales tax
    // accrues via `tax_lines` on invoice issues; payroll tax accrues
    // as part of the payroll-run compound entry). Without this rule
    // the remit step would over-debit the liability and leave a
    // negative balance.
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    if amount <= 0 {
        return Err(payload_err(fact.kind, "amount_cents must be positive"));
    }
    let expense = fact
        .payload
        .get("expense_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "expense_account missing"))?;
    if !matches!(expense, "6400" | "6500" | "6550") {
        return Err(payload_err(
            fact.kind,
            &format!("expense_account `{expense}` not allowed"),
        ));
    }
    let liability = fact
        .payload
        .get("liability_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "liability_account missing"))?;
    if !matches!(liability, "2150" | "2300" | "2310" | "2320") {
        return Err(payload_err(
            fact.kind,
            &format!("liability_account `{liability}` not allowed"),
        ));
    }

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: tax_accrual_memo(fact),
        lines: vec![
            JournalLineDraft::debit(expense.to_string(), amount, 0),
            JournalLineDraft::credit(liability.to_string(), amount, 1),
        ],
    })
}

fn tax_accrual_memo(fact: &FactRef<'_>) -> Option<String> {
    let filing_id = fact
        .payload
        .get("filing_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let jurisdiction = fact
        .payload
        .get("jurisdiction")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let kind = fact
        .payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("tax");
    Some(format!("Tax accrued ({kind} {jurisdiction}): {filing_id}"))
}

fn tax_remitted(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Drain a tax liability back into Cash when the filing's due date
    // arrives. Payload:
    //   {
    //     "filing_id":          "tf-sales-US-CA-2026-03",
    //     "kind":               "sales" | "income" | "payroll_941" | "payroll_940",
    //     "jurisdiction":       "US-CA",
    //     "liability_account":  "2300" | "2310" | "2150",
    //     "amount_cents":       ...,
    //     "period_start":       "YYYY-MM-DD",
    //     "period_end":         "YYYY-MM-DD",
    //   }
    //
    // Entry:
    //   DR 2300/2310/2150  amount_cents
    //   CR 1000 Cash       amount_cents
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    if amount <= 0 {
        return Err(payload_err(fact.kind, "amount_cents must be positive"));
    }
    let liability = fact
        .payload
        .get("liability_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "liability_account missing"))?;
    if !matches!(liability, "2300" | "2310" | "2150" | "2320") {
        return Err(payload_err(
            fact.kind,
            &format!("liability_account `{liability}` not allowed"),
        ));
    }

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: tax_memo(fact),
        lines: vec![
            JournalLineDraft::debit(liability.to_string(), amount, 0),
            JournalLineDraft::credit("1000", amount, 1),
        ],
    })
}

fn tax_memo(fact: &FactRef<'_>) -> Option<String> {
    let filing_id = fact
        .payload
        .get("filing_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let jurisdiction = fact
        .payload
        .get("jurisdiction")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let kind = fact
        .payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("tax");
    Some(format!("Tax remitted ({kind} {jurisdiction}): {filing_id}"))
}

/// 2400 Keg Deposits Payable — refundable container deposits held
/// while a keg fleet is in the field (93f936b9, David's Q1 decision
/// 2026-08-22: "Let's go for the full balance-sheet model").
const KEG_DEPOSIT_LIABILITY_ACCOUNT: &str = "2400";
/// 4150 Keg Deposit Forfeitures — the lost-keg share of a released
/// deposit, recognized as income rather than left rotting in 2400 as
/// a liability owed to nobody.
const KEG_DEPOSIT_FORFEITURE_ACCOUNT: &str = "4150";

/// Read a required non-negative integer count from the payload.
fn keg_count(fact: &FactRef<'_>, key: &str) -> Result<i64, LedgerError> {
    let n = fact
        .payload
        .get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| payload_err(fact.kind, &format!("{key} missing")))?;
    if n < 0 {
        return Err(payload_err(fact.kind, &format!("{key} must be >= 0")));
    }
    Ok(n)
}

fn keg_deposit_charged(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // A keg fleet ships with a refundable deposit. Payload:
    //   {
    //     "charge_id":     "keg-charge-<job_id>",   -- idempotency/provenance
    //     "job_id":        "<keg-return job id>",
    //     "account_id":    "account-00042",
    //     "kegs_out":      12,
    //     "deposit_cents": 36000,
    //     "shipped_on":    "YYYY-MM-DD",
    //   }
    //
    // Entry:
    //   DR 1000 Cash                   deposit_cents
    //   CR 2400 Keg Deposits Payable   deposit_cents
    //
    // The deposit settles in cash alongside the shipment, deliberately
    // OUTSIDE the A/R subledger — same documented shortcut as
    // `finance.invoice.paid`'s "we don't model bank float": a bare CR
    // to 1100 with no invoice row would drift the A/R control account
    // from the invoices projection, and no payment would ever clear a
    // bare deposit debit. `finance.keg_deposit.released` is the only
    // drain, so 2400's standing balance IS the deposits of fleets
    // still in the field.
    let deposit = cents_from_payload(fact.payload.get("deposit_cents"))
        .ok_or_else(|| payload_err(fact.kind, "deposit_cents missing"))?;
    if deposit <= 0 {
        return Err(payload_err(fact.kind, "deposit_cents must be positive"));
    }
    let kegs_out = keg_count(fact, "kegs_out")?;
    if kegs_out == 0 {
        return Err(payload_err(fact.kind, "kegs_out must be positive"));
    }

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: keg_deposit_memo(fact, "charged", &format!("{kegs_out} kegs out")),
        lines: vec![
            JournalLineDraft::debit("1000", deposit, 0),
            JournalLineDraft::credit(KEG_DEPOSIT_LIABILITY_ACCOUNT, deposit, 1),
        ],
    })
}

fn keg_deposit_released(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // The fleet reconciled: kegs came back or are declared lost.
    // Payload:
    //   {
    //     "release_id":    "keg-release-<job_id>",
    //     "job_id":        "<keg-return job id>",
    //     "account_id":    "account-00042",
    //     "kegs_out":      10,
    //     "kegs_returned": 7,
    //     "kegs_lost":     3,
    //     "deposit_cents": 30000,        -- the fleet's FULL deposit
    //     "returned_on":   "YYYY-MM-DD",
    //   }
    //
    // Entry (zero-amount lines omitted):
    //   DR 2400 Keg Deposits Payable      deposit_cents
    //   CR 1000 Cash                      deposit × returned / out
    //   CR 4150 Keg Deposit Forfeitures   the remainder
    //
    // Conservation is a precondition, not a hope: the rule REQUIRES
    // `kegs_returned + kegs_lost == kegs_out`. A payload that violates
    // it is a wrong input event (correctness-protocol.md) — booking it
    // anyway would either strand liability in 2400 forever or over-
    // drain it negative, both silently. The refund share floors on
    // integer division and forfeiture absorbs the remainder, so the
    // entry balances to the cent and the liability drains exactly.
    let deposit = cents_from_payload(fact.payload.get("deposit_cents"))
        .ok_or_else(|| payload_err(fact.kind, "deposit_cents missing"))?;
    if deposit <= 0 {
        return Err(payload_err(fact.kind, "deposit_cents must be positive"));
    }
    let kegs_out = keg_count(fact, "kegs_out")?;
    let kegs_returned = keg_count(fact, "kegs_returned")?;
    let kegs_lost = keg_count(fact, "kegs_lost")?;
    if kegs_out == 0 {
        return Err(payload_err(fact.kind, "kegs_out must be positive"));
    }
    if kegs_returned + kegs_lost != kegs_out {
        return Err(payload_err(
            fact.kind,
            &format!(
                "conservation violated: kegs_returned ({kegs_returned}) + kegs_lost \
                 ({kegs_lost}) != kegs_out ({kegs_out})"
            ),
        ));
    }

    // i128 intermediate so deposit × returned can't overflow i64; the
    // quotient is <= deposit by construction (returned <= out).
    let refund = ((deposit as i128 * kegs_returned as i128) / kegs_out as i128) as i64;
    let forfeited = deposit - refund;

    let mut lines = Vec::with_capacity(3);
    lines.push(JournalLineDraft::debit(
        KEG_DEPOSIT_LIABILITY_ACCOUNT,
        deposit,
        0,
    ));
    let mut sort: i16 = 1;
    if refund > 0 {
        lines.push(JournalLineDraft::credit("1000", refund, sort));
        sort += 1;
    }
    if forfeited > 0 {
        lines.push(JournalLineDraft {
            account_code: KEG_DEPOSIT_FORFEITURE_ACCOUNT.into(),
            debit_cents: 0,
            credit_cents: forfeited,
            memo: Some(format!("Forfeited: {kegs_lost} keg(s) lost")),
            sort_order: sort,
        });
    }

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: keg_deposit_memo(
            fact,
            "released",
            &format!("{kegs_returned} returned / {kegs_lost} lost of {kegs_out}"),
        ),
        lines,
    })
}

fn keg_deposit_memo(fact: &FactRef<'_>, verb: &str, detail: &str) -> Option<String> {
    let job_id = fact
        .payload
        .get("job_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let account_id = fact
        .payload
        .get("account_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    Some(format!(
        "Keg deposit {verb} ({detail}, {account_id}): {job_id}"
    ))
}

fn bill_approved(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    // Conservation: the bill JE's amount is the sum of its
    // `lines` array — one entry per PO line `(sku, qty,
    // unit_cost_cents)`. The payload's top-level `amount_cents`,
    // when present, is a defense-in-depth assertion (we reject
    // mismatches) — never the authoritative number. This pairs
    // with `finance.cogs.recognized` on consumption: every dollar
    // INTO 1300 traces back to a PO line; every dollar OUT
    // traces back to a consume event sized at the running avg
    // cost. There is no path in the system that adjusts 1300
    // from a lump.
    // Two payload shapes, both supported (the types.rs `BillLine` comment
    // is load-bearing here):
    //
    //  - Lined: `{"lines": [{"qty","unit_cost_cents",...}, ...]}` — the
    //    rule sums qty × unit_cost_cents across the array and (when
    //    `amount_cents` is also present) validates the lump against the
    //    breakdown. This is the shape future live posts should adopt.
    //
    //  - Lump: `{"amount_cents": N}` with no `lines` — every bundled
    //    fact today is this shape (the projection rule that produces
    //    finance.bill.approved from inventory.vendor_invoice.approved
    //    passes the source row through 1:1 and the source row has no
    //    per-line breakdown). Falls back to amount_cents directly.
    let amount: i64 = match fact.payload.get("lines").and_then(|v| v.as_array()) {
        Some(lines) => {
            let sum: i64 = lines
                .iter()
                .map(|li| {
                    let qty = li.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
                    let unit_cost = li
                        .get("unit_cost_cents")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    qty.saturating_mul(unit_cost)
                })
                .sum();
            if let Some(claimed) = cents_from_payload(fact.payload.get("amount_cents"))
                && claimed != sum
            {
                return Err(payload_err(
                    fact.kind,
                    &format!(
                        "amount_cents={claimed} disagrees with Σ(lines)={sum} \
                         — the bill's line breakdown is the source of truth"
                    ),
                ));
            }
            sum
        }
        None => cents_from_payload(fact.payload.get("amount_cents")).ok_or_else(|| {
            payload_err(
                fact.kind,
                "bill payload needs `lines` array or top-level `amount_cents`",
            )
        })?,
    };
    if amount <= 0 {
        return Err(payload_err(
            fact.kind,
            &format!("bill amount must be positive; got {amount}"),
        ));
    }
    // Bill category routes the debit, data-driven via
    // bill_accounts.toml: asset categories capitalize (inventory
    // → 1300, equipment → 1500 PP&E so the brewery's $14.5M-class
    // kettle lands as a fixed asset, not raw-materials inventory),
    // expense categories hit the P&L immediately (rent → 6200,
    // utilities/insurance/repairs → 6300). Absent → "inventory",
    // preserving the ingredient-restock pipeline shape. `asset_class`
    // is also accepted as an alias for `bill_category` so equipment
    // bills keep capitalizing.
    let category = fact
        .payload
        .get("bill_category")
        .or_else(|| fact.payload.get("asset_class"))
        .and_then(|v| v.as_str())
        .unwrap_or("inventory");
    let debit_account = bill_account_for(category)?;
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: bill_memo(fact, "Vendor bill approved"),
        lines: vec![
            JournalLineDraft::debit(debit_account, amount, 0),
            JournalLineDraft::credit("2100", amount, 1),
        ],
    })
}

/// Recognize COGS when finished product physically leaves inventory.
/// Driven by the `products.consume` side-effect handler, which has
/// the actual `qty × unit_cost_cents` for each consumed SKU rather
/// than the invoice-side "30% of revenue" shortcut the old
/// `invoice_issued` rule baked in. The fact's payload carries the
/// already-aggregated total and the chart codes; the rule itself is
/// a pure DR/CR pair.
///
/// Payload shape:
///   {
///     "total_cost_cents":   123456,          // required, positive
///     "cogs_account":       "5100",          // optional, default 5100
///     "inventory_account":  "1300",          // optional, default 1300
///     "memo":               "Job XYZ shipment cost"   // optional
///   }
fn cogs_recognized(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("total_cost_cents"))
        .ok_or_else(|| payload_err(fact.kind, "total_cost_cents missing"))?;
    if amount <= 0 {
        return Err(payload_err(
            fact.kind,
            "total_cost_cents must be positive (zero-cost consumption is a no-op the side-effect handler should suppress)",
        ));
    }
    let cogs_account = fact
        .payload
        .get("cogs_account")
        .and_then(|v| v.as_str())
        .unwrap_or("5100")
        .to_string();
    let inventory_account = fact
        .payload
        .get("inventory_account")
        .and_then(|v| v.as_str())
        .unwrap_or("1300")
        .to_string();
    let memo = fact
        .payload
        .get("memo")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "COGS recognized on product consumption".to_string());
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: Some(memo),
        lines: vec![
            JournalLineDraft::debit(cogs_account, amount, 0),
            JournalLineDraft::credit(inventory_account, amount, 1),
        ],
    })
}

/// Asset-to-asset inventory cost transfer. Used for the
/// production cost flow that Model B captures:
///   - raw materials → WIP (DR 1310 / CR 1300) when production
///     consumes ingredients
///   - WIP → finished goods (DR 1320 / CR 1310) when packaging
///     completes a batch
///
/// Semantically distinct from `finance.cogs.recognized` (which
/// moves value asset → expense at sale). Same shape, different
/// account roles in the payload.
///
/// Payload:
///   {
///     "total_cost_cents":  123456,                // required, positive
///     "debit_account":     "1310",                // required, asset
///     "credit_account":    "1300",                // required, asset
///     "memo":              "..."                  // optional
///   }
fn inventory_transferred(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("total_cost_cents"))
        .ok_or_else(|| payload_err(fact.kind, "total_cost_cents missing"))?;
    if amount <= 0 {
        return Err(payload_err(fact.kind, "total_cost_cents must be positive"));
    }
    let debit_account = fact
        .payload
        .get("debit_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "debit_account missing"))?
        .to_string();
    let credit_account = fact
        .payload
        .get("credit_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "credit_account missing"))?
        .to_string();
    let memo = fact
        .payload
        .get("memo")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("Inventory transferred {credit_account} → {debit_account}"));
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: Some(memo),
        lines: vec![
            JournalLineDraft::debit(debit_account, amount, 0),
            JournalLineDraft::credit(credit_account, amount, 1),
        ],
    })
}

fn bill_paid(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: bill_memo(fact, "Vendor bill paid"),
        lines: vec![
            JournalLineDraft::debit("2100", amount, 0),
            JournalLineDraft::credit("1000", amount, 1),
        ],
    })
}

/// Manual journal entry — the admin-authored adjusting or reversing entry.
/// Payload shape:
///
/// ```json
/// {
///   "memo": "Q1 adjustment",
///   "lines": [
///     { "account_code": "6300", "debit_cents": 12500, "memo": "..." },
///     { "account_code": "1000", "credit_cents": 12500 }
///   ]
/// }
/// ```
///
/// Unlike the other rules, this one doesn't derive lines from business data;
/// it just echoes what the operator wrote back into a draft. The balance
/// invariant is enforced by the top-level `evaluate` + the DB trigger.
fn manual_entry(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let lines_json = fact
        .payload
        .get("lines")
        .and_then(|v| v.as_array())
        .ok_or_else(|| payload_err(fact.kind, "missing lines array"))?;

    if lines_json.len() < 2 {
        return Err(payload_err(
            fact.kind,
            "manual entry needs at least 2 lines",
        ));
    }

    let mut lines = Vec::with_capacity(lines_json.len());
    for (idx, li) in lines_json.iter().enumerate() {
        let account_code = li
            .get("account_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "lines[].account_code missing"))?;
        let debit = cents_from_payload(li.get("debit_cents")).unwrap_or(0);
        let credit = cents_from_payload(li.get("credit_cents")).unwrap_or(0);
        if debit < 0 || credit < 0 {
            return Err(payload_err(
                fact.kind,
                "lines[] amounts must be non-negative",
            ));
        }
        if (debit == 0) == (credit == 0) {
            // Both zero or both nonzero — neither is legal for one line.
            return Err(payload_err(
                fact.kind,
                "each line must have exactly one of debit_cents or credit_cents",
            ));
        }
        let memo = li
            .get("memo")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        lines.push(JournalLineDraft {
            account_code: account_code.to_string().into(),
            debit_cents: debit,
            credit_cents: credit,
            memo,
            sort_order: idx as i16,
        });
    }

    let memo = fact
        .payload
        .get("memo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo,
        lines,
    })
}

/// Fiscal-year close — posts closing entries that zero revenue +
/// expense accounts and roll net income into Retained Earnings.
///
/// Payload shape:
/// ```json
/// {
///   "period_id":                 "<uuid of the yearly period>",
///   "period_end":                "2026-12-31",
///   "retained_earnings_account": "3000",
///   "revenue_lines":  [{ "account_code": "4100", "balance_cents": 123000 }, ...],
///   "expense_lines":  [{ "account_code": "6100", "balance_cents":  45000 }, ...]
/// }
/// ```
///
/// Each revenue account carries a credit balance (cents); we debit it
/// by that amount to zero it. Each expense account carries a debit
/// balance; we credit it by that amount to zero it. The net
/// difference (revenue_total − expense_total) is a single net entry
/// to `retained_earnings_account` — credit when the year was
/// profitable, debit when it was a loss. When the year is exactly
/// break-even the RE line is omitted (zero-amount lines are spec
/// noise) and the entry is balanced by construction.
///
/// Optional WIP-variance close-out: when the payload carries a
/// non-zero `wip_variance_cents` (the residual balance on the WIP
/// account as of the close — materials + absorption capitalized in,
/// produce-drains out), the entry also writes that residual off
/// against retained earnings so WIP starts the new year at zero:
///
/// ```json
/// { "wip_variance_cents": 25000, "wip_account": "1310" }
/// ```
///
/// A debit residual (under-drained WIP) posts DR RE / CR wip; a
/// credit residual (over-absorbed) posts the mirror. Deliberately a
/// separate RE line from the net-income roll — and NOT a posting to
/// the drained COGS/expense account, which would re-inflate it right
/// after its own zero line. `wip_account` rides in the payload (the
/// close endpoint owns the starter-chart "1310" default) so this
/// rule stays chart-agnostic, same contract as
/// `retained_earnings_account`; a non-zero variance without an
/// account is a malformed fact and is refused.
#[allow(unused_assignments)] // running sort_order counter; final increment intentionally unread
fn period_closed(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let retained_earnings = fact
        .payload
        .get("retained_earnings_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "missing retained_earnings_account"))?;

    // Balance lines: walk the two arrays and emit a DR/CR per account.
    let mut lines: Vec<JournalLineDraft> = Vec::new();
    let mut revenue_total: i64 = 0;
    let mut expense_total: i64 = 0;
    let mut sort: i16 = 0;

    let revenue_lines = fact
        .payload
        .get("revenue_lines")
        .and_then(|v| v.as_array())
        .ok_or_else(|| payload_err(fact.kind, "missing revenue_lines array"))?;
    for rl in revenue_lines {
        let account = rl
            .get("account_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "revenue_lines[].account_code missing"))?;
        let balance = cents_from_payload(rl.get("balance_cents"))
            .ok_or_else(|| payload_err(fact.kind, "revenue_lines[].balance_cents missing"))?;
        if balance < 0 {
            return Err(payload_err(
                fact.kind,
                "revenue_lines[].balance_cents must be non-negative",
            ));
        }
        if balance == 0 {
            continue;
        }
        revenue_total += balance;
        lines.push(JournalLineDraft {
            account_code: account.to_string().into(),
            debit_cents: balance,
            credit_cents: 0,
            memo: Some(format!("Year-end close: zero out {account}")),
            sort_order: sort,
        });
        sort += 1;
    }

    let expense_lines = fact
        .payload
        .get("expense_lines")
        .and_then(|v| v.as_array())
        .ok_or_else(|| payload_err(fact.kind, "missing expense_lines array"))?;
    for el in expense_lines {
        let account = el
            .get("account_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "expense_lines[].account_code missing"))?;
        let balance = cents_from_payload(el.get("balance_cents"))
            .ok_or_else(|| payload_err(fact.kind, "expense_lines[].balance_cents missing"))?;
        if balance == 0 {
            continue;
        }
        // Negative expense balance can happen in legitimate
        // cases — e.g. labor absorption (CR 6100 / DR 1310) over a
        // partial period without a matching payroll DR. Treat
        // negative the same way: write the OFFSET side so the
        // account zeros out at close. Sign goes into expense_total
        // so net_income reflects the negative expense as a benefit.
        expense_total += balance;
        if balance > 0 {
            lines.push(JournalLineDraft {
                account_code: account.to_string().into(),
                debit_cents: 0,
                credit_cents: balance,
                memo: Some(format!("Year-end close: zero out {account}")),
                sort_order: sort,
            });
        } else {
            // balance < 0 → DR side zeros it out
            lines.push(JournalLineDraft {
                account_code: account.to_string().into(),
                debit_cents: -balance,
                credit_cents: 0,
                memo: Some(format!(
                    "Year-end close: zero out {account} (was credit-balance)"
                )),
                sort_order: sort,
            });
        }
        sort += 1;
    }

    // Net income → Retained Earnings. Positive NI (revenue > expense)
    // credits RE; negative (loss) debits RE. Break-even yields no RE
    // line at all.
    let net_income = revenue_total - expense_total;
    if net_income > 0 {
        lines.push(JournalLineDraft {
            account_code: retained_earnings.to_string().into(),
            debit_cents: 0,
            credit_cents: net_income,
            memo: Some("Year-end close: net income to retained earnings".to_string()),
            sort_order: sort,
        });
        sort += 1;
    } else if net_income < 0 {
        lines.push(JournalLineDraft {
            account_code: retained_earnings.to_string().into(),
            debit_cents: -net_income,
            credit_cents: 0,
            memo: Some("Year-end close: net loss absorbed by retained earnings".to_string()),
            sort_order: sort,
        });
        sort += 1;
    }

    // WIP-variance close-out (see the doc comment): absent field →
    // old fact, nothing to do; present but unparseable → malformed,
    // refuse rather than silently skip a write-off.
    let wip_variance = match fact.payload.get("wip_variance_cents") {
        None => 0,
        Some(v) => cents_from_payload(Some(v))
            .ok_or_else(|| payload_err(fact.kind, "wip_variance_cents must be integer cents"))?,
    };
    if wip_variance != 0 {
        let wip_account = fact
            .payload
            .get("wip_account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "wip_variance_cents without wip_account"))?;
        if wip_variance > 0 {
            lines.push(JournalLineDraft {
                account_code: retained_earnings.to_string().into(),
                debit_cents: wip_variance,
                credit_cents: 0,
                memo: Some(format!(
                    "Year-end close: WIP variance write-off ({wip_account})"
                )),
                sort_order: sort,
            });
            sort += 1;
            lines.push(JournalLineDraft {
                account_code: wip_account.to_string().into(),
                debit_cents: 0,
                credit_cents: wip_variance,
                memo: Some(format!("Year-end close: zero out {wip_account} residual")),
                sort_order: sort,
            });
            sort += 1;
        } else {
            // Credit residual (over-absorbed WIP) → mirror image.
            lines.push(JournalLineDraft {
                account_code: wip_account.to_string().into(),
                debit_cents: -wip_variance,
                credit_cents: 0,
                memo: Some(format!(
                    "Year-end close: zero out {wip_account} residual (was credit-balance)"
                )),
                sort_order: sort,
            });
            sort += 1;
            lines.push(JournalLineDraft {
                account_code: retained_earnings.to_string().into(),
                debit_cents: 0,
                credit_cents: -wip_variance,
                memo: Some(format!(
                    "Year-end close: WIP over-absorption to retained earnings ({wip_account})"
                )),
                sort_order: sort,
            });
            sort += 1;
        }
    }

    if lines.is_empty() {
        return Err(payload_err(
            fact.kind,
            "no revenue or expense balances — nothing to close",
        ));
    }

    let memo = fact
        .payload
        .get("period_end")
        .and_then(|v| v.as_str())
        .map(|end| format!("Year-end close for period ending {end}"));

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo,
        lines,
    })
}

// --- Helpers --------------------------------------------------------------

fn payload_err(kind: &str, reason: &str) -> LedgerError {
    LedgerError::InvalidPayload {
        kind: kind.to_string(),
        reason: reason.to_string(),
    }
}

/// Parse an integer-cents amount from a JSON payload. Accepts both signed
/// and unsigned JSON integers; a string "12345" also works for adapters
/// that stringify large numbers.
fn cents_from_payload(v: Option<&serde_json::Value>) -> Option<i64> {
    let v = v?;
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return Some(n as i64);
    }
    if let Some(s) = v.as_str() {
        return s.parse().ok();
    }
    None
}

fn invoice_memo(fact: &FactRef<'_>, verb: &str) -> Option<String> {
    // Live commerce facts now carry the full Invoice struct, whose id
    // serializes as `id`; the older minimal shape (and the live-only
    // finance.invoice.paid fact) use `invoice_id`. Accept either so the
    // memo — and thus the journal entry — is identical for the live fact
    // and the fact rebuilt from the event.
    fact.payload
        .get("invoice_id")
        .or_else(|| fact.payload.get("id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("{verb}: {id}"))
}

fn bill_memo(fact: &FactRef<'_>, verb: &str) -> Option<String> {
    fact.payload
        .get("vendor_invoice_id")
        .and_then(|v| v.as_str())
        .map(|id| format!("{verb}: {id}"))
}

// ---------------------------------------------------------------------------
// RuleSet v2 — ASC 606 revenue recognition
// ---------------------------------------------------------------------------
//
// The behavioral delta from v1:
//
//  1. `finance.invoice.issued` — for every invoice line whose
//     `recognition_pattern` is `"ratable"`, credit the Deferred
//     Revenue liability account (`2200`) instead of the normal
//     revenue account. Lines tagged `"immediate"` (or untagged)
//     keep v1 behavior. `"milestone"` is rejected — not yet
//     supported in v2.
//  2. New fact kind `finance.revenue.recognized` — the per-period
//     release fact emitted by the recognition scheduler (step 3,
//     not yet shipped). Debits `2200` and credits the schedule's
//     revenue account for the period's amount.
//
// Every other fact kind delegates to v1 mechanically — this keeps
// the diff surface tight and makes a future v3 easy to add in the
// same pattern.

/// Deferred Revenue account (ASC 606 liability). Seeded in the chart
/// by step 1 of the rollout. v2 routes ratable-pattern lines here
/// instead of their normal revenue account.
const DEFERRED_REVENUE_ACCOUNT: &str = "2200";

/// Recognition pattern on an invoice line payload. Default
/// `Immediate` when the field is absent — preserves v1 behavior for
/// callers that haven't started tagging their lines yet.
///
/// Per `asc-606-revenue-recognition.md` D2, each line is explicitly
/// tagged by the caller rather than inferred from the category: the
/// same `service` category can be per-visit (immediate) or a
/// multi-month retainer (ratable), and forcing the rule to guess
/// from the category alone leaks domain knowledge into the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecognitionPattern {
    Immediate,
    Ratable,
    Milestone,
}

impl RecognitionPattern {
    fn from_payload(line: &serde_json::Value) -> Result<Self, LedgerError> {
        let Some(raw) = line.get("recognition_pattern").and_then(|v| v.as_str()) else {
            return Ok(Self::Immediate);
        };
        match raw {
            "immediate" => Ok(Self::Immediate),
            "ratable" => Ok(Self::Ratable),
            "milestone" => Ok(Self::Milestone),
            other => Err(payload_err(
                "finance.invoice.issued",
                &format!("unknown recognition_pattern `{other}`"),
            )),
        }
    }
}

/// `finance.invoice.issued` posting rule — deferred-revenue-aware.
///
/// Ratable lines credit 2200 (Deferred Revenue); immediate lines
/// credit their per-category revenue account directly. Line grouping
/// key is `(category, pattern)` so a mixed invoice emits one
/// immediate credit per revenue category + one deferred credit per
/// ratable category grouped under 2200. COGS recognition rides on a
/// separate fact (`finance.cogs.recognized`) from the
/// `products.consume` side-effect handler at the FG row's actual
/// weighted-moving-average cost.
fn invoice_issued(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let items = fact
        .payload
        .get("line_items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| payload_err(fact.kind, "missing line_items array"))?;

    if items.is_empty() {
        return Err(payload_err(fact.kind, "line_items empty"));
    }

    // One bucket per (category, pattern). Using a BTreeMap with the
    // pattern inside the key keeps the output deterministic + sorted
    // without needing a second pass.
    let mut revenue_total: i64 = 0;
    let mut deferred_total: i64 = 0;
    let mut immediate_by_category: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    let mut deferred_by_category: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    // COGS is NOT part of the invoice JE. Per Q2
    // (docs/architecture-decisions.md §Finance & ledger, resolved
    // 2026-07-07) the consume owns it: the dispatcher's
    // `products-consume-on-invoice-created` rule drives
    // `/api/products/{sku}/inventory/consume` per FG line, which
    // drains the row's conserved value and posts
    // `finance.cogs.recognized` (DR 5100 / CR 1320) at exactly the
    // drained cents, tagged with the line's revenue_category. The
    // invoice JE carries revenue / AR / tax only.
    for li in items {
        // Accept either `category` (live path: commerce-postgres translates
        // `revenue_category` -> `category` when building the fact directly)
        // or `revenue_category` (rebuild path: gl_fact_projection_rules is
        // a 1:1 passthrough, so the source event's `revenue_category` field
        // survives). Both name the same concept; the rule resolves either
        // to the same revenue account via `revenue_account_for`.
        let category = li
            .get("category")
            .or_else(|| li.get("revenue_category"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "line_items[].category missing"))?;
        let amount = cents_from_payload(li.get("amount_cents"))
            .ok_or_else(|| payload_err(fact.kind, "line_items[].amount_cents missing"))?;
        let pattern = RecognitionPattern::from_payload(li)?;
        match pattern {
            RecognitionPattern::Immediate => {
                // Validate the category resolves to a known revenue
                // account — same contract as v1.
                revenue_account_for(category)?;
                revenue_total += amount;
                *immediate_by_category
                    .entry(category.to_string())
                    .or_default() += amount;
            }
            RecognitionPattern::Ratable => {
                // The category still has to be known — it flows
                // through to the schedule row + eventual recognition
                // entry, which credits the revenue account for that
                // category.
                revenue_account_for(category)?;
                deferred_total += amount;
                *deferred_by_category
                    .entry(category.to_string())
                    .or_default() += amount;
            }
            RecognitionPattern::Milestone => {
                return Err(payload_err(
                    fact.kind,
                    "recognition_pattern=milestone not yet supported (v3)",
                ));
            }
        }
    }

    // Tax lines — identical to v1. Tax credits A/R just like a
    // revenue line does; the tax liability account is where it
    // ultimately credits.
    let tax_lines = fact
        .payload
        .get("tax_lines")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tax_credits: Vec<(String, i64, String)> = Vec::new();
    let mut tax_total: i64 = 0;
    for tl in &tax_lines {
        let amount = cents_from_payload(tl.get("amount_cents"))
            .ok_or_else(|| payload_err(fact.kind, "tax_lines[].amount_cents missing"))?;
        if amount < 0 {
            return Err(payload_err(
                fact.kind,
                "tax_lines[].amount_cents must be non-negative",
            ));
        }
        if amount == 0 {
            continue;
        }
        let account = tl
            .get("account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| payload_err(fact.kind, "tax_lines[].account missing"))?;
        if !matches!(account, "2300") {
            return Err(payload_err(
                fact.kind,
                &format!("tax_lines[].account `{account}` not allowed"),
            ));
        }
        let jurisdiction = tl
            .get("jurisdiction")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        tax_total += amount;
        tax_credits.push((account.to_string(), amount, jurisdiction));
    }

    let ar_total = revenue_total + deferred_total + tax_total;
    let mut lines = Vec::with_capacity(
        1 + immediate_by_category.len() + deferred_by_category.len() + tax_credits.len(),
    );
    lines.push(JournalLineDraft::debit("1100", ar_total, 0));

    let mut sort: i16 = 1;
    for (category, amount) in immediate_by_category {
        let code = revenue_account_for(&category)?;
        lines.push(JournalLineDraft::credit(code, amount, sort));
        sort += 1;
    }
    for (category, amount) in deferred_by_category {
        lines.push(JournalLineDraft {
            account_code: DEFERRED_REVENUE_ACCOUNT.to_string().into(),
            debit_cents: 0,
            credit_cents: amount,
            memo: Some(format!("Deferred: {category}")),
            sort_order: sort,
        });
        sort += 1;
    }
    tax_credits.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
    for (account, amount, jurisdiction) in tax_credits {
        let memo = if jurisdiction.is_empty() {
            None
        } else {
            Some(format!("Sales tax: {jurisdiction}"))
        };
        lines.push(JournalLineDraft {
            account_code: account.into(),
            debit_cents: 0,
            credit_cents: amount,
            memo,
            sort_order: sort,
        });
        sort += 1;
    }

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: invoice_memo(fact, "Invoice issued"),
        lines,
    })
}

/// Periodic revenue recognition fact — emitted by the scheduler
/// (step 3) for each active `revenue_schedules` row as it hits its
/// recognition date. Payload shape:
///
/// ```json
/// {
///   "schedule_id":  "rs-...",
///   "period_start": "2026-02-01",
///   "period_end":   "2026-02-28",
///   "amount_cents": 100000,
///   "category":     "contracts",
///   "account_id":  "account-00042"
/// }
/// ```
///
/// Entry:
/// ```text
/// DR 2200 Deferred Revenue   amount_cents
/// CR <revenue_account>       amount_cents
/// ```
fn revenue_recognized(fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
    let amount = cents_from_payload(fact.payload.get("amount_cents"))
        .ok_or_else(|| payload_err(fact.kind, "amount_cents missing"))?;
    if amount <= 0 {
        return Err(payload_err(fact.kind, "amount_cents must be positive"));
    }
    let category = fact
        .payload
        .get("category")
        .and_then(|v| v.as_str())
        .ok_or_else(|| payload_err(fact.kind, "category missing"))?;
    let revenue_account = revenue_account_for(category)?;

    let memo = fact
        .payload
        .get("schedule_id")
        .and_then(|v| v.as_str())
        .map(|id| format!("Revenue recognized ({category}): {id}"));

    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo,
        lines: vec![
            JournalLineDraft::debit(DEFERRED_REVENUE_ACCOUNT, amount, 0),
            JournalLineDraft::credit(revenue_account, amount, 1),
        ],
    })
}

// ---------------------------------------------------------------------------
// Tests for v2 — v1 tests live in tests/rules_v1.rs; these cover the
// v2-only behavior.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod v2_tests {
    use super::*;

    fn fact<'a>(id: uuid::Uuid, kind: &'a str, payload: &'a serde_json::Value) -> FactRef<'a> {
        FactRef {
            id,
            kind,
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
            payload,
        }
    }

    #[test]
    fn contracts_line_marked_ratable_credits_deferred_revenue() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-1",
            "line_items": [{
                "category": "contracts",
                "amount_cents": 1_200_000,
                "recognition_pattern": "ratable",
            }],
        });
        let draft = evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).unwrap();
        assert!(draft.is_balanced());
        assert_eq!(draft.total_debits(), 1_200_000);
        // Exactly one credit: 2200 for the full amount.
        let credits: Vec<(&str, i64)> = draft
            .lines
            .iter()
            .filter(|l| l.credit_cents > 0)
            .map(|l| (l.account_code.as_ref(), l.credit_cents))
            .collect();
        assert_eq!(credits, vec![("2200", 1_200_000)]);
    }

    #[test]
    fn mixed_invoice_splits_immediate_vs_ratable() {
        // A bundle: a $5k device sale (immediate) + $12k contract
        // (ratable). AR debits for the sum; one credit to 4100 for
        // the immediate hardware, one credit to 2200 for the
        // deferred contract.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-bundle",
            "line_items": [
                { "category": "new-sales", "amount_cents": 500_000, "recognition_pattern": "immediate" },
                { "category": "contracts", "amount_cents": 1_200_000, "recognition_pattern": "ratable" },
            ],
        });
        let draft = evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).unwrap();
        assert!(draft.is_balanced());
        // Total debits: AR carries revenue + deferred + tax. These
        // lines carry no SKU/cost_basis, so there's no FG COGS leg
        // (COGS rides the invoice only for FG lines — see
        // `invoice_issued_v2`).
        assert_eq!(draft.total_debits(), 1_700_000);
        // Credits: 4100 = 500_000 (immediate new-sales) + 2200 =
        // 1_200_000 (deferred contracts).
        let mut credits: Vec<(&str, i64)> = draft
            .lines
            .iter()
            .filter(|l| l.credit_cents > 0)
            .map(|l| (l.account_code.as_ref(), l.credit_cents))
            .collect();
        credits.sort();
        assert_eq!(credits, vec![("2200", 1_200_000), ("4100", 500_000)],);
    }

    #[test]
    fn multiple_ratable_categories_group_under_2200() {
        // Two ratable lines in different categories still both
        // credit 2200; the memo distinguishes them at the line
        // level. This matches D1 (single deferred account for v1).
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-multi",
            "line_items": [
                { "category": "contracts", "amount_cents": 1_200_000, "recognition_pattern": "ratable" },
                { "category": "service",   "amount_cents":   300_000, "recognition_pattern": "ratable" },
            ],
        });
        let draft = evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).unwrap();
        assert!(draft.is_balanced());
        // Both credits go to 2200; memos carry the categories.
        let deferred: Vec<&JournalLineDraft> = draft
            .lines
            .iter()
            .filter(|l| l.account_code.as_ref() == "2200")
            .collect();
        assert_eq!(deferred.len(), 2);
        let memos: Vec<&str> = deferred.iter().filter_map(|l| l.memo.as_deref()).collect();
        assert!(memos.iter().any(|m| m.contains("contracts")));
        assert!(memos.iter().any(|m| m.contains("service")));
    }

    #[test]
    fn ratable_with_tax_lines_balances() {
        // Tax behavior is unchanged; verify the cross-interaction.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-tax",
            "line_items": [
                { "category": "contracts", "amount_cents": 1_200_000, "recognition_pattern": "ratable" },
            ],
            "tax_lines": [
                { "account": "2300", "amount_cents": 96_000, "jurisdiction": "US-CA" },
            ],
        });
        let draft = evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).unwrap();
        assert!(draft.is_balanced());
        assert_eq!(draft.total_debits(), 1_296_000);
    }

    #[test]
    fn milestone_pattern_is_rejected() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-m",
            "line_items": [{
                "category": "contracts",
                "amount_cents": 1_200_000,
                "recognition_pattern": "milestone",
            }],
        });
        let err =
            evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).unwrap_err();
        assert!(format!("{err:?}").contains("milestone"));
    }

    #[test]
    fn unknown_pattern_is_rejected() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "invoice_id": "inv-u",
            "line_items": [{
                "category": "contracts",
                "amount_cents": 100,
                "recognition_pattern": "quarterly",
            }],
        });
        assert!(evaluate(&BossRuleSet, &fact(id, "finance.invoice.issued", &payload)).is_err(),);
    }

    #[test]
    fn revenue_recognized_posts_deferred_to_revenue() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "schedule_id":  "rs-1",
            "period_start": "2026-02-01",
            "period_end":   "2026-02-28",
            "amount_cents": 100_000,
            "category":     "contracts",
            "account_id":  "account-00001",
        });
        let draft = evaluate(
            &BossRuleSet,
            &fact(id, "finance.revenue.recognized", &payload),
        )
        .unwrap();
        assert!(draft.is_balanced());
        assert_eq!(draft.total_debits(), 100_000);
        let debit: &JournalLineDraft = draft.lines.iter().find(|l| l.debit_cents > 0).unwrap();
        assert_eq!(debit.account_code.as_ref(), "2200");
        let credit: &JournalLineDraft = draft.lines.iter().find(|l| l.credit_cents > 0).unwrap();
        assert_eq!(credit.account_code.as_ref(), "4140");
    }

    #[test]
    fn revenue_recognized_rejects_zero_or_negative_amount() {
        let id = uuid::Uuid::new_v4();
        for amount in [0, -1, -100] {
            let payload = serde_json::json!({
                "schedule_id":  "rs-1",
                "period_start": "2026-02-01",
                "period_end":   "2026-02-28",
                "amount_cents": amount,
                "category":     "contracts",
                "account_id":  "account-00001",
            });
            assert!(
                evaluate(
                    &BossRuleSet,
                    &fact(id, "finance.revenue.recognized", &payload),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn revenue_recognized_rejects_unknown_category() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "schedule_id":  "rs-1",
            "period_start": "2026-02-01",
            "period_end":   "2026-02-28",
            "amount_cents": 100_000,
            "category":     "bogus",
            "account_id":  "account-00001",
        });
        assert!(
            evaluate(
                &BossRuleSet,
                &fact(id, "finance.revenue.recognized", &payload)
            )
            .is_err(),
        );
    }

    // --- finance.period.closed -------------------------------------------

    #[test]
    fn period_closed_zeros_revenue_expense_and_credits_retained_earnings() {
        let id = uuid::Uuid::new_v4();
        // $1,200 revenue (two accounts), $500 expense (two accounts),
        // → $700 net income → credit RE $700.
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000001",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [
                { "account_code": "4100", "balance_cents": 70_000 },
                { "account_code": "4140", "balance_cents": 50_000 },
            ],
            "expense_lines": [
                { "account_code": "6100", "balance_cents": 30_000 },
                { "account_code": "6200", "balance_cents": 20_000 },
            ],
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced());
        assert_eq!(draft.total_debits(), 120_000);
        // Lines: 2 DR revenue + 2 CR expense + 1 CR RE = 5 lines.
        assert_eq!(draft.lines.len(), 5);
        // Revenue lines: DR the account.
        let rev_4100 = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "4100")
            .unwrap();
        assert_eq!(rev_4100.debit_cents, 70_000);
        assert_eq!(rev_4100.credit_cents, 0);
        // RE line: CR by net income.
        let re = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "3000")
            .unwrap();
        assert_eq!(re.credit_cents, 70_000);
        assert_eq!(re.debit_cents, 0);
    }

    #[test]
    fn period_closed_writes_wip_variance_to_retained_earnings() {
        // Residual 1310 balance writes off via a
        // year-end RE adjustment so 1310 closes to 0 AND 5100
        // still closes clean (posting to 5100 inside the same JE
        // would inflate it after the expense-zero line drained it).
        //
        // The WIP account rides in the payload (`wip_account`), not
        // hardcoded in the rule — same contract as
        // `retained_earnings_account`: the rule is chart-agnostic,
        // and the close endpoint owns the starter-chart default
        // ("1310").
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000010",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 100_000 }],
            "expense_lines": [{ "account_code": "5100", "balance_cents": 60_000 }],
            "wip_variance_cents": 25_000,
            "wip_account": "1310",
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced(), "WIP variance close must balance");
        // Expected: DR 4100 100k, CR 5100 60k, CR RE 40k, DR RE 25k, CR 1310 25k.
        // Total debits = 100k + 25k = 125k. Total credits = 60k + 40k + 25k = 125k.
        assert_eq!(draft.total_debits(), 125_000);
        // The 1310 CR line zeros out WIP at period end.
        let wip_cr = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "1310")
            .expect("1310 line missing");
        assert_eq!(wip_cr.credit_cents, 25_000);
        assert_eq!(wip_cr.debit_cents, 0);
        // 5100 stays clean — only the expense-zero CR, no WIP DR.
        let cogs_lines: Vec<_> = draft
            .lines
            .iter()
            .filter(|l| l.account_code.as_ref() == "5100")
            .collect();
        assert_eq!(
            cogs_lines.len(),
            1,
            "5100 should have one close line (the expense zero), not a WIP variance line"
        );
        assert_eq!(cogs_lines[0].credit_cents, 60_000);
    }

    #[test]
    fn period_closed_negative_wip_variance_debits_wip_credits_re() {
        // Over-absorption leaves 1310 with a CREDIT balance
        // (variance negative): zeroing it needs the mirror JE —
        // DR 1310 / CR RE.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000011",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 100_000 }],
            "expense_lines": [{ "account_code": "5100", "balance_cents": 60_000 }],
            "wip_variance_cents": -7_000,
            "wip_account": "1310",
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced());
        // DR 4100 100k + DR 1310 7k = 107k total debits.
        assert_eq!(draft.total_debits(), 107_000);
        let wip = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "1310")
            .expect("1310 line missing");
        assert_eq!(wip.debit_cents, 7_000);
        assert_eq!(wip.credit_cents, 0);
        // RE carries two lines: the net-income CR 40k and the
        // variance CR 7k — separate memos, separate close-out
        // semantics, deliberately not netted.
        let re_credit_total: i64 = draft
            .lines
            .iter()
            .filter(|l| l.account_code.as_ref() == "3000")
            .map(|l| l.credit_cents)
            .sum();
        assert_eq!(re_credit_total, 47_000);
    }

    #[test]
    fn period_closed_zero_wip_variance_adds_no_lines() {
        // wip_variance_cents: 0 (the endpoint always stamps the
        // field) must behave exactly like the field being absent.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000012",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 100_000 }],
            "expense_lines": [{ "account_code": "5100", "balance_cents": 60_000 }],
            "wip_variance_cents": 0,
            "wip_account": "1310",
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced());
        // DR 4100, CR 5100, CR RE — no 1310 line.
        assert_eq!(draft.lines.len(), 3);
        assert!(
            !draft
                .lines
                .iter()
                .any(|l| l.account_code.as_ref() == "1310"),
            "zero variance must not emit a WIP line"
        );
    }

    #[test]
    fn period_closed_wip_variance_without_account_is_rejected() {
        // A non-zero variance with no wip_account is a malformed
        // fact — refuse rather than guess at the chart.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000013",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 100_000 }],
            "expense_lines": [{ "account_code": "5100", "balance_cents": 60_000 }],
            "wip_variance_cents": 25_000,
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let err = evaluate(&BossRuleSet, &fact_ref).unwrap_err();
        assert!(
            matches!(err, LedgerError::InvalidPayload { .. }),
            "expected InvalidPayload, got {err:?}"
        );
    }

    #[test]
    fn inventory_capitalized_posts_dr_raw_cr_grir() {
        // Goods-receipt capitalization routes through the value-movement
        // rule via the distinct `finance.inventory.capitalized` kind:
        // DR 1300 raw / CR 2110 GR-IR. The vendor bill later clears 2110.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "total_cost_cents": 42_000,
            "debit_account": "1300",
            "credit_account": "2110",
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.inventory.capitalized",
            happened_on: chrono::NaiveDate::from_ymd_opt(2025, 4, 1).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced(), "capitalization must balance");
        assert_eq!(draft.total_debits(), 42_000);
        let raw = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "1300")
            .expect("1300 raw line");
        assert_eq!(raw.debit_cents, 42_000);
        assert_eq!(raw.credit_cents, 0);
        let grir = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "2110")
            .expect("2110 GR-IR line");
        assert_eq!(grir.credit_cents, 42_000);
        assert_eq!(grir.debit_cents, 0);
    }

    #[test]
    fn period_closed_handles_net_loss_by_debiting_retained_earnings() {
        let id = uuid::Uuid::new_v4();
        // Expense > revenue — loss flows to RE as a debit.
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000002",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 10_000 }],
            "expense_lines": [{ "account_code": "6100", "balance_cents": 25_000 }],
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced());
        let re = draft
            .lines
            .iter()
            .find(|l| l.account_code.as_ref() == "3000")
            .unwrap();
        assert_eq!(re.debit_cents, 15_000);
        assert_eq!(re.credit_cents, 0);
    }

    #[test]
    fn period_closed_skips_zero_balances_but_still_balances() {
        // A zero-balance account produces no line; if every line is
        // zero the rule errors out.
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000003",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [
                { "account_code": "4100", "balance_cents": 0 },
                { "account_code": "4140", "balance_cents": 5_000 },
            ],
            "expense_lines": [
                { "account_code": "6100", "balance_cents": 0 },
            ],
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let draft = evaluate(&BossRuleSet, &fact_ref).unwrap();
        assert!(draft.is_balanced());
        // 1 DR revenue (4140) + 1 CR RE — zero-balance 4100 and 6100 dropped.
        assert_eq!(draft.lines.len(), 2);
    }

    #[test]
    fn period_closed_with_all_zero_balances_errors() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "period_id": "c0000000-0000-0000-0000-000000000004",
            "period_end": "2026-12-31",
            "retained_earnings_account": "3000",
            "revenue_lines": [{ "account_code": "4100", "balance_cents": 0 }],
            "expense_lines": [{ "account_code": "6100", "balance_cents": 0 }],
        });
        let fact_ref = FactRef {
            id,
            kind: "finance.period.closed",
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            payload: &payload,
        };
        let err = evaluate(&BossRuleSet, &fact_ref).unwrap_err();
        assert!(
            matches!(err, LedgerError::InvalidPayload { .. }),
            "expected InvalidPayload, got {err:?}"
        );
    }

    #[test]
    fn invoice_written_off_posts_bad_debt_against_ar() {
        let id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "id": "inv-1",
            "amount_cents": 75_000_i64,
            "account_id": "acc-X",
        });
        let draft = evaluate(
            &BossRuleSet,
            &fact(id, "finance.invoice.written_off", &payload),
        )
        .expect("write-off should evaluate");
        assert!(draft.is_balanced(), "DR/CR must balance");
        let dr_6700 = draft
            .lines
            .iter()
            .find(|l| l.account_code == "6700")
            .expect("expected DR to 6700 Bad Debt Expense");
        let cr_1100 = draft
            .lines
            .iter()
            .find(|l| l.account_code == "1100")
            .expect("expected CR to 1100 A/R");
        assert_eq!(dr_6700.debit_cents, 75_000);
        assert_eq!(dr_6700.credit_cents, 0);
        assert_eq!(cr_1100.credit_cents, 75_000);
        assert_eq!(cr_1100.debit_cents, 0);
    }
}
