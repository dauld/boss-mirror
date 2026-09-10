//! The agent-run log port — three operations and nothing else.
//!
//! UNLIKE the cadence registry next door, this surface DOES record an
//! event, and for a reason cadence names: `cadence_firings` is its own
//! measurement record and a parallel event stream would duplicate it.
//! Here the row is a PROJECTION — `agents.run.recorded` carries the
//! full run state, the row is rebuilt from it
//! (`super::rebuild::rebuild_agent_runs`), and the write is one
//! transaction so the log and the projection commit together. That is
//! the audit-log-is-the-system-of-record contract, not a second copy.

use async_trait::async_trait;

use super::types::{AgentRun, NewAgentRun, RateCardRow, RunFilter};

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

/// What a record attempt did. `recorded: false` means this `run_id` was
/// already on the log — a retried report, not a failure — and the
/// returned run is the one already held, so the caller sees the
/// authoritative record either way.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRun {
    pub recorded: bool,
    pub run: AgentRun,
}

#[async_trait]
pub trait AgentRunLog: Send + Sync {
    /// Record one FINISHED agent run, pricing it off the rate card in
    /// the same transaction that writes the row and the event.
    ///
    /// Finished, deliberately: this surface is the record of what a run
    /// cost, which is not knowable until it ends. Making the work
    /// visible WHILE it happens is a different fact with a different
    /// home (backlog be025b44 — the builder opens its car at build
    /// start); a half-filled row here would answer "what did it cost"
    /// with a number that was still moving.
    ///
    /// Idempotent on `run_id`, so the reporter can retry a failed
    /// report without inventing a second run.
    ///
    /// `recorded_by` is who FILED the record — usually the dispatching
    /// session, sometimes the agent itself. It rides the event as
    /// `_actor` and is not the same thing as the run's own `actor_id`,
    /// which is the CPU that did the work.
    async fn record_run(
        &self,
        run: &NewAgentRun,
        recorded_by: &boss_core::actor::ActorId,
    ) -> Result<RecordedRun, AgentRunError>;

    /// Matching runs, newest finish first.
    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<AgentRun>, AgentRunError>;

    /// The rate card, model-ordered. Read-only on purpose — see
    /// `super::mod`'s doc comment for why a price is a tree change.
    async fn rate_card(&self) -> Result<Vec<RateCardRow>, AgentRunError>;
}

/// Reject what cannot be a run before anything is written. Shared by
/// both adapters so a Postgres caller and an in-memory caller are
/// refused for the same reasons, and each refusal names its fix.
pub fn validate(run: &NewAgentRun) -> Result<(), AgentRunError> {
    if run.run_id.trim().is_empty() {
        return Err(AgentRunError::BadRequest(
            "run_id is required — it is the idempotency key, so the caller must mint it".into(),
        ));
    }
    if run.finished_at < run.started_at {
        return Err(AgentRunError::BadRequest(format!(
            "finished_at ({}) is before started_at ({}) — a run cannot end before it starts",
            run.finished_at, run.started_at
        )));
    }
    if !run.actor_id.is_agent() {
        return Err(AgentRunError::BadRequest(format!(
            "actor_id {:?} is not an agent — an agent run must name its CPU in the \
             <mode>:<model> form (e.g. \"claude:opus-5\"), because the model is read \
             out of the actor id and nowhere else",
            run.actor_id.to_string()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runs::types::RunOutcome;
    use boss_core::actor::ActorId;

    fn ok_run() -> NewAgentRun {
        NewAgentRun {
            run_id: "run-1".into(),
            actor_id: ActorId::agent("claude", "opus-5"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
            tokens: crate::agent_runs::types::TokenUsage::Split {
                input: 1,
                output: 1,
            },
            tool_calls: 1,
            job_id: None,
            branch: None,
            detail: serde_json::Value::Null,
        }
    }

    #[test]
    fn a_well_formed_run_passes() {
        assert!(validate(&ok_run()).is_ok());
    }

    #[test]
    fn an_empty_run_id_is_refused_and_says_why() {
        let mut run = ok_run();
        run.run_id = "   ".into();
        let err = validate(&run).unwrap_err().to_string();
        assert!(err.contains("run_id"), "{err}");
        assert!(err.contains("idempotency"), "{err}");
    }

    #[test]
    fn a_run_that_ends_before_it_starts_is_refused() {
        let mut run = ok_run();
        run.finished_at = run.started_at - chrono::Duration::seconds(1);
        assert!(validate(&run).is_err());
    }

    #[test]
    fn a_zero_length_run_is_allowed() {
        // A run that started and finished inside the same second is
        // odd but real (an immediate refusal, a cancelled dispatch).
        let mut run = ok_run();
        run.finished_at = run.started_at;
        assert!(validate(&run).is_ok());
    }

    #[test]
    fn a_non_agent_actor_is_refused_and_names_the_expected_form() {
        let mut run = ok_run();
        run.actor_id = ActorId::automation("train-conductor");
        let err = validate(&run).unwrap_err().to_string();
        assert!(err.contains("<mode>:<model>"), "{err}");
    }

    #[test]
    fn a_human_actor_is_refused_too() {
        let mut run = ok_run();
        run.actor_id = ActorId::human("emp-032");
        assert!(validate(&run).is_err());
    }

    #[test]
    fn ports_are_object_safe() {
        fn takes<T: ?Sized>() {}
        takes::<dyn AgentRunLog>();
    }
}
