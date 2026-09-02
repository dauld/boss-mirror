#![cfg(kani)]
//! Kani proofs for the ledger balance invariant.
//!
//! `cargo kani -p boss-ledger` runs these under the Kani bounded model
//! checker — each `#[kani::proof]` is explored exhaustively (within
//! the stated bounds) and produces a machine-checked guarantee rather
//! than a sampled one. The whole module is `#[cfg(kani)]` so regular
//! `cargo build` / `cargo test` doesn't compile it.
//!
//! Target: per-rule balance of `JournalEntryDraft`. Every rule in
//! `rules.rs` must produce an entry where `total_debits() ==
//! total_credits()`. The top-level `evaluate` wrapper enforces this
//! at runtime with an error path; Kani lifts that runtime check into
//! a compile-time-ish proof for the pure arithmetic of each rule
//! shape we care about.
//!
//! Why these proofs exist separately from the rules: Kani works best
//! on concrete integer arithmetic. Each proof reconstructs the line-
//! emission logic of one rule kernel directly from its inputs,
//! without serde_json. If the rule is edited, the proof must be
//! edited too — catching the drift is the point.

use crate::types::{JournalEntryDraft, JournalLineDraft};
use chrono::NaiveDate;

fn posted_on() -> NaiveDate {
    // Kani doesn't need the date to vary for the balance invariant —
    // only the line amounts matter. Pin it to a concrete value so
    // CBMC has less to reason about.
    NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
}

// ---------------------------------------------------------------------------
// Shape 1: simple two-line entry (one debit + one credit of the same amount)
//
// Matches invoice_paid, payment_received, payment_settled — the "move
// cash between two accounts" shape. Balance is trivial by construction
// (debit == credit == amount) but naming it as a proof locks the
// contract in: any rule added with this shape must pass this proof.
// ---------------------------------------------------------------------------

fn build_two_line(amount: i64) -> JournalEntryDraft {
    // Account codes are fixed static literals — the proof only cares
    // about the numeric balance, not which accounts are touched.
    JournalEntryDraft {
        posted_on: posted_on(),
        memo: None,
        lines: vec![
            JournalLineDraft::debit("1000", amount, 0),
            JournalLineDraft::credit("1100", amount, 1),
        ],
    }
}

#[kani::proof]
#[kani::unwind(3)]
fn proof_two_line_entry_is_balanced() {
    let amount: i64 = kani::any();
    // Rules reject negative amounts upstream; same bound here.
    kani::assume(amount >= 0);
    // Cap well below i64::MAX/2 so sums can't overflow under any
    // future compound rule that adds multiple such pairs.
    kani::assume(amount <= 1_000_000_000_000);

    let draft = build_two_line(amount);
    assert!(draft.is_balanced());
    assert_eq!(draft.total_debits(), amount);
    assert_eq!(draft.total_credits(), amount);
}

// ---------------------------------------------------------------------------
// Shape 2: payroll compound entry
//
// Matches payroll_run. Four possible lines:
//   DR 6100 gross               (always present)
//   DR 6400 employer_tax        (only if > 0)
//   CR 2150 liability           (only if > 0, = withheld + employer_tax)
//   CR 1000 net                 (always present, = gross - withheld)
//
// Balance invariant (algebra on the possibly-conditional lines):
//   gross + employer_tax  ==  (withheld + employer_tax) + (gross - withheld)
//   gross + employer_tax  ==  gross + employer_tax                          ✓
//
// The proof captures that ANY (gross, withheld, employer_tax) triple
// satisfying the runtime preconditions (non-negative, withheld ≤
// gross) produces a balanced entry, regardless of which conditional
// lines are present.
// ---------------------------------------------------------------------------

fn build_payroll(gross: i64, withheld: i64, employer_tax: i64) -> JournalEntryDraft {
    let net = gross - withheld;
    let liability = withheld + employer_tax;
    let mut lines = Vec::with_capacity(4);
    lines.push(JournalLineDraft::debit("6100", gross, 0));
    if employer_tax > 0 {
        lines.push(JournalLineDraft::debit("6400", employer_tax, 1));
    }
    if liability > 0 {
        lines.push(JournalLineDraft::credit("2150", liability, 2));
    }
    lines.push(JournalLineDraft::credit("1000", net, 3));
    JournalEntryDraft {
        posted_on: posted_on(),
        memo: None,
        lines,
    }
}

