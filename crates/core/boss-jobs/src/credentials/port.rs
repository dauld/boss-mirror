//! Credentials-registry port — the two reads every consumer needs,
//! and nothing else.
//!
//! No writes here, deliberately. The registry's writers are
//! migrations (new credentials, corrected facts) and the rotation
//! path (`rotated_at`); neither goes through HTTP, so an endpoint
//! that could mutate a row would be scope nobody asked for — the
//! same reasoning as `DeliveryPolicyRepository`.

use async_trait::async_trait;

use super::types::CredentialRow;

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("storage: {0}")]
    Storage(String),
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
}
