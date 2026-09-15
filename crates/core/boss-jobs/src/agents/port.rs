//! Agents-registry port — the one read the door needs.
//!
//! Deliberately a single question. The registry's rows arrive by
//! migration (see the module doc), and a `list` / `get` with no reader
//! would be scope nobody asked for; when a surface wants the roster,
//! the read is added beside the reader that needs it.

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum AgentsError {
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait AgentsRegistry: Send + Sync {
    /// The registered actor id this login signs as, or `None` when no
    /// alias maps it. `None` is an answer — the door treats it as an
    /// unresolved login, which during the migration window is admitted
    /// and counted — and `Err` is the registry failing to answer at
    /// all, which is a different fact and is logged as one.
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError>;
}
