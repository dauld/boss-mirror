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
//! **The budget is consulted here, at the record (backlog 7dd9f28c).**
//! `boss_core::agent::BudgetDecision::decide` is the ONE rule — the
//! cybernetics ledger (`boss_events::ledger`) and this recorder both
//! call it — and the caps it judges against are the `agents` row's
//! (design 6fda05ae gave the actor one id and this module's `model`
//! column priced the run against what it actually ran, which is what
//! made the join possible). The recorder measures the actor's priced
//! spend in the hour before the run STARTED plus its runs in flight at
//! that instant (`types::measure_load`), admits or refuses
//! (`port::admit`), and writes the decision down: an `Allow` rides the
//! row and the event as `budget`, a `Deny` is its own event
//! (`agents.run.denied`) and no row. So a refusal is as visible as
//! spend, which is the whole point of the value-shaped decision: on
//! 2026-09-08 a session ran out of credit and the only signal was the
//! work stopping. HONEST LIMIT: this record is written at FINISH, so
//! "admitted" here is a judgement of a run that already happened — the
//! refusal is a fact the desk can act on, not a gate that stopped the
//! spend. Stopping it needs a run that opens at start (below).
//!
//! **Not in scope, deliberately.** One sibling packet borders this one
//! and this module is additive to it:
//!   - `be025b44` — a car's packet is filed only at gate-green, so the
//!     build is invisible while it happens. When a builder opens its
//!     car at build START, that packet's id is what
//!     [`types::NewAgentRun::job_id`] should carry, and
//!     [`types::NewAgentRun::branch`] is the bridge until then: a
//!     branch is what a builder knows before any packet exists for it.

/// The metadata key naming the agent run that is EXECUTING a step, or
/// that a gate-run / car rides under — the `link` every landing rule
/// follows (`jobs.complete_linked_step`). One spelling, here, because
/// four readers depend on it agreeing: `boss gate` stamps it on a
/// gate-run, `boss dispatch` writes it onto the step it claims
/// (backlog dd6d44b7), the dispatcher rules read it back off the
/// `step.done.<kind>` marker, and the step PUT carries it forward
/// (backlog b91a2103) so a wholesale metadata write cannot erase it.
pub const EDGE_KEY: &str = "agent_run";

/// Whether a claim hands a step to a DIFFERENT holder — the one case in
/// which the claim clears [`EDGE_KEY`] (backlog 9562f6df).
///
/// Freshness of the edge belongs at the claim: a step that came free
/// with the previous run still named, and was then claimed outside
/// `boss dispatch` (a person in the UI), kept naming that run, and the
/// delivery rule would land it off a completion it did not do. Both
/// release doors — `boss step release` and
/// `jobs.reclaim_abandoned_step` — already null the edge as they free
/// the step; this is the same decision for every door that frees a step
/// WITHOUT nulling it, taken where the executor actually changes.
///
/// NOT on every claim: a re-claim by the holder is idempotent by design,
/// and the holder spelled by one of its aliases (design 6fda05ae) is the
/// holder — the CAS respelling it is not a change of executor.
/// `boss dispatch` claims first and writes the new run's edge after, so
/// clearing here never races the edge a dispatch is about to write.
pub fn claim_changes_holder(old_holder: Option<&str>, claimant: &str, aliases: &[String]) -> bool {
    match old_holder {
        None => true,
        Some(h) => h != claimant && !aliases.iter().any(|a| a == h),
    }
}

/// `metadata` without the run edge — every other key untouched.
pub fn without_edge(metadata: &serde_json::Value) -> serde_json::Value {
    match metadata.as_object() {
        Some(obj) => serde_json::Value::Object(
            obj.iter()
                .filter(|(k, _)| k.as_str() != EDGE_KEY)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => metadata.clone(),
    }
}

pub mod events;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

pub use events::{AGENT_RUN_DENIED, AGENT_RUN_RECORDED};
pub use in_memory::InMemoryAgentRuns;
pub use port::{AgentRunError, AgentRunLog, RecordedRun, RegisteredAgent, admit};
#[cfg(feature = "postgres")]
pub use postgres::PgAgentRuns;
#[cfg(feature = "postgres")]
pub use rebuild::rebuild_agent_runs;
pub use types::{
    ADMISSION_WINDOW, AgentRun, AgentRunView, EFFORT_APPLIED_FROM_SHA, EFFORT_RECORDED_FROM_SHA,
    EffortEra, GroupSpend, NewAgentRun, PricingBasis, RateCardRow, RunFilter, RunOutcome,
    RunSummary, TokenUsage, effort_applied_from, measure_load, price_run, pricing_basis, summarize,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The three holders a claim can meet (9562f6df): nobody, the
    /// claimant under either spelling, and someone else.
    #[test]
    fn only_a_different_holder_is_a_change() {
        let aliases = vec!["claude@algedonic.dev".to_string()];
        assert!(claim_changes_holder(None, "agent-claude", &aliases));
        assert!(!claim_changes_holder(
            Some("agent-claude"),
            "agent-claude",
            &aliases
        ));
        assert!(!claim_changes_holder(
            Some("claude@algedonic.dev"),
            "agent-claude",
            &aliases
        ));
        assert!(claim_changes_holder(
            Some("emp-someone"),
            "agent-claude",
            &aliases
        ));
    }

    #[test]
    fn without_edge_drops_the_edge_and_nothing_else() {
        let md = serde_json::json!({ EDGE_KEY: "run-1", "notes": "kept" });
        assert_eq!(without_edge(&md), serde_json::json!({ "notes": "kept" }));
        assert_eq!(
            without_edge(&serde_json::Value::Null),
            serde_json::Value::Null
        );
    }
}
