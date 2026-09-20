//! The dispatcher's firing record — read-only here (backlog b14afc48).
//!
//! `dispatcher_firings` (migration
//! `20260920190401-a-dispatcher-rule-records-its-firing.sql`) holds one
//! row per dispatcher rule firing, written by the rules runner that
//! fired it (`boss_dispatcher::rules::firings`). The jobs API only ever
//! READS it, for one question the IT world map asks of every border:
//! when did this machine last fire?
//!
//! ONE OPERATION, DELIBERATELY. The map needs the newest firing of a
//! NAMED rule, which is the `dispatcher_firings_rule_recency` index's
//! own read; a "list every firing" door would answer a question nobody
//! has with a page of rows the surface would then have to reduce. The
//! port grows when a second reader arrives (the reclaim and runner
//! work, a3397b01 / 57c108c2, are the candidates).
//!
//! WHAT THIS PORT CANNOT ANSWER, and must not pretend to: how often a
//! rule is EXPECTED to fire. A dispatcher rule fires on an event and
//! declares no heartbeat, so there is no interval to compare a silence
//! against — see [`crate::borders::DispatcherFiring`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error)]
pub enum DispatcherFiringsError {
    #[error("storage: {0}")]
    Storage(String),
}

/// The newest firing of one rule, as the row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFiring {
    pub firing_id: String,
    /// The topic the rule matched on.
    pub fired_on: String,
    pub fired_at: DateTime<Utc>,
}

#[async_trait]
pub trait DispatcherFiringsRepository: Send + Sync {
    /// The newest firing of `rule`, or `None` when the record holds no
    /// row for it. An error is NOT a `None`: the caller has to be able
    /// to tell "never fired" from "could not be read", because the
    /// second rendered as the first is how a dead machine reads
    /// healthy.
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, DispatcherFiringsError>;
}

/// In-memory adapter: the firings a test declares, keyed by rule name,
/// newest-wins.
pub struct InMemoryDispatcherFirings {
    firings: Vec<(String, LastFiring)>,
}

impl InMemoryDispatcherFirings {
    pub fn new(firings: Vec<(String, LastFiring)>) -> Self {
        Self { firings }
    }
}

#[async_trait]
impl DispatcherFiringsRepository for InMemoryDispatcherFirings {
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, DispatcherFiringsError> {
        Ok(self
            .firings
            .iter()
            .filter(|(name, _)| name == rule)
            .map(|(_, f)| f)
            .max_by_key(|f| f.fired_at)
            .cloned())
    }
}

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::{PgPool, Row};

    /// Postgres adapter. One statement, the recency index's own.
    pub struct PgDispatcherFirings {
        pool: PgPool,
    }

    impl PgDispatcherFirings {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    fn storage(e: sqlx::Error) -> DispatcherFiringsError {
        DispatcherFiringsError::Storage(e.to_string())
    }

    #[async_trait]
    impl DispatcherFiringsRepository for PgDispatcherFirings {
        async fn last_firing(
            &self,
            rule: &str,
        ) -> Result<Option<LastFiring>, DispatcherFiringsError> {
            let row = sqlx::query(
                "SELECT firing_id, fired_on, fired_at FROM dispatcher_firings \
                 WHERE rule_name = $1 ORDER BY fired_at DESC LIMIT 1",
            )
            .bind(rule)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
            let Some(r) = row else { return Ok(None) };
            Ok(Some(LastFiring {
                firing_id: r.try_get("firing_id").map_err(storage)?,
                fired_on: r.try_get("fired_on").map_err(storage)?,
                fired_at: r.try_get("fired_at").map_err(storage)?,
            }))
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgDispatcherFirings;
