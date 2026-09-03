//! In-memory adapter for `CredentialsRegistry` — the port-level test
//! double. Mirrors the one Pg semantic that matters: `list` is
//! ordered by id, so the rendered registry is stable run to run.

use async_trait::async_trait;

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::CredentialRow;

#[derive(Default)]
pub struct InMemoryCredentials {
    rows: Vec<CredentialRow>,
}

impl InMemoryCredentials {
    pub fn new(rows: Vec<CredentialRow>) -> Self {
        Self { rows }
    }
}

#[async_trait]
impl CredentialsRegistry for InMemoryCredentials {
    async fn list(&self) -> Result<Vec<CredentialRow>, CredentialsError> {
        let mut rows = self.rows.clone();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialRow>, CredentialsError> {
        Ok(self.rows.iter().find(|r| r.id == id).cloned())
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
}
