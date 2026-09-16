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
use boss_core::agent::{AgentCaps, AgentLoad, BudgetDecision};

use super::types::{AgentRun, NewAgentRun, RateCardRow, RunFilter};

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("bad request: {0}")]
    BadRequest(String),
    /// The run was refused against its actor's budget (backlog
    /// 7dd9f28c). Its own class, not a `BadRequest`: the report was
    /// well-formed, the actor was over its cap, and the refusal is
    /// already a fact on the log (`agents.run.denied`) by the time the
    /// caller sees this.
    #[error("budget denied: {reason}")]
    Denied { reason: String },
    #[error("storage: {0}")]
    Storage(String),
}

/// What the `agents` row says about the actor a run names — the two
/// columns the recorder reads. `None` when the actor has no row: a
/// legacy colon-form id, or a registered id nobody registered (which
/// `resolve_model` refuses on its own grounds when the run names no
/// model either).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisteredAgent {
    pub default_model: String,
    pub caps: AgentCaps,
}

/// The admission step both adapters run between resolving the model
/// and writing anything: the actor's caps against its measured load,
/// through the one rule (`BudgetDecision::decide`). An actor with no
/// row is unbudgeted — `AgentCaps::default()`, every cap `None` — and
/// is admitted with nothing to count down; a missing row must not stop
/// the stack.
pub fn admit(agent: Option<&RegisteredAgent>, load: AgentLoad) -> BudgetDecision {
    BudgetDecision::decide(agent.map(|a| a.caps).unwrap_or_default(), load)
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
    /// Admitted against the actor's budget first (backlog 7dd9f28c):
    /// the actor's registry caps against its priced spend in the hour
    /// before the run started and its runs in flight at that instant
    /// ([`super::types::measure_load`]), judged by [`admit`]. An
    /// `Allow` rides the row and the event as `budget`; a `Deny` is
    /// written to the log as `agents.run.denied` — actor, window,
    /// spend, cap, reason, and what the refused run itself cost — and
    /// answered as [`AgentRunError::Denied`] with no row written. Each
    /// refused attempt is its own event: a retry of a refused report
    /// is a second ask, and is refused (or admitted, if the hour has
    /// rolled) on its own measurement.
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
            "actor_id {:?} is not an agent — an agent run must name its CPU as a \
             registered agent id (e.g. \"agent-claude\", with the model in the body's \
             `model` key or defaulted from the agents registry) or in the legacy \
             <mode>:<model> form (e.g. \"claude:opus-5\")",
            run.actor_id.to_string()
        )));
    }
    if run.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
        return Err(AgentRunError::BadRequest(
            "model is blank — name the model this run ran on (as agent_rate_card spells \
             it, e.g. \"opus-5[1m]\"), or leave the key out to take the agent's default"
                .into(),
        ));
    }
    Ok(())
}

/// The model a run is recorded with — the resolve half of the rule
/// [`NewAgentRun::model`] reads: the report's own word first; else
/// what the actor id already says (the model half of a legacy colon
/// form — a registered id says nothing); else the registered agent's
/// `default_model` (`registry_default`, read by the adapter from the
/// `agents` row for `run.actor_id`). Resolved ONCE, here, and
/// written to the row and the event, so a rebuild replays the model
/// the run was recorded with rather than re-asking a registry whose
/// default may since have changed — the same reason the price is
/// replayed and not recomputed.
///
/// A run that names no model by any of the three is refused, naming
/// both fixes: a run that cannot say what it ran on is not a record of
/// what it cost, and a registered id no `agents` row backs is the
/// other half of the same gap.
pub fn resolve_model(
    run: &NewAgentRun,
    registry_default: Option<&str>,
) -> Result<String, AgentRunError> {
    run.model()
        .or(registry_default)
        .map(str::to_string)
        .ok_or_else(|| {
            AgentRunError::BadRequest(format!(
                "actor {:?} names no model and no agents row supplies a default — send \
                 `model` in the report, or register the agent (an `agents` row with a \
                 default_model) so a run that does not say can take its default",
                run.actor_id.to_string()
            ))
        })
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
            model: None,
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

    fn registered_run(model: Option<&str>) -> NewAgentRun {
        NewAgentRun {
            actor_id: ActorId::RegisteredAgent("agent-claude".into()),
            model: model.map(str::to_string),
            ..ok_run()
        }
    }

    #[test]
    fn a_well_formed_run_passes() {
        assert!(validate(&ok_run()).is_ok());
    }

    #[test]
    fn a_registered_agent_is_a_well_formed_actor() {
        assert!(validate(&registered_run(None)).is_ok());
    }

    #[test]
    fn a_blank_model_is_refused_and_names_the_two_fixes() {
        let err = validate(&registered_run(Some("  ")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("blank"), "{err}");
        assert!(err.contains("leave the key out"), "{err}");
    }

    // -- resolve_model: the one rule, in the order it is applied --------

    #[test]
    fn the_reports_own_word_wins() {
        let model =
            resolve_model(&registered_run(Some("haiku-4-5")), Some("opus-5")).expect("resolves");
        assert_eq!(
            model, "haiku-4-5",
            "the default is a fallback, not an override"
        );
    }

    #[test]
    fn a_registered_agent_that_does_not_say_takes_the_registry_default() {
        let model = resolve_model(&registered_run(None), Some("opus-5")).expect("resolves");
        assert_eq!(model, "opus-5");
    }

    #[test]
    fn a_legacy_colon_form_actor_resolves_from_its_own_id() {
        // No registry row is consulted for the colon form; the id says.
        let model = resolve_model(&ok_run(), None).expect("resolves");
        assert_eq!(model, "opus-5");
    }

    #[test]
    fn a_run_that_names_no_model_by_any_rule_is_refused_naming_both_fixes() {
        let mut run = registered_run(None);
        run.actor_id = ActorId::RegisteredAgent("agent-nobody".into());
        let err = resolve_model(&run, None).unwrap_err().to_string();
        assert!(err.contains("agent-nobody"), "{err}");
        assert!(err.contains("send `model`"), "{err}");
        assert!(err.contains("register the agent"), "{err}");
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
    fn a_non_agent_actor_is_refused_and_names_both_agent_forms() {
        let mut run = ok_run();
        run.actor_id = ActorId::automation("train-conductor");
        let err = validate(&run).unwrap_err().to_string();
        assert!(err.contains("agent-claude"), "{err}");
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
