//! The log tail — rules consumption straight from `audit_log`
//! (transactional-audit-log Q6, stage 1; backlog `3d6d6bea`).
//!
//! With the outbox → single-relay pipeline, `audit_log` id order ≡
//! commit order, so a consumer that walks ids misses nothing — the
//! Q2/Q6 coupling the design doc records. This module is the
//! walking: `SELECT ... WHERE id > cursor ORDER BY id`, hand each
//! row to the transport-agnostic rules `handle`, advance the cursor
//! per settled row. The durable state is one row in
//! `dispatcher_log_cursor`; there is no ack ledger and no dedup
//! table because a crash re-runs at most the unsettled tail and
//! every handler is already idempotent (the at-least-once contract
//! this crate has held since the JetStream era).
//!
//! ## Retry without NAK
//!
//! Ordered delivery has no redelivery queue: a failing row BLOCKS the
//! cursor and is re-presented on the next drain with an incremented
//! in-memory attempt count, paced by the same widening schedule the
//! JetStream consumers used. At [`MAX_ATTEMPTS`] the row is
//! dead-lettered — loudly, with the `DEAD-LETTER:` line the release
//! gates grep for — and the cursor advances past it: a poison event
//! delays the tail by its budget (~98s), never wedges it. Attempts
//! are in-memory by design: a process restart re-presents the row
//! with a fresh budget, which errs toward retrying — the safe side
//! for side effects that are idempotent.
//!
//! ## Sequencing note
//!
//! v1 is sequential where the JetStream loop fanned out 12-wide. The
//! measured rules volume is ~1 event/sec steady with bursts the
//! relay itself paces; if drain depth ever says otherwise, the
//! fan-out returns with a settled-prefix cursor advance. Named here
//! so the simplification is a decision, not an accident.

use std::future::Future;

use anyhow::{Context as _, Result};
use serde_json::Value;
use sqlx::PgPool;
use tracing::{error, warn};

use boss_nats::durable::Settle;

/// Retry budget per row: the original presentation plus seven retries.
///
/// DERIVED, not copied, from the JetStream consumers' budget. The two
/// transports must agree because the runner predicts the dead-letter from
/// the attempt count to annotate the packet (`a9c498eb`,
/// [`crate::rules::dead_letter`]) — a 8-vs-9 drift between these two
/// would annotate one delivery early on one path, or not at all on the
/// other. One definition cannot drift from itself (CLAUDE.md §9a).
pub const MAX_ATTEMPTS: u32 = boss_nats::durable::MAX_DELIVER as u32;

/// One row the tail is currently blocked on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub audit_id: i64,
    /// Presentations so far (1-based: the first failure is attempt 1).
    pub attempts: u32,
    pub reason: String,
}

/// What one [`LogTail::drain_once`] accomplished.
#[derive(Debug, Default)]
pub struct DrainReport {
    /// Rows settled (Ack or Permanent) and advanced past.
    pub processed: u64,
    /// Rows dead-lettered after exhausting [`MAX_ATTEMPTS`].
    pub dead_lettered: u64,
    /// The row the tail is blocked on, if the drain stopped early.
    pub blocked: Option<Blocked>,
}

/// The widening retry schedule, mirrored from the JetStream
/// consumers' NAK pacing (1s → 30s tail).
pub fn retry_delay(attempts: u32) -> std::time::Duration {
    const SCHEDULE: [u64; 7] = [1, 2, 5, 10, 20, 30, 30];
    let idx = (attempts.max(1) as usize - 1).min(SCHEDULE.len() - 1);
    std::time::Duration::from_secs(SCHEDULE[idx])
}

pub struct LogTail {
    pool: PgPool,
    consumer: String,
    /// In-flight retry state for the row the cursor is blocked on.
    blocked: Option<Blocked>,
}

impl LogTail {
    pub fn new(pool: PgPool, consumer: &str) -> Self {
        Self {
            pool,
            consumer: consumer.to_string(),
            blocked: None,
        }
    }

