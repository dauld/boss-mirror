//! General Ledger — chart of accounts + posting rules + journal projection
//! over `financial_facts`.
//!
//! Decision record: `docs/architecture-decisions.md` §Finance &
//! ledger. The GL is a
//! projection of immutable financial facts, not a separate system of record.
//! Rules are pure functions from facts to balanced debit/credit entries;
//! the projection is rebuildable from facts at will.
//!
//! v1b ships:
//! - domain types (`types`)
//! - rule evaluation (`rules`) — RuleSet v1 covers AR + AP lifecycle
//! - postgres write path (`postgres`, under the `postgres` feature)
//!   that commerce and inventory call synchronously inside their
//!   write transactions

pub mod chart;
pub mod error;
pub mod excise;
pub mod posting_rules;
pub mod recognize;
pub mod rules;
pub mod types;

// Kani bounded-model-checker proofs. The module compiles under both
// `cargo build` and `cargo kani`, but only `cargo kani` runs the
// `#[kani::proof]` functions inside; the helpers themselves are
// harmless in a normal build.
pub mod kani_proofs;

#[cfg(feature = "postgres")]
pub mod bank_settlements;
#[cfg(feature = "postgres")]
pub mod bills;
#[cfg(feature = "postgres")]
pub mod config;
#[cfg(feature = "postgres")]
pub mod events;
#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod payroll;
#[cfg(feature = "postgres")]
pub mod periods;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
#[cfg(feature = "postgres")]
pub mod rebuild_bank_settlements;
#[cfg(feature = "postgres")]
pub mod rebuild_facts;
#[cfg(feature = "postgres")]
pub mod rebuild_payroll;
#[cfg(feature = "postgres")]
pub mod rebuild_tax_filings;
#[cfg(feature = "postgres")]
pub mod replay_check;
#[cfg(feature = "postgres")]
pub mod supersede;
#[cfg(feature = "postgres")]
pub mod tax_filings;

pub use error::LedgerError;
pub use posting_rules::{DataRuleSet, PostingRule, PostingRuleInput};
pub use rules::{BossRuleSet, RuleSet, evaluate, revenue_accounts_map};
pub use types::{
    AccountCode, AccountKind, FactRef, JournalEntryDraft, JournalLineDraft, NormalSide,
};

#[cfg(feature = "postgres")]
pub use postgres::post_fact_in_tx;

#[cfg(feature = "postgres")]
pub use rebuild::{RebuildReport, rebuild};

#[cfg(feature = "postgres")]
pub use rebuild_facts::{
    ProjectedFact, ProjectionError, ProjectionRule, RebuildFactsReport, project_event,
    rebuild_facts, rebuild_facts_in_tx,
};

#[cfg(feature = "postgres")]
pub use rebuild_payroll::{RebuildPayrollReport, rebuild_payroll, rebuild_payroll_in_tx};

#[cfg(feature = "postgres")]
pub use rebuild_tax_filings::{
    RebuildTaxFilingsReport, rebuild_tax_filings, rebuild_tax_filings_in_tx,
};

#[cfg(feature = "postgres")]
pub use rebuild_bank_settlements::{
    RebuildBankSettlementsReport, rebuild_bank_settlements, rebuild_bank_settlements_in_tx,
};

#[cfg(feature = "postgres")]
pub use replay_check::{
    DeepReplayCheckReport, Divergence, EntryShape, FactDivergence, FactKey, FactShape, LineShape,
    ReplayCheckReport, replay_check, replay_check_from_audit_log,
};

#[cfg(feature = "postgres")]
pub use supersede::{
    SupersedeOutcome, SupersedeRequest, apply_supersede_in_tx, replay_supersede_events_in_tx,
};

#[cfg(feature = "postgres")]
pub use events::{FactWrite, deterministic_fact_id, record_fact_in_tx, record_ledger_event_in_tx};
