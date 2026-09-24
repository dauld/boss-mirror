//! A dispatcher rule's firing record (backlog b14afc48).
//!
//! WHY. The IT world map draws every border with the machine that moves
//! it and when that machine last fired. Every machine could answer
//! except a dispatcher rule: `cadence_firings` measures the conductor's
//! schedule, a gate-run's own rows stamp the gate runner, and the
//! reactive layer — the most automated hop on the map, `marshalling ->
//! dock` moved by `auto-park-on-gate-green` — recorded nothing at all.
//! A hop nobody watches BECAUSE it is automated was the one hop that
//! could not prove it ran, so a stalled auto-park and an idle one
//! rendered identically.
//!
//! THE SHAPE IS `cadence_firings`, applied to the reactive side: one
//! row per firing, a deterministic id as the exactly-once guard, the
//! evidence in `detail`. The id is
//! `dispatcher:<rule>:<triggering event id>`, so a NAK-and-redeliver —
//! which re-runs the handlers — records the SAME firing rather than
//! inventing a second one. The rows are a measurement record, not an
//! audit event: nothing rebuilds from them and nothing else reads them
//! to decide anything.
//!
//! WHAT IS RECORDED IS WHAT RAN. A firing is written for each rule
//! whose handlers all SUCCEEDED on this delivery. A rule whose handler
//! failed did not move the packet, and a record saying it fired would
//! be the false-green the whole surface exists to refuse — the failure
//! has its own record (the dead-letter, `super::dead_letter`).
//!
//! WHAT IS NOT HERE: the clock-driven schedule runner
//! (`super::schedule_runner`). It fires on a sim-day boundary rather
//! than an event, no border on the map names it as its machine, and a
//! record nobody reads is the shape this car exists to remove — not
//! add. Its firings join the same table when a surface asks.
//!
//! BEST-EFFORT, ALWAYS. Recording happens after the settle is decided
//! and can never change it: a firing record that could not be written
//! must not turn a delivered side effect into a redelivery. Same reason
//! the dead-letter annotation returns `()`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// How long a firing row is kept — defined beside the table's reader,
/// where the rules list says "no firing in the last N days" by the same
/// number (backlog 43c4451a); this crate depends on that one, so the
/// one copy lives there and the prune reads it from here.
pub use boss_jobs::dispatcher_firings::RETENTION_DAYS;

/// How often the sink prunes, at most. The prune is one indexed DELETE
/// and the retention window is a month, so an hour of slack costs
/// nothing and keeps the hot path one INSERT.
pub const PRUNE_EVERY_MINUTES: i64 = 60;

/// One rule firing, as the row holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub firing_id: String,
    pub rule: String,
    /// The topic the rule matched on.
    pub fired_on: String,
    pub fired_at: DateTime<Utc>,
    pub detail: Value,
}

/// The exactly-once id of one rule's firing on one event. Deterministic
/// by construction: the same event redelivered computes the same id and
/// the primary key dedupes it, so the count answers "how many events
/// fired this rule", never "how many times the transport presented
/// them".
pub fn firing_id(rule: &str, event_id: &str) -> String {
    format!("dispatcher:{rule}:{event_id}")
}

/// Where a firing is recorded. A port, so the runner's tests assert
/// what would be written without a database, and so the record can move
/// behind a door later without touching the runner.
#[async_trait]
pub trait FiringSink: Send + Sync {
    /// Record these firings. Idempotent on `firing_id`: re-recording a
    /// firing already held is a no-op, never an error.
    async fn record(&self, firings: &[Firing]) -> Result<(), String>;
}

pub use pg::PgFirings;

mod pg {
    use super::*;
    use sqlx::PgPool;
    use tracing::warn;

    /// Postgres adapter — the dispatcher's own pool, the same one the
    /// log tail and the schedule cursor use.
    pub struct PgFirings {
        pool: PgPool,
        /// When this process last pruned. Adapter state, not domain
        /// state: it only bounds how often the DELETE runs, and a
        /// restart costing one extra prune is free.
        last_prune: tokio::sync::Mutex<Option<DateTime<Utc>>>,
    }

    impl PgFirings {
        pub fn new(pool: PgPool) -> Self {
            Self {
                pool,
                last_prune: tokio::sync::Mutex::new(None),
            }
        }

        /// Drop rows past [`RETENTION_DAYS`], at most every
        /// [`PRUNE_EVERY_MINUTES`]. Its failure is logged and dropped:
        /// a table that grew for another hour is not a reason to fail
        /// the recording of a firing that did happen.
        async fn maybe_prune(&self, now: DateTime<Utc>) {
            {
                let mut last = self.last_prune.lock().await;
                if let Some(t) = *last
                    && (now - t).num_minutes() < PRUNE_EVERY_MINUTES
                {
                    return;
                }
                *last = Some(now);
            }
            let cutoff = now - chrono::Duration::days(RETENTION_DAYS);
            if let Err(e) = sqlx::query("DELETE FROM dispatcher_firings WHERE fired_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
            {
                warn!(error = %e, "dispatcher firings: prune failed; rows past retention remain");
            }
        }
    }

    #[async_trait]
    impl FiringSink for PgFirings {
        async fn record(&self, firings: &[Firing]) -> Result<(), String> {
            let Some(first) = firings.first() else {
                return Ok(());
            };
            for f in firings {
                sqlx::query(
                    "INSERT INTO dispatcher_firings \
                     (firing_id, rule_name, fired_on, fired_at, detail) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (firing_id) DO NOTHING",
                )
                .bind(&f.firing_id)
                .bind(&f.rule)
                .bind(&f.fired_on)
                .bind(f.fired_at)
                .bind(&f.detail)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("recording firing {}: {e}", f.firing_id))?;
            }
            self.maybe_prune(first.fired_at).await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redelivered_event_computes_the_same_firing_id() {
        // The exactly-once guard: the transport may present an event
        // eight times, and the record must still say the rule fired
        // once on it.
        let a = firing_id("auto-park-on-gate-green", "evt-1");
        let b = firing_id("auto-park-on-gate-green", "evt-1");
        assert_eq!(a, b);
        assert_ne!(a, firing_id("auto-park-on-gate-green", "evt-2"));
        assert_ne!(a, firing_id("another-rule", "evt-1"));
        assert_eq!(a, "dispatcher:auto-park-on-gate-green:evt-1");
    }
}