    /// First-run initialization: start at MAX(audit_log.id) — the
    /// position a freshly-bound durable consumer would hold. Existing
    /// cursors are left exactly where they are.
    pub async fn ensure_cursor(&mut self) -> Result<()> {
        sqlx::query(
            "INSERT INTO dispatcher_log_cursor (consumer, last_audit_id) \
             SELECT $1, COALESCE((SELECT MAX(id) FROM audit_log), 0) \
             ON CONFLICT (consumer) DO NOTHING",
        )
        .bind(&self.consumer)
        .execute(&self.pool)
        .await
        .context("initializing log cursor")?;
        Ok(())
    }

    async fn cursor(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT last_audit_id FROM dispatcher_log_cursor WHERE consumer = $1")
            .bind(&self.consumer)
            .fetch_one(&self.pool)
            .await
            .context("reading log cursor")
    }

    async fn advance(&self, to: i64) -> Result<()> {
        sqlx::query(
            "UPDATE dispatcher_log_cursor SET last_audit_id = $2, updated_at = NOW() \
             WHERE consumer = $1",
        )
        .bind(&self.consumer)
        .bind(to)
        .execute(&self.pool)
        .await
        .context("advancing log cursor")?;
        Ok(())
    }

    /// Drain one batch. Settled rows advance the cursor per item; a
    /// `Retry` outcome stops the drain with the row recorded in
    /// `blocked` (re-presented next call). The caller owns pacing —
    /// this function never sleeps, so tests drive the whole retry
    /// ladder without waiting on the schedule.
    ///
    /// `handle` receives `(topic, event_id, payload, attempt)`. The
    /// attempt is this presentation's 1-based count, passed IN rather
    /// than kept to this struct, because the handler has to know a
    /// failure now is the last one the budget allows — that is the only
    /// moment it still holds the rule, the handler, the count and the
    /// error together (`a9c498eb`).
    pub async fn drain_once<F, Fut>(&mut self, batch: i64, handle: F) -> Result<DrainReport>
    where
        F: Fn(String, String, Value, u32) -> Fut,
        Fut: Future<Output = Settle>,
    {
        let mut report = DrainReport::default();
        let from = self.cursor().await?;

        let rows: Vec<(i64, String, String, Value)> = sqlx::query_as(
            "SELECT id, event_id::text, kind, payload FROM audit_log \
             WHERE id > $1 ORDER BY id LIMIT $2",
        )
        .bind(from)
        .bind(batch)
        .fetch_all(&self.pool)
        .await
        .context("reading audit tail")?;

        for (id, event_id, kind, payload) in rows {
            // Inherit the event's sim-ness so side effects on
            // simulated facts write simulated state — the same
            // task-local discipline as the JetStream loop.
            let simulated = payload
                .get("_simulated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // This presentation's attempt count, computed BEFORE the
            // handler runs because the handler needs it (see the doc
            // above). The Retry arm below uses the same value.
            let attempts = match &self.blocked {
                Some(b) if b.audit_id == id => b.attempts + 1,
                _ => 1,
            };
            let outcome = boss_core::sim_origin::with_sim_chain(
                simulated,
                handle(kind.clone(), event_id, payload, attempts),
            )
            .await;

            match outcome {
                Settle::Ack => {
                    self.advance(id).await?;
                    self.blocked = None;
                    report.processed += 1;
                }
                Settle::Permanent(reason) => {
                    // Deterministic data error: retrying cannot help.
                    warn!(audit_id = id, %kind, %reason, "log tail: permanent, advancing");
                    self.advance(id).await?;
                    self.blocked = None;
                    report.processed += 1;
                }
                Settle::Retry(reason) => {
                    if attempts >= MAX_ATTEMPTS {
                        // The exact prefix the release gates grep for.
                        error!(
                            audit_id = id,
                            %kind,
                            attempts,
                            %reason,
                            "DEAD-LETTER: log tail exhausted the retry budget; advancing past the event"
                        );
                        self.advance(id).await?;
                        self.blocked = None;
                        report.dead_lettered += 1;
                        continue;
                    }
                    let blocked = Blocked {
                        audit_id: id,
                        attempts,
                        reason,
                    };
                    self.blocked = Some(blocked.clone());
                    report.blocked = Some(blocked);
                    return Ok(report);
                }
            }
        }
        Ok(report)
    }
}
