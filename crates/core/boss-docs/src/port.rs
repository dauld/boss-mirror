//! Hexagonal port: what the service needs from persistence.

use async_trait::async_trait;

use crate::types::{
    DesignDoc, DesignQuestion, FlushJob, FlushJobPayload, JobStatus, JobStatusUpdate,
    PendingDecision, PendingDecisionInput, RejectedDocRecord,
};

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

    /// Remove a doc (and its questions + pending decisions) from the
    /// cache. Used when a file disappears from disk.
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

    // ----- Pending decisions -----

    /// Upsert a pending decision. Keyed on `(doc_path, anchor)`, so
    /// clicking twice on the same question overwrites the first.
    async fn upsert_pending_decision(
        &self,
        input: &PendingDecisionInput,
        decided_by: &str,
    ) -> Result<PendingDecision, DocsError>;

    async fn delete_pending_decision(&self, doc_path: &str, anchor: &str) -> Result<(), DocsError>;

    async fn pending_decisions_for_doc(
        &self,
        doc_path: &str,
    ) -> Result<Vec<PendingDecision>, DocsError>;

    // ----- Flush jobs -----

    /// Atomically snapshot pending decisions into a new flush job and
    /// delete them. Returns the new job. Errors with
    /// `DocsError::BadRequest` if there are zero pending decisions
    /// for the doc (D8 from the design doc).
    async fn create_flush_job(
        &self,
        payload: &FlushJobPayload,
        requested_by: &str,
    ) -> Result<FlushJob, DocsError>;

    async fn flush_job_by_id(&self, id: &str) -> Result<Option<FlushJob>, DocsError>;

    async fn flush_jobs_by_status(&self, status: JobStatus) -> Result<Vec<FlushJob>, DocsError>;

    /// Move a job through its lifecycle. `worked_by` is the actor that
    /// made the call — the HTTP layer reads it off the request, never
    /// the body, because the body is the caller's to write and the
    /// identity header is the gateway's (backlog c3cd3301). `None`
    /// leaves the recorded worker alone; a requeue clears it, because
    /// a job waiting to run has no worker.
    async fn update_flush_job_status(
        &self,
        id: &str,
        update: &JobStatusUpdate,
        worked_by: Option<&str>,
    ) -> Result<FlushJob, DocsError>;

    /// Retry a failed job by setting status back to `queued` and
    /// calling the dispatch_worker hook (handled by the HTTP layer,
    /// not the repository).
    async fn retry_flush_job(&self, id: &str) -> Result<FlushJob, DocsError>;

    async fn recent_flush_jobs(&self, limit: i64) -> Result<Vec<FlushJob>, DocsError>;
}
