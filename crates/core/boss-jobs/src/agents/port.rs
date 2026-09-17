//! Agents-registry port — the read the door needs, the roster a
//! surface reads, and the one write a tenant's declaration lands by.
//!
//! The read stayed a single question until 2026-09-17 (the registry's
//! rows arrived by migration, and a `list` with no reader would have
//! been scope nobody asked for). Backlog f56155f0 added the reader — a
//! tenant declares its agents in `seeds/agents.toml`, the batch lands
//! them, and `GET /api/agents` is how the declaration is read back and
//! proved — so `list` and `publish` were added beside it.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::types::{AgentInput, AgentRow, AgentsBatchOutcome};

/// The fact a tenant's declaration leaves: one per agent row the
/// batch INSERTED (backlog d9409039, 2026-09-17). Never per kept row
/// — a row the registry already held changed nothing, and the kept
/// row is already named in the batch's answer — and never per batch:
/// the rebuilders reproduce rows, not requests.
pub const AGENT_DECLARED: &str = "agent.declared";

/// Build the `agent.declared` event for one inserted row: the
/// declaration as inserted (id, name, model, caps, and the aliases
/// landed with it), plus `declared_by` — the actor the request signed
/// with, read from the stamp so it is the same value `_actor`
/// carries. One builder for both adapters, so the in-memory double
/// records exactly what the Pg adapter stages on the outbox.
pub fn declared_event(stamp: &EventStamp, row: &AgentInput) -> Result<Event, AgentsError> {
    let mut payload = serde_json::to_value(row).map_err(|e| AgentsError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "declared_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
    }
    Ok(stamp.event(AGENT_DECLARED, payload))
}

#[derive(Debug, thiserror::Error)]
pub enum AgentsError {
    #[error("storage: {0}")]
    Storage(String),
    /// A `default_model` the rate card does not price. The `agents`
    /// table's FK refuses it; named here so the door can answer 422
    /// with the model rather than a storage error.
    #[error("default_model `{0}` is not a rate-card model")]
    Unpriced(String),
}

#[async_trait]
pub trait AgentsRegistry: Send + Sync {
    /// The registered actor id this login signs as, or `None` when no
    /// alias maps it. `None` is an answer — the door treats it as an
    /// unresolved login, which during the migration window is admitted
    /// and counted — and `Err` is the registry failing to answer at
    /// all, which is a different fact and is logged as one.
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError>;

    /// Every registered agent with its aliases, ordered by id.
    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError>;

    /// Land a tenant's declarations: insert-if-absent by id (a row the
    /// platform already registered is KEPT, never overwritten, and the
    /// outcome names which declared fields differ), aliases
    /// insert-if-absent by alias, one transaction. Rows arrive already
    /// validated (`validate_agent`); an unpriced `default_model` is
    /// the one refusal the registry itself makes.
    ///
    /// Every row inserted records one [`AGENT_DECLARED`] event built
    /// from `stamp` ([`declared_event`]) in that same transaction; a
    /// kept row records nothing (backlog d9409039 — until then a
    /// tenant's agents left no audit-log fact).
    async fn publish(
        &self,
        rows: &[AgentInput],
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError>;
}
