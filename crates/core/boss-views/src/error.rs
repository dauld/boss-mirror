//! Error type for the Views port.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ViewsError {
    #[error("view not found: {0}")]
    NotFound(String),
    /// The `filter` expression did not parse. Reported at save time,
    /// not at read time — a View that only fails when someone opens it
    /// is a trap laid for whoever opens it.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
    #[error("invalid view: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
    /// The caller's scope could not be decided. Kept as the policy
    /// client's own error so the door renders it the one way every door
    /// does — an outage is a 503 with Retry-After — rather than as a
    /// storage 500 carrying its text (backlog fe9d212c).
    #[error(transparent)]
    Policy(#[from] boss_policy_client::PolicyClientError),
}