// Parked: this proof passes `Runtime Symex` but CBMC then
// enumerates every allocation-failure path through
// `Vec::with_capacity` + `push` and times out before the balance
// assertion itself gets checked. Not a real counter-example — a
// Kani friction with heap-allocating proofs. Rewrite using a
// fixed-size `[JournalLineDraft; 4]` to eliminate the grow paths,
// or use `kani::Arbitrary::any_array` once we bump the nightly
// toolchain. Uncomment the `#[kani::proof]` + `#[kani::unwind]`
// attributes when that rewrite lands.
// #[kani::proof]
// #[kani::unwind(5)]
#[allow(dead_code)]
fn proof_payroll_entry_is_balanced() {
    let gross: i64 = kani::any();
    let withheld: i64 = kani::any();
    let employer_tax: i64 = kani::any();
    // Rule preconditions — `payroll_run` enforces these and returns
    // an error otherwise, so the proof holds on the happy path.
    kani::assume(gross >= 0);
    kani::assume(withheld >= 0);
    kani::assume(employer_tax >= 0);
    kani::assume(withheld <= gross);
    // Bound each amount so CBMC's arithmetic stays tractable (bits-
    // per-variable drives solve time). 10^10 cents = $100M is well
    // above any real payroll run.
    kani::assume(gross <= 10_000_000_000);
    kani::assume(employer_tax <= 10_000_000_000);

    let draft = build_payroll(gross, withheld, employer_tax);
    assert!(draft.is_balanced());
    assert_eq!(draft.total_debits(), gross + employer_tax);
    assert_eq!(draft.total_credits(), gross + employer_tax);
}

// ---------------------------------------------------------------------------
// Shape 3: AR recognition with tax lines
//
// Matches the invoice_issued kernel once the per-line amounts have
// been grouped by category. The rule's algebra (after payload parsing):
//   debit(A/R, revenue_total + tax_total)
//   credit(revenue_account_X, sum_of_category_X)  // per distinct category
//   credit(tax_account, amount)                   // per non-zero tax line
//
// Balance: revenue_total + tax_total on debits == sum of by_category
// credits + sum of tax_credit amounts. Since by_category exhausts
// revenue (sum of all categories == revenue_total) and tax_total
// exhausts the tax lines (sum of all nonzero amounts == tax_total),
// balance holds.
//
// We bound the arrays at 4 categories + 4 tax lines — more than covers
// today's emitters and keeps the solver happy.
// ---------------------------------------------------------------------------

const MAX_CATS: usize = 4;
const MAX_TAXES: usize = 4;
const PER_AMOUNT_CAP: i64 = 1_000_000_000; // $10M per line is well above reality

fn build_invoice_issued(cats: [i64; MAX_CATS], taxes: [i64; MAX_TAXES]) -> JournalEntryDraft {
    let revenue_total: i64 = cats.iter().sum();
    let tax_total: i64 = taxes.iter().sum();
    let ar_total = revenue_total + tax_total;

    let mut lines = Vec::with_capacity(1 + MAX_CATS + MAX_TAXES);
    lines.push(JournalLineDraft::debit("1100", ar_total, 0));
    let mut sort: i16 = 1;
    // Distinct revenue accounts per "category index" — mirrors the
    // rule's BTreeMap grouping by category.
    let revenue_accounts = ["4100", "4110", "4120", "4130"];
    for (i, amount) in cats.iter().enumerate() {
        if *amount > 0 {
            lines.push(JournalLineDraft::credit(revenue_accounts[i], *amount, sort));
            sort += 1;
        }
    }
    // All tax lines post to 2300 today — the rule's allow-list.
    for amount in taxes {
        if amount > 0 {
            lines.push(JournalLineDraft::credit("2300", amount, sort));
            sort += 1;
        }
    }

    JournalEntryDraft {
        posted_on: posted_on(),
        memo: None,
        lines,
    }
}

