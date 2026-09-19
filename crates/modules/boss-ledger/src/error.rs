//! Ledger errors. Surface whatever breaks in rule evaluation or persistence
//! cleanly enough that a failing fact post aborts the surrounding domain
//! transaction with an actionable message.

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// No posting rule is registered for this fact kind. Either the fact
    /// is new and needs a rule added, or it doesn't belong in the ledger.
    #[error("no posting rule for fact kind `{0}`")]
    UnknownFactKind(String),

    /// The rule produced a draft entry whose debits don't equal its credits.
    /// Rules are required to be balanced; if this ever fires the rule is
    /// buggy and needs a test.
    #[error("rule for `{kind}` produced unbalanced entry: debits={debits} credits={credits}")]
    Unbalanced {
        kind: String,
        debits: i64,
        credits: i64,
    },

    /// The fact payload was missing a field the rule needed, or a field
    /// had the wrong type. Also fires for malformed payloads.
    #[error("fact `{kind}` payload invalid: {reason}")]
    InvalidPayload { kind: String, reason: String },

    /// Rule referenced an account code that isn't in the chart. Either
    /// the starter seed is incomplete, or the rule targets an account
    /// that's been retired.
    #[error("unknown account code `{0}`")]
    UnknownAccount(String),

    /// A fact's business date falls in a period that has been locked.
    /// The correct remediation is to emit a correction fact dated in the
    /// current open period, not to unlock the historical period.
    #[error("cannot post fact with happened_on={happened_on}: period {period_id} is locked")]
    LockedPeriod {
        period_id: uuid::Uuid,
        happened_on: chrono::NaiveDate,
    },

    /// A chart-of-accounts batch that fails the door's own validation
    /// (`chart::validate`: an unknown kind or normal_balance, a
    /// duplicated code, a parent not declared before its child) —
    /// a caller error naming the row, refused before any row is
    /// written (backlog 41af5195).
    #[error("chart of accounts refused: {0}")]
    InvalidChart(String),

    /// A tax batch that fails the door's own validation
    /// (`tax_registry::validate`: a kind declared twice, a state that
    /// is not a two-letter code, a rate outside the CHECK) or names an
    /// account `gl_accounts` does not hold — a caller error naming the
    /// row, refused before any row is written (backlog 7f163e58).
    #[error("tax seed refused: {0}")]
    InvalidTaxSeed(String),

    /// A tax fact (`finance.tax.accrued` / `finance.tax.remitted`) names
    /// a kind this instance's `tax_kinds` registry does not hold, or a
    /// liability account other than the one that kind's row names —
    /// the tenant's declaration (`POST /api/ledger/tax/batch`) is the
    /// one definition of which account a kind hits, and a kind with no
    /// row cannot post (backlog e021be29: until 2026-09-19 the posting
    /// rules held the demo tenant's four accounts in a `matches!` and
    /// refused every other tenant's as "not allowed").
    #[error("fact `{fact_kind}` refused: {reason}")]
    TaxKindNotRegistered { fact_kind: String, reason: String },

    /// Postgres failure during insert, lookup, or trigger check.
    #[error("storage failure: {0}")]
    Storage(String),
}
