//! Unit tests for RuleSet v1 — pure, no DB. Every rule must produce a
//! balanced entry; unknown fact kinds + malformed payloads must fail
//! explicitly rather than producing incorrect postings.

use boss_ledger::rules::{BossRuleSet, evaluate};
use boss_ledger::types::FactRef;
use boss_ledger::{JournalLineDraft, LedgerError};
use chrono::NaiveDate;
use serde_json::json;
use uuid::Uuid;

fn fact<'a>(kind: &'a str, payload: &'a serde_json::Value) -> FactRef<'a> {
    FactRef {
        id: Uuid::nil(),
        kind,
        happened_on: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        payload,
    }
}

fn line_for<'a>(draft: &'a [JournalLineDraft], code: &str) -> &'a JournalLineDraft {
    draft
        .iter()
        .find(|l| l.account_code == code)
        .unwrap_or_else(|| panic!("no line for account {code}: {draft:?}"))
}

#[test]
fn invoice_issued_single_line_is_balanced() {
    let payload = json!({
        "invoice_id": "inv-1",
        "amount_cents": 1_200_000,
        "currency": "USD",
        "line_items": [
            {"category": "new-sales", "amount_cents": 1_200_000, "currency": "USD"},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
    assert!(draft.is_balanced());
    // RuleSet v1 posts revenue + A/R only; COGS recognition has
    // moved off the invoice path entirely (Model B). The fixed-
    // ratio COGS shortcut here was a `cogs_bps_for(category) ×
    // revenue` invention that credited 1300 Inventory with no
    // matching debit; the brewery sim's books only "balanced"
    // when ingredient-restock bills happened to total the same
    // amount. COGS now rides on `finance.cogs.recognized` from
    // products.consume at the actual cost basis of the FG row.
    assert_eq!(draft.total_debits(), 1_200_000i64);
    assert_eq!(line_for(&draft.lines, "1100").debit_cents, 1_200_000i64);
    assert_eq!(line_for(&draft.lines, "4100").credit_cents, 1_200_000i64);
}

#[test]
fn invoice_issued_mixed_categories_splits_revenue() {
    // Mixed-category invoice: debits collapse into one A/R line; credits
    // split by category into the matching revenue accounts.
    let payload = json!({
        "invoice_id": "inv-2",
        "amount_cents": 1_500_000,
        "currency": "USD",
        "line_items": [
            {"category": "new-sales", "amount_cents": 1_000_000, "currency": "USD"},
            {"category": "service",   "amount_cents":   300_000, "currency": "USD"},
            {"category": "service",   "amount_cents":   200_000, "currency": "USD"},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1100").debit_cents, 1_500_000i64);
    assert_eq!(line_for(&draft.lines, "4100").credit_cents, 1_000_000i64);
    // Two service lines roll up into one credit to 4120.
    assert_eq!(line_for(&draft.lines, "4120").credit_cents, 500_000i64);
}

#[test]
fn invoice_issued_all_revenue_categories_resolve() {
    let cases = [
        ("new-sales", "4100"),
        ("used-sales", "4110"),
        ("service", "4120"),
        ("parts", "4130"),
        ("contracts", "4140"),
    ];
    for (category, account) in cases {
        let payload = json!({
            "invoice_id": "inv-x",
            "amount_cents": 50_000,
            "currency": "USD",
            "line_items": [{"category": category, "amount_cents": 50_000, "currency": "USD"}],
        });
        let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
        assert_eq!(
            line_for(&draft.lines, account).credit_cents,
            50_000i64,
            "category {category} should credit {account}"
        );
    }
}

#[test]
fn invoice_issued_unknown_category_fails() {
    let payload = json!({
        "invoice_id": "inv-bad",
        "amount_cents": 50_000,
        "currency": "USD",
        "line_items": [{"category": "mystery", "amount_cents": 50_000, "currency": "USD"}],
    });
    match evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)) {
        Err(LedgerError::InvalidPayload { reason, .. }) => {
            assert!(reason.contains("mystery"), "reason: {reason}");
        }
        other => panic!("expected InvalidPayload, got {other:?}"),
    }
}

#[test]
fn invoice_issued_missing_line_items_fails() {
    let payload = json!({"invoice_id": "inv-nolines", "amount_cents": 50_000, "currency": "USD"});
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn invoice_paid_swaps_ar_to_cash() {
    let payload = json!({"invoice_id": "inv-1", "amount_cents": 1_200_000, "currency": "USD"});
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.paid", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1000").debit_cents, 1_200_000i64);
    assert_eq!(line_for(&draft.lines, "1100").credit_cents, 1_200_000i64);
}

#[test]
fn payment_received_debits_cash_in_transit() {
    // Two-phase payment part 1: A/R moves into Cash in Transit (1010),
    // not Cash. Bank clearing generator will flip 1010 → 1000 later.
    let payload = json!({"invoice_id": "inv-1", "amount_cents": 1_200_000, "currency": "USD"});
    let draft = evaluate(&BossRuleSet, &fact("finance.payment.received", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1010").debit_cents, 1_200_000i64);
    assert_eq!(line_for(&draft.lines, "1100").credit_cents, 1_200_000i64);
    // Cash (1000) is not touched at the received step.
    assert!(draft.lines.iter().all(|l| l.account_code != "1000"));
}

#[test]
fn payment_settled_drains_cash_in_transit_to_cash() {
    let payload = json!({
        "invoice_id": "inv-1",
        "settlement_id": "set-abc",
        "amount_cents": 1_200_000,
        "currency": "USD",
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.payment.settled", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1000").debit_cents, 1_200_000i64);
    assert_eq!(line_for(&draft.lines, "1010").credit_cents, 1_200_000i64);
    assert_eq!(
        draft.memo.as_deref(),
        Some("Payment settled: set-abc"),
        "memo should carry the settlement id"
    );
}

#[test]
fn payment_settled_without_settlement_id_falls_back_to_invoice_memo() {
    let payload = json!({"invoice_id": "inv-9", "amount_cents": 500_000, "currency": "USD"});
    let draft = evaluate(&BossRuleSet, &fact("finance.payment.settled", &payload)).unwrap();
    assert_eq!(draft.memo.as_deref(), Some("Payment settled: inv-9"));
}

#[test]
fn payment_received_missing_amount_fails() {
    let payload = json!({"invoice_id": "inv-1", "currency": "USD"});
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.payment.received", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn two_phase_payment_round_trip_nets_cash_delta_and_zeros_transit() {
    // Proves the invariant the bank-clearing generator is buying us:
    // after received + settled both post, the A/R drop and the Cash
    // gain are equal, and Cash in Transit lands at zero.
    let payload = json!({"invoice_id": "inv-1", "amount_cents": 750_000, "currency": "USD"});

    let recv = evaluate(&BossRuleSet, &fact("finance.payment.received", &payload)).unwrap();
    let settle_payload = json!({
        "invoice_id": "inv-1",
        "settlement_id": "set-1",
        "amount_cents": 750_000,
        "currency": "USD",
    });
    let settle = evaluate(
        &BossRuleSet,
        &fact("finance.payment.settled", &settle_payload),
    )
    .unwrap();

    let cash_delta = line_for(&settle.lines, "1000").debit_cents;
    let ar_delta = line_for(&recv.lines, "1100").credit_cents;
    let transit_in = line_for(&recv.lines, "1010").debit_cents;
    let transit_out = line_for(&settle.lines, "1010").credit_cents;

    assert_eq!(cash_delta, ar_delta, "Cash gain must equal A/R drop");
    assert_eq!(transit_in, transit_out, "Cash in Transit must net to zero");
}

#[test]
fn payroll_run_posts_compound_entry() {
    // 5 employees, $10,000 gross / $2,200 withheld / $900 employer tax.
    // Expected:
    //   DR 6100  10,000  (gross)
    //   DR 6400     900  (employer tax)
    //   CR 2150   3,100  (withheld + employer tax)
    //   CR 1000   7,800  (net)
    let payload = json!({
        "run_id": "pr-abc",
        "gross_cents": 1_000_000,
        "withheld_cents": 220_000,
        "employer_tax_cents": 90_000,
        "employee_count": 5,
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.payroll.run", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "6100").debit_cents, 1_000_000i64);
    assert_eq!(line_for(&draft.lines, "6400").debit_cents, 90_000i64);
    assert_eq!(line_for(&draft.lines, "2150").credit_cents, 310_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 780_000i64);
    assert_eq!(
        draft.memo.as_deref(),
        Some("Payroll run pr-abc (5 employees)")
    );
}

#[test]
fn payroll_run_with_zero_employer_tax_omits_6400_line() {
    // Edge case: no employer-side cost (e.g. sim bootstrap / test data).
    // The rule should still balance and should not emit a zero-amount
    // line for 6400 Payroll Taxes.
    let payload = json!({
        "run_id": "pr-bare",
        "gross_cents": 500_000,
        "withheld_cents": 100_000,
        "employer_tax_cents": 0,
        "employee_count": 2,
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.payroll.run", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert!(draft.lines.iter().all(|l| l.account_code != "6400"));
    assert_eq!(line_for(&draft.lines, "2150").credit_cents, 100_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 400_000i64);
}

#[test]
fn payroll_run_rejects_withheld_over_gross() {
    let payload = json!({
        "run_id": "pr-broken",
        "gross_cents": 100_000,
        "withheld_cents": 200_000,
        "employer_tax_cents": 0,
        "employee_count": 1,
    });
    match evaluate(&BossRuleSet, &fact("finance.payroll.run", &payload)) {
        Err(LedgerError::InvalidPayload { reason, .. }) => {
            assert!(reason.contains("withheld"), "reason: {reason}");
        }
        other => panic!("expected InvalidPayload, got {other:?}"),
    }
}

#[test]
fn payroll_run_rejects_negative_amounts() {
    let payload = json!({
        "run_id": "pr-neg",
        "gross_cents": -100,
        "withheld_cents": 0,
        "employer_tax_cents": 0,
        "employee_count": 1,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.payroll.run", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn payroll_run_missing_gross_fails() {
    let payload = json!({"run_id": "pr-1", "withheld_cents": 0, "employer_tax_cents": 0});
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.payroll.run", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn bill_approved_clears_grir_and_lands_ap() {
    // Lines array is the source of truth: Σ(qty × unit_cost_cents)
    // = 450_000. The lump amount_cents alongside is a redundant
    // assertion the rule cross-checks. The inventory bill now DEBITS
    // 2110 GR-IR (raw was capitalized at goods receipt), not 1300.
    let payload = json!({
        "vendor_invoice_id": "vi-1",
        "amount_cents": 450_000,
        "currency": "USD",
        "lines": [
            { "part_sku": "ING-MALT-2ROW-50", "qty": 100, "unit_cost_cents": 2500 },
            { "part_sku": "ING-HOPS-CASCADE-44", "qty": 4, "unit_cost_cents": 50_000 },
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.bill.approved", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2110").debit_cents, 450_000i64);
    assert_eq!(line_for(&draft.lines, "2100").credit_cents, 450_000i64);
}

#[test]
fn bill_paid_settles_ap_from_cash() {
    let payload = json!({"vendor_invoice_id": "vi-1", "amount_cents": 450_000, "currency": "USD"});
    let draft = evaluate(&BossRuleSet, &fact("finance.bill.paid", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2100").debit_cents, 450_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 450_000i64);
}

#[test]
fn unknown_fact_kind_is_rejected() {
    let payload = json!({});
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.mystery", &payload)),
        Err(LedgerError::UnknownFactKind(_))
    ));
}

#[test]
fn posted_on_comes_from_fact_happened_on() {
    let payload = json!({"invoice_id": "inv-1", "amount_cents": 10_000, "currency": "USD"});
    let f = fact("finance.invoice.paid", &payload);
    let draft = evaluate(&BossRuleSet, &f).unwrap();
    assert_eq!(draft.posted_on, f.happened_on);
}

#[test]
fn manual_entry_passes_lines_through() {
    let payload = json!({
        "memo": "Q1 rent accrual",
        "lines": [
            {"account_code": "6200", "debit_cents": 250_000, "memo": "April rent"},
            {"account_code": "2100", "credit_cents": 250_000},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.manual.entry", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(draft.memo.as_deref(), Some("Q1 rent accrual"));
    assert_eq!(line_for(&draft.lines, "6200").debit_cents, 250_000i64);
    assert_eq!(line_for(&draft.lines, "2100").credit_cents, 250_000i64);
    // Sort order follows input order so the UI shows the entry the way the
    // user wrote it.
    assert_eq!(line_for(&draft.lines, "6200").sort_order, 0);
    assert_eq!(line_for(&draft.lines, "2100").sort_order, 1);
    // Per-line memos preserved.
    assert_eq!(
        line_for(&draft.lines, "6200").memo.as_deref(),
        Some("April rent"),
    );
}

#[test]
fn manual_entry_needs_at_least_two_lines() {
    let payload = json!({
        "lines": [{"account_code": "6200", "debit_cents": 100}],
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.manual.entry", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn manual_entry_rejects_line_with_both_sides() {
    let payload = json!({
        "lines": [
            {"account_code": "6200", "debit_cents": 100, "credit_cents": 100},
            {"account_code": "2100", "credit_cents": 100},
        ],
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.manual.entry", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn manual_entry_rejects_unbalanced_draft() {
    // The rule itself doesn't enforce balance; the top-level evaluate()
    // does. Manual entries go through the same check, so a malformed
    // payload with unbalanced sums surfaces as LedgerError::Unbalanced.
    let payload = json!({
        "lines": [
            {"account_code": "6200", "debit_cents": 100},
            {"account_code": "2100", "credit_cents": 99},
        ],
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.manual.entry", &payload)),
        Err(LedgerError::Unbalanced { .. })
    ));
}

// --- tax_lines on finance.invoice.issued ---------------------------------

#[test]
fn invoice_issued_with_sales_tax_credits_2300() {
    // $1,000 service invoice at 7.25% California sales tax. The account
    // owes $1,072.50. Entry:
    //   DR 1100  107_250
    //   CR 4120  100_000
    //   CR 2300    7_250
    let payload = json!({
        "invoice_id": "inv-ca-1",
        "amount_cents": 107_250,
        "currency": "USD",
        "line_items": [
            {"category": "service", "amount_cents": 100_000, "currency": "USD"},
        ],
        "tax_lines": [
            {"account": "2300", "jurisdiction": "US-CA", "amount_cents": 7_250},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1100").debit_cents, 107_250i64);
    assert_eq!(line_for(&draft.lines, "4120").credit_cents, 100_000i64);
    assert_eq!(line_for(&draft.lines, "2300").credit_cents, 7_250i64);
    assert_eq!(
        line_for(&draft.lines, "2300").memo.as_deref(),
        Some("Sales tax: US-CA")
    );
}

#[test]
fn invoice_issued_zero_tax_line_is_omitted() {
    // An empty/exempt tax line shouldn't generate a zero-amount
    // CR row that the DB's nonzero-line CHECK would reject.
    let payload = json!({
        "invoice_id": "inv-exempt",
        "amount_cents": 100_000,
        "currency": "USD",
        "line_items": [
            {"category": "new-sales", "amount_cents": 100_000, "currency": "USD"},
        ],
        "tax_lines": [
            {"account": "2300", "jurisdiction": "US-OR", "amount_cents": 0},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert!(draft.lines.iter().all(|l| l.account_code != "2300"));
}

#[test]
fn invoice_issued_no_tax_lines_key_is_backward_compatible() {
    // Pre-session-3 payloads don't carry a tax_lines array at all.
    // The rule must accept them unchanged.
    let payload = json!({
        "invoice_id": "inv-legacy",
        "amount_cents": 50_000,
        "currency": "USD",
        "line_items": [
            {"category": "parts", "amount_cents": 50_000, "currency": "USD"},
        ],
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1100").debit_cents, 50_000i64);
    assert_eq!(line_for(&draft.lines, "4130").credit_cents, 50_000i64);
}

#[test]
fn invoice_issued_rejects_negative_tax() {
    let payload = json!({
        "invoice_id": "inv-bad",
        "amount_cents": 100_000,
        "currency": "USD",
        "line_items": [{"category": "service", "amount_cents": 100_000, "currency": "USD"}],
        "tax_lines": [{"account": "2300", "jurisdiction": "US-CA", "amount_cents": -500}],
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn invoice_issued_rejects_unknown_tax_account() {
    // Only 2300 is in the allow-list today. A future RuleSet bump
    // that adds 2320 Excise would extend the match; for now, unknown
    // codes must fail loud rather than write to the wrong account.
    let payload = json!({
        "invoice_id": "inv-bad",
        "amount_cents": 100_000,
        "currency": "USD",
        "line_items": [{"category": "service", "amount_cents": 100_000, "currency": "USD"}],
        "tax_lines": [{"account": "2320", "jurisdiction": "US-CA", "amount_cents": 100}],
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.invoice.issued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

// --- finance.tax.remitted ------------------------------------------------

#[test]
fn tax_remitted_drains_sales_liability_to_cash() {
    let payload = json!({
        "filing_id": "tf-sales-US-CA-2026-03",
        "kind": "sales",
        "jurisdiction": "US-CA",
        "liability_account": "2300",
        "amount_cents": 725_000,
        "period_start": "2026-03-01",
        "period_end": "2026-03-31",
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.tax.remitted", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2300").debit_cents, 725_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 725_000i64);
    assert_eq!(
        draft.memo.as_deref(),
        Some("Tax remitted (sales US-CA): tf-sales-US-CA-2026-03")
    );
}

#[test]
fn tax_remitted_drains_payroll_liability_2150() {
    // 941 filing pulls withheld + employer-side tax out of 2150.
    let payload = json!({
        "filing_id": "tf-941-US-FEDERAL-2026-Q1",
        "kind": "payroll_941",
        "jurisdiction": "US-FEDERAL",
        "liability_account": "2150",
        "amount_cents": 1_800_000,
        "period_start": "2026-01-01",
        "period_end": "2026-03-31",
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.tax.remitted", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2150").debit_cents, 1_800_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 1_800_000i64);
}

#[test]
fn tax_remitted_rejects_unknown_liability() {
    let payload = json!({
        "filing_id": "tf-bad",
        "kind": "sales",
        "jurisdiction": "US-CA",
        "liability_account": "6100",
        "amount_cents": 100,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.tax.remitted", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn tax_accrued_books_expense_to_liability() {
    // Income-tax estimated payment: DR 6500 expense / CR 2310 liability.
    let payload = json!({
        "filing_id": "tf-income-US-FEDERAL-2026-Q1",
        "kind": "income",
        "jurisdiction": "US-FEDERAL",
        "expense_account": "6500",
        "liability_account": "2310",
        "amount_cents": 1_500_000,
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.tax.accrued", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "6500").debit_cents, 1_500_000i64);
    assert_eq!(line_for(&draft.lines, "2310").credit_cents, 1_500_000i64);
    assert_eq!(
        draft.memo.as_deref(),
        Some("Tax accrued (income US-FEDERAL): tf-income-US-FEDERAL-2026-Q1")
    );
}

#[test]
fn tax_accrued_rejects_unknown_expense_account() {
    // Only 6400 + 6500 are whitelisted as tax-expense accounts today.
    let payload = json!({
        "filing_id": "tf-bad",
        "kind": "income",
        "jurisdiction": "US-FEDERAL",
        "expense_account": "6100",
        "liability_account": "2310",
        "amount_cents": 100,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.tax.accrued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn tax_accrued_rejects_zero_amount() {
    let payload = json!({
        "filing_id": "tf-zero",
        "kind": "income",
        "jurisdiction": "US-FEDERAL",
        "expense_account": "6500",
        "liability_account": "2310",
        "amount_cents": 0,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.tax.accrued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn tax_accrued_rejects_unknown_liability() {
    let payload = json!({
        "filing_id": "tf-bad",
        "kind": "income",
        "jurisdiction": "US-FEDERAL",
        "expense_account": "6500",
        "liability_account": "1000",
        "amount_cents": 100,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.tax.accrued", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn tax_remitted_rejects_zero_amount() {
    let payload = json!({
        "filing_id": "tf-0",
        "kind": "income",
        "jurisdiction": "US-FEDERAL",
        "liability_account": "2310",
        "amount_cents": 0,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.tax.remitted", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

// --- keg deposits (93f936b9: full balance-sheet keg model) -----------------

#[test]
fn keg_deposit_charged_credits_the_liability() {
    // Fleet out: the customer's deposit is cash in, owed back — DR 1000
    // Cash / CR 2400 Keg Deposits Payable.
    let payload = json!({
        "charge_id": "keg-charge-job-1",
        "job_id": "job-1",
        "account_id": "account-00042",
        "kegs_out": 12,
        "deposit_cents": 36_000,
        "shipped_on": "2026-03-01",
    });
    let draft = evaluate(&BossRuleSet, &fact("finance.keg_deposit.charged", &payload)).unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "1000").debit_cents, 36_000i64);
    assert_eq!(line_for(&draft.lines, "2400").credit_cents, 36_000i64);
    assert_eq!(
        draft.memo.as_deref(),
        Some("Keg deposit charged (12 kegs out, account-00042): job-1")
    );
}

#[test]
fn keg_deposit_charged_missing_amount_fails() {
    let payload = json!({
        "charge_id": "keg-charge-job-2",
        "job_id": "job-2",
        "kegs_out": 4,
    });
    assert!(matches!(
        evaluate(&BossRuleSet, &fact("finance.keg_deposit.charged", &payload)),
        Err(LedgerError::InvalidPayload { .. })
    ));
}

#[test]
fn keg_deposit_charged_rejects_non_positive_inputs() {
    for (kegs_out, deposit_cents) in [(0i64, 1_000i64), (-3, 1_000), (4, 0), (4, -500)] {
        let payload = json!({
            "charge_id": "keg-charge-bad",
            "job_id": "job-bad",
            "kegs_out": kegs_out,
            "deposit_cents": deposit_cents,
        });
        assert!(
            matches!(
                evaluate(&BossRuleSet, &fact("finance.keg_deposit.charged", &payload)),
                Err(LedgerError::InvalidPayload { .. })
            ),
            "kegs_out={kegs_out} deposit_cents={deposit_cents} must be rejected"
        );
    }
}

#[test]
fn keg_deposit_released_full_return_refunds_everything() {
    // Every keg came back: the whole liability drains to a cash refund;
    // no forfeiture line at all.
    let payload = json!({
        "release_id": "keg-release-job-1",
        "job_id": "job-1",
        "account_id": "account-00042",
        "kegs_out": 12,
        "kegs_returned": 12,
        "kegs_lost": 0,
        "deposit_cents": 36_000,
        "returned_on": "2026-03-15",
    });
    let draft = evaluate(
        &BossRuleSet,
        &fact("finance.keg_deposit.released", &payload),
    )
    .unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2400").debit_cents, 36_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 36_000i64);
    assert!(
        !draft.lines.iter().any(|l| l.account_code == "4150"),
        "no forfeiture line when nothing was lost: {:?}",
        draft.lines
    );
}

#[test]
fn keg_deposit_released_partial_return_splits_refund_and_forfeiture() {
    // 7 of 10 kegs back: refund 7/10 of the deposit, forfeit the rest
    // as income. The liability drains in FULL — a lost keg's deposit is
    // the brewery's to keep, not a balance owed to nobody forever.
    let payload = json!({
        "release_id": "keg-release-job-3",
        "job_id": "job-3",
        "account_id": "account-00007",
        "kegs_out": 10,
        "kegs_returned": 7,
        "kegs_lost": 3,
        "deposit_cents": 30_000,
        "returned_on": "2026-03-20",
    });
    let draft = evaluate(
        &BossRuleSet,
        &fact("finance.keg_deposit.released", &payload),
    )
    .unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2400").debit_cents, 30_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 21_000i64);
    assert_eq!(line_for(&draft.lines, "4150").credit_cents, 9_000i64);
}

#[test]
fn keg_deposit_released_all_lost_forfeits_everything() {
    let payload = json!({
        "release_id": "keg-release-job-4",
        "job_id": "job-4",
        "kegs_out": 5,
        "kegs_returned": 0,
        "kegs_lost": 5,
        "deposit_cents": 15_000,
    });
    let draft = evaluate(
        &BossRuleSet,
        &fact("finance.keg_deposit.released", &payload),
    )
    .unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2400").debit_cents, 15_000i64);
    assert_eq!(line_for(&draft.lines, "4150").credit_cents, 15_000i64);
    assert!(
        !draft.lines.iter().any(|l| l.account_code == "1000"),
        "no refund line when nothing came back: {:?}",
        draft.lines
    );
}

#[test]
fn keg_deposit_released_rounding_remainder_lands_in_forfeiture() {
    // 1000¢ over 3 kegs doesn't divide: refund floors to 666¢ for the 2
    // returned, forfeiture absorbs the remainder (334¢) so the entry
    // balances to the cent and the liability drains exactly.
    let payload = json!({
        "release_id": "keg-release-job-5",
        "job_id": "job-5",
        "kegs_out": 3,
        "kegs_returned": 2,
        "kegs_lost": 1,
        "deposit_cents": 1_000,
    });
    let draft = evaluate(
        &BossRuleSet,
        &fact("finance.keg_deposit.released", &payload),
    )
    .unwrap();
    assert!(draft.is_balanced());
    assert_eq!(line_for(&draft.lines, "2400").debit_cents, 1_000i64);
    assert_eq!(line_for(&draft.lines, "1000").credit_cents, 666i64);
    assert_eq!(line_for(&draft.lines, "4150").credit_cents, 334i64);
}

#[test]
fn keg_deposit_released_rejects_non_conserving_counts() {
    // kegs_out = kegs_returned + kegs_lost is the fleet's conservation
    // invariant. A payload that violates it is a wrong input event —
    // reject loudly, never book a partial or inflated drain.
    for (out, returned, lost) in [(1i64, 1i64, 1i64), (10, 4, 3), (3, 4, -1)] {
        let payload = json!({
            "release_id": "keg-release-bad",
            "job_id": "job-bad",
            "kegs_out": out,
            "kegs_returned": returned,
            "kegs_lost": lost,
            "deposit_cents": 3_000,
        });
        assert!(
            matches!(
                evaluate(
                    &BossRuleSet,
                    &fact("finance.keg_deposit.released", &payload)
                ),
                Err(LedgerError::InvalidPayload { .. })
            ),
            "out={out} returned={returned} lost={lost} must be rejected"
        );
    }
}

#[test]
fn keg_deposit_released_missing_counts_fail() {
    let payload = json!({
        "release_id": "keg-release-job-6",
        "job_id": "job-6",
        "deposit_cents": 3_000,
    });
    assert!(matches!(
        evaluate(
            &BossRuleSet,
            &fact("finance.keg_deposit.released", &payload)
        ),
        Err(LedgerError::InvalidPayload { .. })
    ));
}