// Parked: same CBMC heap-allocation blowup as
// proof_payroll_entry_is_balanced. Rewrite with fixed-size arrays
// to revive. See comment above that proof for the plan.
// #[kani::proof]
// #[kani::unwind(10)]
#[allow(dead_code)]
fn proof_invoice_issued_entry_is_balanced() {
    let mut cats = [0i64; MAX_CATS];
    let mut taxes = [0i64; MAX_TAXES];
    for c in cats.iter_mut() {
        *c = kani::any();
        kani::assume(*c >= 0 && *c <= PER_AMOUNT_CAP);
    }
    for t in taxes.iter_mut() {
        *t = kani::any();
        kani::assume(*t >= 0 && *t <= PER_AMOUNT_CAP);
    }
    // Rule rejects invoices with zero line items.
    let any_cat_positive = cats.iter().any(|c| *c > 0);
    kani::assume(any_cat_positive);

    let draft = build_invoice_issued(cats, taxes);
    assert!(draft.is_balanced());
    let expected: i64 = cats.iter().sum::<i64>() + taxes.iter().sum::<i64>();
    assert_eq!(draft.total_debits(), expected);
    assert_eq!(draft.total_credits(), expected);
}

// ---------------------------------------------------------------------------
// Shape 4: total_debits / total_credits / is_balanced are consistent
//
// Cheap algebraic property on the `JournalEntryDraft` helpers
// themselves — if we ever change their implementation, Kani flags
// the drift. Fix a small bound on line count so CBMC stays fast.
// ---------------------------------------------------------------------------

#[kani::proof]
#[kani::unwind(5)]
fn proof_total_debits_plus_credits_never_panics() {
    const N: usize = 3;
    let mut lines: Vec<JournalLineDraft> = Vec::with_capacity(N);
    for i in 0..N {
        let debit: i64 = kani::any();
        let credit: i64 = kani::any();
        kani::assume(debit >= 0 && debit <= 1_000_000_000);
        kani::assume(credit >= 0 && credit <= 1_000_000_000);
        lines.push(JournalLineDraft {
            account_code: "1000".into(),
            debit_cents: debit,
            credit_cents: credit,
            memo: None,
            sort_order: i as i16,
        });
    }
    let draft = JournalEntryDraft {
        posted_on: posted_on(),
        memo: None,
        lines,
    };
    let d = draft.total_debits();
    let c = draft.total_credits();
    assert_eq!(draft.is_balanced(), d == c);
}

// ---------------------------------------------------------------------------
// Shape 5: keg-deposit release split (one debit, remainder-absorbing
// credit pair)
//
// Matches keg_deposit_released: DR 2400 deposit; CR 1000 refund =
// deposit × returned / out (integer floor); CR 4150 forfeited =
// deposit − refund. Balance holds by construction (the two credits
// are defined to sum to the debit); the load-bearing facts to prove
// are that BOTH credits are non-negative under the rule's
// preconditions — a negative line would trip the DB's per-line CHECK
// — and that the i128 intermediate keeps the product from wrapping.
// Arithmetic-only on purpose: the heap-building proofs above are
// parked on CBMC allocation blowup, and the vec adds nothing here.
// ---------------------------------------------------------------------------

#[kani::proof]
fn proof_keg_deposit_release_split_conserves() {
    let deposit: i64 = kani::any();
    let out: i64 = kani::any();
    let returned: i64 = kani::any();
    // Rule preconditions (keg_deposit_released rejects everything
    // else): positive deposit, positive fleet, returned within the
    // fleet (lost = out − returned makes conservation implicit).
    kani::assume(deposit > 0 && deposit <= 1_000_000_000_000);
    kani::assume(out > 0 && out <= 1_000_000);
    kani::assume(returned >= 0 && returned <= out);

    let refund = ((deposit as i128 * returned as i128) / out as i128) as i64;
    let forfeited = deposit - refund;

    // The floor keeps the refund inside the deposit…
    assert!(refund >= 0);
    assert!(refund <= deposit);
    // …so the remainder credit can never go negative…
    assert!(forfeited >= 0);
    // …and the two credits drain the debit exactly.
    assert_eq!(refund + forfeited, deposit);
    // Edge honesty: full return refunds everything, so no forfeiture
    // line survives the zero-line filter.
    if returned == out {
        assert_eq!(forfeited, 0);
    }
}
