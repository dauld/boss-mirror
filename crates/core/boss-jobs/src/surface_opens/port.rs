//! The surface-opens port — three operations and nothing else.
//!
//! No outbox events here, and for the same reason `cadence` gives: the
//! row is its own measurement record. A parallel event stream would
//! also put a per-request fact on the audit log, which §Policy & auth
//! decided against.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::types::{RETENTION_DAYS, Rollup, Sweep};

#[derive(Debug, thiserror::Error)]
pub enum SurfaceOpensError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait SurfaceOpens: Send + Sync {
    /// Record one open: this actor opened this route pattern at this
    /// instant. The door has already validated the pattern and refused
    /// a machine-shaped actor; the adapter writes.
    async fn record(
        &self,
        actor_id: &str,
        route: &str,
        at: DateTime<Utc>,
    ) -> Result<(), SurfaceOpensError>;

    /// Every (actor, route) pair opened in `[since, until)`, with its
    /// count and last open — actor-ordered, then most-opened first.
    /// `since >= until` is a bad request, not an empty answer: a
    /// window that cannot hold a row is a caller's mistake and an empty
    /// list would read as a quiet day.
    async fn rollup(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Rollup, SurfaceOpensError>;

    /// Delete every row opened before `before`. Returns how many.
    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SurfaceOpensError>;
}

/// The one window check both adapters run, so an in-memory caller and
/// a Postgres caller are refused for the same reason.
pub fn validate_window(
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<(), SurfaceOpensError> {
    if since >= until {
        return Err(SurfaceOpensError::BadRequest(format!(
            "since ({since}) is not before until ({until}) — the window holds nothing"
        )));
    }
    Ok(())
}

/// The sweep as the door reports it: the cutoff is `now` less the
/// retention, and the adapter's count rides beside it.
pub async fn sweep_retention(
    repo: &dyn SurfaceOpens,
    now: DateTime<Utc>,
) -> Result<Sweep, SurfaceOpensError> {
    let before = now - chrono::Duration::days(RETENTION_DAYS);
    let deleted = repo.sweep(before).await?;
    Ok(Sweep {
        deleted,
        before,
        retention_days: RETENTION_DAYS,
    })
}
