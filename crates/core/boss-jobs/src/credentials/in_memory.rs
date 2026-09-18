//! In-memory adapter for `CredentialsRegistry` — the port-level test
//! double. Mirrors the Pg semantics that matter: `list` is ordered by
//! id, an unknown rotation target is `UnknownCredential`, and the
//! install phase stamps `rotated_at` with the event's own instant.

use async_trait::async_trait;
use std::sync::Mutex;

use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::port::{CredentialsError, CredentialsRegistry, declared_event};
use super::types::{CredentialInput, CredentialRow, CredentialsBatchOutcome, RotationPhase};

/// The registry row a declaration lands as: JSON arrays for the two
/// list columns (the Pg adapter's `scopes`/`consumers` are JSONB),
/// `rotated_at` NULL — a declaration never claims a rotation instant.
fn row_of(c: &CredentialInput) -> Result<CredentialRow, CredentialsError> {
    Ok(CredentialRow {
        id: c.id.clone(),
        kind: c.kind.clone(),
        issuer: c.issuer.clone(),
        principal: c.principal.clone(),
        scopes: serde_json::to_value(&c.scopes)
            .map_err(|e| CredentialsError::Storage(e.to_string()))?,
        storage_location: c.storage_location.clone(),
        consumers: serde_json::to_value(&c.consumers)
            .map_err(|e| CredentialsError::Storage(e.to_string()))?,
        rotation_policy: c.rotation_policy.clone(),
        rotated_at: None,
        notes: c.notes.clone(),
    })
}

#[derive(Default)]
pub struct InMemoryCredentials {
    rows: Mutex<Vec<CredentialRow>>,
    events: Mutex<Vec<Event>>,
}

impl InMemoryCredentials {
    pub fn new(rows: Vec<CredentialRow>) -> Self {
        Self {
            rows: Mutex::new(rows),
            events: Mutex::new(Vec::new()),
        }
    }

    /// Every rotation event recorded through this adapter, in order —
    /// what a Pg deployment would find on the outbox.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.events.lock().expect("events lock").clone()
    }
}

