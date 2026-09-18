//! Credentials-registry port — the two reads every consumer needs,
//! plus the ONE write the rotation path owes the log.
//!
//! Reads stay deliberately minimal (`list` / `get`), the same
//! reasoning as `DeliveryPolicyRepository`: the registry's row
//! authors are migrations (new credentials, corrected facts), and an
//! endpoint that could mutate arbitrary row facts would be scope
//! nobody asked for.
//!
//! The write exists because the rotation path is a dispatcher
//! handler, and dispatcher handlers own no database — they speak
//! HTTP to the public surface like any caller (the census-door
//! precedent). `record_rotation` is that one write: record the
//! phase's `credential.*` event and, on the install phase, stamp
//! `rotated_at` — the two facts landing in ONE transaction so the
//! row and the log cannot disagree about whether a rotation was
//! recorded. Nothing else on the row is writable through this port.

use async_trait::async_trait;

use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::types::{
    CREDENTIAL_DECLARED, CredentialInput, CredentialRow, CredentialsBatchOutcome, RotationPhase,
};

/// Build the `credential.declared` event for one inserted row: the
/// declaration as inserted plus `declared_by` — the actor the request
/// signed with, read from the stamp so it is the same value `_actor`
/// carries — and `tenant_id`, the declaring instance. One builder for
/// both adapters, so the in-memory double records exactly what the Pg
/// adapter stages on the outbox. Locations and consumers only, never a
/// value: the input shape has no field one could ride in.
pub fn declared_event(
    stamp: &EventStamp,
    tenant_id: &str,
    row: &CredentialInput,
) -> Result<Event, CredentialsError> {
    let mut payload =
        serde_json::to_value(row).map_err(|e| CredentialsError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "declared_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
        map.insert(
            "tenant_id".to_string(),
            serde_json::Value::String(tenant_id.to_string()),
        );
    }
    Ok(stamp.event(CREDENTIAL_DECLARED, payload))
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("storage: {0}")]
    Storage(String),
    /// A rotation write named a credential the registry does not
    /// know. A loud, distinct variant rather than a silent no-op:
    /// an absent row is a finding (the read door's 404 posture),
    /// and recording a rotation event against nothing would detach
    /// the event from the registry row it exists to annotate.
    #[error("no credential {0:?} in the registry")]
    UnknownCredential(String),
}

#[async_trait]
pub trait CredentialsRegistry: Send + Sync {
    /// Every credential the registry knows, ordered by id so the
    /// rendered list is stable run to run.
    async fn list(&self) -> Result<Vec<CredentialRow>, CredentialsError>;

    /// One credential by its durable id, or `None` when the registry
    /// holds no such row. `None` is an answer, not an error — the
    /// HTTP door turns it into a 404 that names the id.
    async fn get(&self, id: &str) -> Result<Option<CredentialRow>, CredentialsError>;

    /// Admit an instance's declarations, insert-if-absent by id (the
    /// classes batch idiom; backlog ee368d0c): a row already there is
    /// KEPT as it is — its `rotated_at` and the notes the rotation
    /// path wrote are book-keeping the declaration must not erase —
    /// and a second publish inserts nothing new. One
    /// `credential.declared` fact per row inserted, staged in the
    /// insert's own transaction, none for a kept row. Validation is
    /// the caller's (`validate_credential`), the same check at every
    /// door.
    async fn publish(
        &self,
        tenant_id: &str,
        declared: &[CredentialInput],
        stamp: &EventStamp,
    ) -> Result<CredentialsBatchOutcome, CredentialsError>;

    /// Record one rotation phase: append the phase's `credential.*`
    /// event (kind from [`RotationPhase::event_kind`], envelope from
    /// `stamp`) and — for [`RotationPhase::Installed`], the moment
    /// the value last changed — stamp the row's `rotated_at` with
    /// the SAME instant the event carries (`stamp.timestamp`), so
    /// live row and replayed row agree.
    ///
    /// `evidence` is the caller-owned payload: identifiers and
    /// observed effects only, NEVER a value — the door's module doc
    /// owns that rule; this port trusts its instrument the way the
    /// census door trusts its handler.
    async fn record_rotation(
        &self,
        id: &str,
        phase: RotationPhase,
        evidence: serde_json::Value,
        stamp: &EventStamp,
    ) -> Result<(), CredentialsError>;
}
