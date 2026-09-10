//! Hexagonal port: what the service needs from persistence.

use async_trait::async_trait;

use crate::types::{DesignDoc, DesignQuestion, RejectedDocRecord};

#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

#[async_trait]
pub trait DocsRepository: Send + Sync {
    // ----- Docs and questions (read-cache) -----

    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError>;

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError>;

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError>;

    /// Replace a doc's metadata + questions atomically. Used by the
    /// reindexer after re-parsing a file. Deletes any existing
    /// questions for the doc and inserts the new set in one txn.
    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError>;

    /// Remove a doc (and its questions) from the cache. Used when a
    /// file disappears from disk.
    async fn delete_doc(&self, path: &str) -> Result<(), DocsError>;

    // ----- Rejections -----

    /// Docs currently failing to index. Non-empty means "these are not
    /// in the tracker right now", not "these once failed".
    async fn all_rejections(&self) -> Result<Vec<RejectedDocRecord>, DocsError>;

    /// Record a rejection, preserving `first_seen_at` if the doc was
    /// already failing.
    async fn upsert_rejection(&self, path: &str, reason: &str) -> Result<(), DocsError>;

    /// Clear a rejection — the doc indexed cleanly.
    async fn clear_rejection(&self, path: &str) -> Result<(), DocsError>;
}
