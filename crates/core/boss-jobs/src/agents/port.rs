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
use boss_core::publish::PublishMode;
use boss_core::publisher::EventStamp;

use super::types::{AgentInput, AgentRow, AgentsBatchOutcome, FieldChange};

/// The fact a tenant's declaration leaves: one per agent row the
/// batch INSERTED (backlog d9409039, 2026-09-17). Never per batch:
/// the rebuilders reproduce rows, not requests. A row the registry
/// already held records [`AGENT_UPDATED`] when the declaration moved
/// it, and nothing when it was already as declared.
pub const AGENT_DECLARED: &str = "agent.declared";

/// The fact a declaration leaves when it CHANGES a row the registry
/// already held (backlog 09887242, 2026-09-17): the row as it reads
/// after, the `changes` (field, from, to) the declaration made, and
/// `updated_by`. Until this kind the batch was insert-if-absent, so a
/// tenant that renamed its agent after the first publish read the
/// old name forever — the same defect as the roster's `emp-david`
/// keeping the first publish's location.
pub const AGENT_UPDATED: &str = "agent.updated";

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

/// Build the `agent.updated` event for one row a declaration changed:
/// the row as it now reads (its aliases included), the changes, and
/// `updated_by` from the stamp — the same one-value rule as
/// `declared_by`.
pub fn updated_event(
    stamp: &EventStamp,
    after: &AgentRow,
    changes: &[FieldChange],
) -> Result<Event, AgentsError> {
    let mut payload =
        serde_json::to_value(after).map_err(|e| AgentsError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "changes".to_string(),
            serde_json::to_value(changes).map_err(|e| AgentsError::Storage(e.to_string()))?,
        );
        map.insert(
            "updated_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
    }
    Ok(stamp.event(AGENT_UPDATED, payload))
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

    /// Land a tenant's declarations, one transaction: a row the
    /// registry does not hold is inserted; a row it holds is, under
    /// [`PublishMode::InsertIfAbsent`] (the default), KEPT with the
    /// declared fields it differs on named in the outcome — THE
    /// INSTANCE IS THE TRUTH (design e187198f, 2026-09-18: until then
    /// the declaration overwrote the row on every publish, and the
    /// publish runs at every boot, so an operator's edit lived until
    /// the next converge) — and under [`PublishMode::Take`] UPDATED on
    /// every declared field that differs, the outcome naming each
    /// change (a declaration is the whole row, since [`AgentInput`] is
    /// the table's columns). A declared alias nobody holds lands under
    /// the declared id in either mode (an alias is its own row); one
    /// another agent holds is kept there under the default and moved
    /// under take. An alias the tenant does not declare is never
    /// touched. Rows arrive already validated (`validate_agent`); an
    /// unpriced `default_model` is the one refusal the registry
    /// itself makes.
    ///
    /// Every row inserted records one [`AGENT_DECLARED`] event and
    /// every row changed one [`AGENT_UPDATED`] event, both built from
    /// `stamp` in that same transaction; a row kept or already as
    /// declared records nothing (backlog d9409039 — until then a
    /// tenant's agents left no audit-log fact).
    async fn publish(
        &self,
        rows: &[AgentInput],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError>;
}