#[async_trait]
impl CredentialsRegistry for InMemoryCredentials {
    async fn list(&self) -> Result<Vec<CredentialRow>, CredentialsError> {
        let mut rows = self.rows.lock().expect("rows lock").clone();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialRow>, CredentialsError> {
        Ok(self
            .rows
            .lock()
            .expect("rows lock")
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }

    async fn publish(
        &self,
        tenant_id: &str,
        declared: &[CredentialInput],
        stamp: &EventStamp,
    ) -> Result<CredentialsBatchOutcome, CredentialsError> {
        let mut rows = self.rows.lock().expect("rows lock");
        let mut events = self.events.lock().expect("events lock");
        let mut inserted = 0;
        for c in declared {
            if rows.iter().any(|r| r.id == c.id) {
                continue;
            }
            rows.push(row_of(c)?);
            events.push(declared_event(stamp, tenant_id, c)?);
            inserted += 1;
        }
        Ok(CredentialsBatchOutcome {
            received: declared.len(),
            inserted,
        })
    }

    async fn record_rotation(
        &self,
        id: &str,
        phase: RotationPhase,
        evidence: serde_json::Value,
        stamp: &EventStamp,
    ) -> Result<(), CredentialsError> {
        let mut rows = self.rows.lock().expect("rows lock");
        let Some(row) = rows.iter_mut().find(|r| r.id == id) else {
            return Err(CredentialsError::UnknownCredential(id.to_string()));
        };
        let event = stamp.event(phase.event_kind(), evidence);
        if phase == RotationPhase::Installed {
            row.rotated_at = Some(stamp.timestamp);
        }
        self.events.lock().expect("events lock").push(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn row(id: &str) -> CredentialRow {
        CredentialRow {
            id: id.into(),
            kind: "forgejo-access-token".into(),
            issuer: "forgejo (10.20.0.15)".into(),
            principal: "user david".into(),
            scopes: json!(["write:repository"]),
            storage_location: "k8s Secret boss-dev/boss-dev-forge-token key token".into(),
            consumers: json!([{ "kind": "secret-mount", "location": "/etc/boss-train/forge.token" }]),
            rotation_policy: "on-demand".into(),
            rotated_at: None,
            notes: String::new(),
        }
    }

    fn stamp() -> EventStamp {
        EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("rule:broker-test".into()),
        )
    }

    #[tokio::test]
    async fn list_is_ordered_by_id_whatever_the_insert_order() {
        let repo = InMemoryCredentials::new(vec![row("zeta"), row("alpha")]);
        let ids: Vec<String> = repo
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["alpha", "zeta"]);
    }

    #[tokio::test]
    async fn get_answers_the_row_or_none() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let got = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
        assert_eq!(got.kind, "forgejo-access-token");
        assert!(
            repo.get("no-such-credential").await.unwrap().is_none(),
            "an unknown id is None, not an error — the HTTP door owns the 404"
        );
    }

    #[tokio::test]
    async fn an_empty_registry_lists_empty() {
        let repo = InMemoryCredentials::default();
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_recorded_phase_becomes_an_event_of_its_kind() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Minted,
            json!({ "token_name": "boss-dev-forge-token-7ee101aa" }),
            &stamp(),
        )
        .await
        .unwrap();
        let events = repo.recorded_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "credential.minted");
        assert_eq!(events[0].source, "jobs");
        assert_eq!(
            events[0].payload["token_name"],
            "boss-dev-forge-token-7ee101aa"
        );
        assert_eq!(
            events[0].payload["_actor"], "automation:rule:broker-test",
            "the stamp's actor rides the payload exactly as EventStamp injects it"
        );
    }

    #[tokio::test]
    async fn only_the_install_phase_stamps_rotated_at() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let s = stamp();
        repo.record_rotation("boss-dev-forge-token", RotationPhase::Minted, json!({}), &s)
            .await
            .unwrap();
        assert!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_none(),
            "a mint changes nothing installed — rotated_at waits for the install"
        );
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            json!({}),
            &s,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at,
            Some(s.timestamp),
            "the row bind and the event share ONE instant (stamp.timestamp)"
        );
    }

    fn declaration(id: &str) -> CredentialInput {
        CredentialInput {
            id: id.into(),
            kind: "stripe-restricted-key".into(),
            issuer: "stripe (the operator's dashboard)".into(),
            principal: "the company's Stripe account".into(),
            scopes: vec!["charges: read".into()],
            storage_location:
                "k8s Secret boss/boss-credential-broker-root key stripe-restricted-read".into(),
            consumers: vec![super::super::types::Consumer {
                kind: "env".into(),
                location: "dispatcher env BOSS_BROKER_STRIPE_KEY".into(),
            }],
            rotation_policy: "on-demand".into(),
            notes: "declared by the tenant".into(),
        }
    }

    /// The declaration door (backlog ee368d0c): insert-if-absent by id,
    /// one `credential.declared` per row INSERTED carrying `declared_by`
    /// and the tenant, none for a kept row — and the kept row keeps
    /// what the rotation path wrote (`rotated_at`), because a
    /// declaration is knowledge about the credential, not its history.
    #[tokio::test]
    async fn a_declaration_inserts_absent_rows_keeps_held_ones_and_leaves_one_fact_per_insert() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let s = stamp();
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            json!({}),
            &s,
        )
        .await
        .unwrap();
        let mut held = declaration("boss-dev-forge-token");
        held.notes = "a declaration must not erase the rotation's book-keeping".into();
        let out = repo
            .publish("acme", &[held, declaration("stripe-restricted-read")], &s)
            .await
            .unwrap();
        assert_eq!((out.received, out.inserted), (2, 1));
        let kept = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
        assert_eq!(
            kept.rotated_at,
            Some(s.timestamp),
            "the held row is untouched"
        );
        assert_eq!(kept.notes, "", "the held row's notes are untouched");
        let new = repo.get("stripe-restricted-read").await.unwrap().unwrap();
        assert_eq!(new.scopes, json!(["charges: read"]));
        assert!(
            new.rotated_at.is_none(),
            "a declaration claims no rotation instant"
        );
        let declared: Vec<_> = repo
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == "credential.declared")
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "one fact per inserted row, none for the kept one"
        );
        assert_eq!(declared[0].payload["id"], "stripe-restricted-read");
        assert_eq!(declared[0].payload["tenant_id"], "acme");
        assert_eq!(
            declared[0].payload["declared_by"], declared[0].payload["_actor"],
            "declared_by and the stamp's actor are one value"
        );
        // A second publish inserts nothing and records nothing.
        let again = repo
            .publish("acme", &[declaration("stripe-restricted-read")], &s)
            .await
            .unwrap();
        assert_eq!((again.received, again.inserted), (1, 0));
        assert_eq!(
            repo.recorded_events()
                .iter()
                .filter(|e| e.kind == "credential.declared")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_rotation_against_an_unknown_credential_is_refused_loudly() {
        let repo = InMemoryCredentials::new(vec![]);
        let err = repo
            .record_rotation("ghost", RotationPhase::Minted, json!({}), &stamp())
            .await
            .unwrap_err();
        assert!(matches!(err, CredentialsError::UnknownCredential(id) if id == "ghost"));
        assert!(
            repo.recorded_events().is_empty(),
            "no event may detach from the row it annotates"
        );
    }
}
