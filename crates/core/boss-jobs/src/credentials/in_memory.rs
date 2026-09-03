//! In-memory adapter for `CredentialsRegistry` — the port-level test
//! double. Mirrors the Pg semantics that matter: `list` is ordered by
//! id, an unknown rotation target is `UnknownCredential`, and the
//! install phase stamps `rotated_at` with the event's own instant.

use async_trait::async_trait;
use std::sync::Mutex;

use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::{CredentialRow, RotationPhase};

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
