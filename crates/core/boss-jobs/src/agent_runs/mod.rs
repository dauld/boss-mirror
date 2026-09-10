//! Agent-run record — what an actor's run cost, held by the system.
//!
//! The actors run it (CLAUDE.md §Reading frame): humans and registered
//! agents are the CPUs, and *"actors are not users of the system; they
//! are the part of it that executes"*. Until this module existed, an
//! agent CPU had no instrumentation at all. On 2026-09-10 four coding
//! agents spent ~761,000 tokens and ~58 minutes producing four cars
//! that will land on main; the cars are on the record and their cost
//! existed only in a terminal transcript that dies with the session.
//! Backlog 83344e16.
//!
//! Hexagonal, the same shape as `crate::cadence`: domain types + port
//! trait + Pg adapter + in-memory adapter + HTTP door, plus a
//! projection rebuilder because here the log — not the row — is the
//! system of record.
//!
//! **The rate card is read-only over HTTP, and that is a decision.**
//! A price is a row, as the packet asked, but it is a row that arrives
//! by migration, not by API write. The evidence is two days old: the
//! dispatcher rule registry accepts live authoring, and on 2026-09-10
//! the running system enforced 64 rules while the tree held 60 files —
//! four rows that exist in no file and carry no stated reason (backlog
//! 8d471ec5). A rate card that can be edited live would fail the same
//! way, and a wrong price is harder to notice than a missing rule. An
//! authoring surface can follow if editing without a deploy turns out
//! to be worth that risk; the part that mattered — pricing is DATA and
//! not a `match` in Rust — is already true.
//!
//! **Not in scope, deliberately.** Two sibling packets border this one
//! and this module is additive to both:
//!   - `7dd9f28c` — AgentSpec and BudgetDecision have no registry.
//!     `boss_core::port::CostLedger::check_budget` is where a budget is
//!     consulted BEFORE a run; this module records runs AFTER. Its
//!     `spent(agent, window)` question is this module's
//!     `list_runs(actor_id, since)` + `summarize`, which is the join
//!     point: a budget can be spent against these rows.
//!   - `be025b44` — a car's packet is filed only at gate-green, so the
//!     build is invisible while it happens. When a builder opens its
//!     car at build START, that packet's id is what
//!     [`types::NewAgentRun::job_id`] should carry, and
//!     [`types::NewAgentRun::branch`] is the bridge until then: a
//!     branch is what a builder knows before any packet exists for it.

pub mod events;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

pub use events::AGENT_RUN_RECORDED;
pub use in_memory::InMemoryAgentRuns;
pub use port::{AgentRunError, AgentRunLog, RecordedRun};
#[cfg(feature = "postgres")]
pub use postgres::PgAgentRuns;
#[cfg(feature = "postgres")]
pub use rebuild::rebuild_agent_runs;
pub use types::{
    AgentRun, GroupSpend, NewAgentRun, RateCardRow, RunFilter, RunOutcome, RunSummary, price_run,
    summarize,
};
