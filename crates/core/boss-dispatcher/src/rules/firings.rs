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
//! THE SCHEDULE RUNNER WRITES HERE TOO (backlog 4b175523). It was left
//! out "until a surface asks", and the rules list asked: every
//! scheduled rule — 24 of 76 live on 2026-09-26 — could only read "not
//! recorded", so a stalled cadence and an idle one looked the same. A
//! day rule's id is `dispatcher:<rule>:clock-day:<day>`, so a crash
//! between firing a sim-day and persisting the cursor re-fires the day
//! and records it once; a sub-day rule's is keyed by its tick instant.
//!
//! AND WHAT DID NOT DELIVER, WHEN NO PACKET CAN SAY SO. A dead-letter on
//! a topic whose subject is not a packet (`super::dead_letter::
//! annotation_target` answers None) has no packet to be annotated on;
//! it is recorded here as an [`Outcome::DeadLetter`] row under
//! [`dead_letter_id`], so the rules list's rollup can count it and the
//! liveness counter can call it recorded (4b175523).
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

/// What a row says the rule did on its event — the `outcome` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every handler of the rule succeeded.
    Fired,
    /// The handlers failed past the budget (or permanently) on a topic
    /// that names no packet to annotate.
    DeadLetter,
}

impl Outcome {
    /// The column's spelling, which the migration's CHECK and the
    /// jobs API's readers (`boss_jobs::dispatcher_firings`) share.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Fired => "fired",
            Outcome::DeadLetter => "dead-letter",
        }
    }
}

/// One rule firing, as the row holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub firing_id: String,
    pub rule: String,
    /// The topic the rule matched on.
    pub fired_on: String,
    pub fired_at: DateTime<Utc>,
    pub detail: Value,
    pub outcome: Outcome,
}

/// The exactly-once id of one rule's firing on one event. Deterministic
/// by construction: the same event redelivered computes the same id and
/// the primary key dedupes it, so the count answers "how many events
/// fired this rule", never "how many times the transport presented
/// them".
pub fn firing_id(rule: &str, event_id: &str) -> String {
    format!("dispatcher:{rule}:{event_id}")
}

/// The exactly-once id of one rule's dead-letter on one event. A prefix
/// of its own, so a log-tail restart that later DELIVERS the same event
/// (a fresh budget) records its firing instead of the primary key
/// swallowing it as the dead-letter already held.
pub fn dead_letter_id(rule: &str, event_id: &str) -> String {
    format!("dead-letter:{rule}:{event_id}")
}

/// The rules whose handlers ALL succeeded, in fire order, each once: a
/// rule with three handlers fired once on its event, and a rule with one
/// failed handler did not fire at all — its record is the dead-letter.
/// The one reading of "fired" both runners record by.
pub fn fired_rules(results: &[super::handler::InvocationResult]) -> Vec<String> {
    let failed: std::collections::HashSet<&str> = results
        .iter()
        .filter(|r| r.outcome.is_err())
        .map(|r| r.rule_name.as_str())
        .collect();
    let mut fired: Vec<String> = Vec::new();
    for r in results {
        if !failed.contains(r.rule_name.as_str()) && !fired.contains(&r.rule_name) {
            fired.push(r.rule_name.clone());
        }
    }
    fired
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
                     (firing_id, rule_name, fired_on, fired_at, detail, outcome) \
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (firing_id) DO NOTHING",
                )
                .bind(&f.firing_id)
                .bind(&f.rule)
                .bind(&f.fired_on)
                .bind(f.fired_at)
                .bind(&f.detail)
                .bind(f.outcome.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| format!("recording firing {}: {e}", f.firing_id))?;
            }
            self.maybe_prune(first.fired_at).await;
            Ok(())
        }
    }
}

/// In-memory sinks for the runners' tests: one that keeps what it was
/// asked to record, one that fails the way a down database would.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Arc;

    pub(crate) struct RecordingFirings {
        pub(crate) recorded: tokio::sync::Mutex<Vec<Firing>>,
    }

    impl RecordingFirings {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                recorded: tokio::sync::Mutex::new(Vec::new()),
            })
        }
        pub(crate) async fn rules(&self) -> Vec<String> {
            self.recorded
                .lock()
                .await
                .iter()
                .map(|f| f.rule.clone())
                .collect()
        }
    }

    #[async_trait]
    impl FiringSink for RecordingFirings {
        async fn record(&self, firings: &[Firing]) -> Result<(), String> {
            self.recorded.lock().await.extend_from_slice(firings);
            Ok(())
        }
    }

    pub(crate) struct FailingFirings;

    #[async_trait]
    impl FiringSink for FailingFirings {
        async fn record(&self, _firings: &[Firing]) -> Result<(), String> {
            Err("connection refused".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::handler::{HandlerError, InvocationResult};
    use super::*;

    fn result(rule: &str, ok: bool) -> InvocationResult {
        InvocationResult {
            rule_name: rule.to_string(),
            handler: "h".to_string(),
            outcome: if ok {
                Ok(())
            } else {
                Err(HandlerError::Permanent("bad arg".into()))
            },
        }
    }

    #[test]
    fn a_rule_fired_only_when_every_one_of_its_handlers_succeeded() {
        let fired = fired_rules(&[
            result("a", true),
            result("a", true),
            result("b", true),
            result("b", false),
            result("c", true),
        ]);
        assert_eq!(
            fired,
            ["a", "c"],
            "each once, in order; b failed one handler"
        );
        assert!(fired_rules(&[]).is_empty());
    }

    #[test]
    fn a_dead_letter_and_a_firing_on_one_event_never_share_an_id() {
        // A log-tail restart re-presents a dead-lettered event with a
        // fresh budget; if it delivers, its firing must land beside the
        // dead-letter, not be swallowed by the primary key.
        assert_ne!(dead_letter_id("r", "evt-1"), firing_id("r", "evt-1"));
        assert_eq!(dead_letter_id("r", "evt-1"), "dead-letter:r:evt-1");
        assert_eq!(Outcome::Fired.as_str(), "fired");
        assert_eq!(Outcome::DeadLetter.as_str(), "dead-letter");
    }

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
