//! JetStream durability layer for the event bus.
//!
//! Plain core NATS is at-most-once: a message is delivered once and, if
//! the subscriber's handler fails, the work is simply lost. For the
//! dispatcher's consumers that meant a transient hiccup — a briefly
//! unreachable policy-api, a saturated Postgres — silently *orphaned* the
//! Job whose step-completion event got dropped.
//!
//! This module adds a durable delivery buffer on top of the same NATS
//! server. A JetStream stream ([`STREAM_NAME`]) captures the job/step
//! lifecycle subjects; the dispatcher binds **durable pull consumers**
//! ([`open_durable`]) that explicitly ACK on success and NAK on failure.
//! A NAK redelivers the message on a backoff schedule, so a transient
//! failure self-heals once the underlying condition clears, and only a
//! genuinely-unprocessable event (exhausting [`MAX_DELIVER`]) is dropped —
//! loudly, as a dead-letter.
//!
//! This is **not** a second system of record. The Postgres `audit_log`
//! remains authoritative; the stream is a bounded, expiring buffer whose
//! sole job is reliable in-flight delivery. Publishers keep using plain
//! core NATS (`EventBus::publish`) — JetStream captures any message
//! published to a subject the stream covers, so no publisher changes.

use async_nats::jetstream::consumer::{AckPolicy, pull};
use async_nats::jetstream::{AckKind, Context, Message, stream};
use std::time::Duration;
use tracing::{error, warn};

/// Canonical durable stream name. One stream buffers every subject the
/// dispatcher must consume reliably.
pub const STREAM_NAME: &str = "BOSS_EVENTS";

/// Redelivery budget: how many times a NAK'd message is retried before it
/// is dead-lettered. Sized (with [`redelivery_backoff`]) to ride out a
/// multi-minute transient — DB saturation, a briefly-unreachable
/// downstream — then give up rather than loop a poison event forever.
pub const MAX_DELIVER: i64 = 8;

/// How long the server waits for an ACK before assuming the consumer died
/// and redelivering. The consumers NAK explicitly on handler failure, so
/// this is the crash-safety backstop, not the normal retry path.
///
/// This only holds if no `backoff` schedule is set on the consumer: the
/// server treats a backoff list as the per-attempt ack deadline, so a
/// schedule starting at 1s silently shrank the effective ack_wait to 1s
/// — any event whose queue-wait + handler time crossed 1s was redelivered
/// while its first delivery was still succeeding, and every side effect
/// ran twice with no NAK anywhere in the logs (facts/JEs survived on
/// idempotency keys; unconditional audit-event emits doubled). Retry
/// pacing therefore lives in [`settle`]'s explicit NAK delay, never in
/// the consumer config.
const ACK_WAIT: Duration = Duration::from_secs(30);

/// Subjects the stream captures. MUST cover every family the rule
/// registry references: a durable consumer's filter may legally name
/// subjects the stream doesn't ingest, and the result is zero
/// deliveries with zero errors — perfectly silent dead air. That is
/// exactly how 6b's `commerce.invoice.created` rule (the consume that
/// owns COGS) fired zero times across a full year run while every
/// gate stayed green (2026-07-10): the filter accepted the topic, the
/// stream never stored the messages. The
/// `stream_covers_every_rule_topic` test in boss-dispatcher pins this
/// list against the shipped rule registry — and writing that test
/// immediately surfaced a SECOND casualty: the tax-filing webhook
/// forward (`ledger.tax_filing_filed`) had been dead air the same
/// way. Domain families with no rule topic still flow over core NATS
/// only; the test starts failing the moment a rule consumes one.
pub fn stream_subjects() -> Vec<String> {
    // `asset.>` joined 2026-08-08 (feedback `0da79b36`): the assets
    // ingress rebuilds its repository from these events, and consuming
    // them at-most-once off core NATS silently dropped writes. The
    // subject must be HERE for the `assets-ingress` durable consumer
    // to receive anything — `ensure_stream` reconciles a live broker's
    // subject list on every service start, so adding a subject in code
    // reaches prod on deploy.
    [
        "jobs.>",
        "step.>",
        "inventory.>",
        "commerce.>",
        "ledger.>",
        "asset.>",
        // `docs.>` joined 2026-08-10 (dogfooding arc e556c000, S2):
        // the design-review-spawn rule consumes `docs.design.indexed`,
        // and the coverage tripwire below this comment's lineage
        // (stream_covers_every_rule_topic) failed the build until the
        // family flowed into the stream — the exact silent-zero class
        // it was written for, caught at compile-adjacent time instead
        // of in production.
        "docs.>",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Per-attempt redelivery spacing. Entry `i` is the delay before delivery
/// `i+2` (delivery 1 is the original). The schedule widens so a transient
/// condition has time to clear, and the tail repeats at 30s until
/// [`MAX_DELIVER`] is hit.
///
/// Supplied per-NAK via [`redelivery_delay`] — NEVER configured as the
/// consumer's server-side `backoff`, because a backoff schedule replaces
/// `ack_wait` as the per-attempt ack deadline and silently redelivers
/// slow-but-healthy deliveries (see [`ACK_WAIT`]). An explicit NAK delay
/// paces only genuine failures.
fn redelivery_backoff() -> Vec<Duration> {
    vec![
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(10),
        Duration::from_secs(20),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ]
}

/// The NAK delay for a failed delivery, by its (1-based) attempt number:
/// the [`redelivery_backoff`] schedule, clamped to its 30s tail.
fn redelivery_delay(delivered: i64) -> Duration {
    let sched = redelivery_backoff();
    let idx = usize::try_from(delivered.max(1) - 1)
        .unwrap_or(0)
        .min(sched.len() - 1);
    sched[idx]
}

/// Errors from the durable layer. Kept stringly-typed so the async-nats
/// error zoo doesn't leak into the public signature.
#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error("jetstream stream error: {0}")]
    Stream(String),
    #[error("jetstream consumer error: {0}")]
    Consumer(String),
}

/// The canonical [`STREAM_NAME`] stream configuration. A bounded,
/// file-backed `Limits`-retention buffer: messages are kept until they age
/// past [`max_age`](stream::Config::max_age) or the byte cap is hit,
/// independent of ACKs, so multiple durable consumers can each read the
/// full set. (`WorkQueue` retention — delete-on-first-ack — would force
/// non-overlapping consumer filters and couple the two dispatcher loops;
/// `Limits` keeps them independent.)
pub fn stream_config() -> stream::Config {
    stream::Config {
        name: STREAM_NAME.to_string(),
        subjects: stream_subjects(),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        // 3 days wall-clock. Real (wall-paced) ops keep only a small
        // window; a high-warp regen lands a whole sim-year within minutes
        // of wall time, so nothing ages out mid-run — the byte cap bounds
        // that case.
        max_age: Duration::from_secs(3 * 24 * 60 * 60),
        // 4 GiB ceiling (server file store is 8 GiB). Comfortably exceeds a
        // full sim-year of job/step/inventory events.
        max_bytes: 4 * 1024 * 1024 * 1024,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Idempotently ensure the [`STREAM_NAME`] stream exists. Safe to call from
/// every connecting service; the first creates it, the rest no-op.
pub async fn ensure_stream(ctx: &Context) -> Result<(), DurableError> {
    let mut stream = ctx
        .get_or_create_stream(stream_config())
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    // get_or_create returns an EXISTING stream verbatim — a deployed
    // box keeps its old subject list forever unless someone updates
    // it. That drift is how a code-side subjects change silently
    // doesn't apply in prod: the consumer filters accept the new
    // topic, the stream never ingests it. Reconcile on every start.
    let info = stream
        .info()
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    let mut have = info.config.subjects.clone();
    let mut want = stream_subjects();
    have.sort();
    want.sort();
    if have != want {
        ctx.update_stream(stream_config())
            .await
            .map_err(|e| DurableError::Stream(format!("updating stream subjects: {e}")))?;
    }
    Ok(())
}

/// Delete the stream (dropping its messages **and** durable consumers) and
/// recreate it empty. For dev/regen resets ONLY: a fresh regen resets
/// Postgres, so the delivery buffer must be cleared too or the durable
/// consumers would replay the previous run's events against the new
/// database (every replayed step-completion 404s → fails the health gate).
/// Never call this on a normal production restart — that would discard
/// in-flight events the whole durability layer exists to protect.
pub async fn reset_stream(ctx: &Context) -> Result<(), DurableError> {
    // Best-effort delete; an absent stream is fine (first run).
    let _ = ctx.delete_stream(STREAM_NAME).await;
    ensure_stream(ctx).await
}

/// Drop every message currently buffered on `stream_name`, keeping the
/// stream and its durable consumers in place.
///
/// This is the delivery-buffer half of a demo epoch restart. That
/// restart trims `audit_log` back to the seed baseline, so anything
/// still sitting in the buffer at that moment belongs to the epoch being
/// deleted — redelivering it against the trimmed database 404s through
/// the whole redelivery budget and dead-letters. The dead-letter count
/// is a release gate whose entire value is reading zero, so a restart
/// that reliably manufactures spurious entries erodes the signal. Same
/// reasoning as the `TRUNCATE event_outbox` one hop upstream: at restart
/// time every pending item is the finished epoch's by definition, and
/// must die with it rather than relay into the new one.
///
/// Purge, NOT [`reset_stream`]: reset deletes the stream and with it
/// every durable consumer, forcing a live dispatcher to re-create its
/// consumers mid-flight. A purge leaves the consumers attached; the
/// server advances their floors past the dropped messages.
pub async fn purge_stream(ctx: &Context, stream_name: &str) -> Result<(), DurableError> {
    let stream = ctx
        .get_stream(stream_name)
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    stream
        .purge()
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    Ok(())
}

/// Durable pull-consumer configuration for the given durable name and
/// filter subjects. Explicit ACK + bounded redelivery is the whole point:
/// without it a failed handler would drop the event.
pub fn consumer_config(durable: &str, filter_subjects: Vec<String>) -> pull::Config {
    pull::Config {
        durable_name: Some(durable.to_string()),
        name: Some(durable.to_string()),
        ack_policy: AckPolicy::Explicit,
        ack_wait: ACK_WAIT,
        max_deliver: MAX_DELIVER,
        filter_subjects,
        // Deliberately NO `backoff` here: the server would use it as the
        // per-attempt ack deadline (overriding ack_wait), so backoff[0]=1s
        // silently duplicated every delivery slower than 1s. The retry
        // schedule rides each NAK instead (see `settle`).
        ..Default::default()
    }
}

/// Open (creating if needed) a durable pull consumer on [`STREAM_NAME`] and
/// return its message stream. Each yielded [`Message`] must be settled with
/// [`settle`] (ACK on success, NAK/dead-letter on failure) or it will be
/// redelivered after `ack_wait`.
///
/// `stream_name` is a parameter (not hardcoded to [`STREAM_NAME`]) only so
/// tests can exercise the mechanics on a throwaway stream; production
/// passes [`STREAM_NAME`].
pub async fn open_durable(
    ctx: &Context,
    stream_name: &str,
    durable: &str,
    filter_subjects: Vec<String>,
) -> Result<pull::Stream, DurableError> {
    let stream = ctx
        .get_stream(stream_name)
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    // create-or-UPDATE (not get_or_create): reconciles a persisted
    // consumer whose config drifted from `consumer_config` — durable
    // consumers outlive binary deploys, so without the update an old
    // config (e.g. the server-side `backoff` that used to override
    // ack_wait) would survive every restart. Update preserves the
    // consumer's cursor; only the config changes.
    let consumer = stream
        .create_consumer(consumer_config(durable, filter_subjects))
        .await
        .map_err(|e| DurableError::Consumer(e.to_string()))?;
    consumer
        .messages()
        .await
        .map_err(|e| DurableError::Consumer(e.to_string()))
}

/// Pure policy: has this message exhausted its redelivery budget? The
/// server reports `delivered` as a 1-based attempt count, so the
/// [`MAX_DELIVER`]-th delivery is the last.
pub fn is_dead_letter(delivered: i64) -> bool {
    delivered >= MAX_DELIVER
}

/// Settle a delivered message per the reliability policy:
/// - `Ok`  → ACK; the stream advances past it.
/// - `Err` before the budget is spent → NAK; the server redelivers it on
///   the [`redelivery_backoff`] schedule once the transient clears.
/// - `Err` on the final attempt → dead-letter: a loud `error!` (the source
///   event is recoverable from `audit_log`) and `Term` so it stops cycling.
///
/// Generic over the error type so callers can pass their own (`anyhow`,
/// `&str`, …) without this crate depending on it.
/// Three-way settle outcome. `Retry` NAKs for redelivery (converges
/// once the transient cause clears, dead-letters after the budget);
/// `Permanent` skips the budget entirely: a deterministic data error
/// (a seed typo, a malformed body) fails identically on every
/// redelivery, so the 8 NAKs it used to burn were pure noise and a
/// multi-minute delay before the SAME dead-letter. The log line is the
/// same `DEAD-LETTER:` pattern either way — the health gates and
/// operators see one vocabulary, with `class=permanent` marking the
/// fast path.
pub enum Settle {
    Ack,
    Retry(String),
    Permanent(String),
}

pub async fn settle_classified(msg: &Message, outcome: Settle) {
    match outcome {
        Settle::Ack => {
            if let Err(e) = msg.ack().await {
                warn!(subject = %msg.subject, error = %e, "JetStream ACK failed; message will redeliver");
            }
        }
        Settle::Permanent(err) => {
            let delivered = msg.info().map(|i| i.delivered).unwrap_or(MAX_DELIVER);
            error!(
                subject = %msg.subject,
                delivered,
                class = "permanent",
                error = %err,
                "DEAD-LETTER: deterministic data error; terminating without redelivery (source recoverable from audit_log)"
            );
            let _ = msg.ack_with(AckKind::Term).await;
        }
        Settle::Retry(err) => settle(msg, Err::<(), _>(err)).await,
    }
}

pub async fn settle<E: std::fmt::Display>(msg: &Message, outcome: Result<(), E>) {
    match outcome {
        Ok(()) => {
            if let Err(e) = msg.ack().await {
                warn!(subject = %msg.subject, error = %e, "JetStream ACK failed; message will redeliver");
            }
        }
        Err(err) => {
            // `info()` only fails to parse on a non-JetStream message,
            // which can't reach here; treat an unreadable count as the
            // final attempt so a broken message can't loop forever.
            let delivered = msg.info().map(|i| i.delivered).unwrap_or(MAX_DELIVER);
            if is_dead_letter(delivered) {
                error!(
                    subject = %msg.subject,
                    delivered,
                    max_deliver = MAX_DELIVER,
                    error = %err,
                    "DEAD-LETTER: event exhausted redelivery budget; dropping (source recoverable from audit_log)"
                );
                let _ = msg.ack_with(AckKind::Term).await;
            } else {
                // Pacing rides the NAK itself (a bare NAK redelivers
                // immediately) — the consumer carries no server-side
                // backoff, which would double as the ack deadline.
                let delay = redelivery_delay(delivered);
                warn!(
                    subject = %msg.subject,
                    delivered,
                    retry_in_secs = delay.as_secs(),
                    error = %err,
                    "handler failed; NAK for redelivery"
                );
                let _ = msg.ack_with(AckKind::Nak(Some(delay))).await;
            }
        }
    }
}

/// Collapse a set of rule subjects to the minimal non-overlapping set of
/// first-token wildcards suitable as JetStream consumer `filter_subjects`.
///
/// The rules registry references many precise subjects (`step.done.*`,
/// `step.ready.*`, `inventory.item.consumed`, `jobs.job.closed`). A single
/// pull consumer can't take overlapping filters (the server rejects
/// `step.ready.*` alongside `step.ready.delegate-subjob`), and one consumer
/// per subject would redeliver an event once per matching filter. Coarsening
/// every subject to its `<first-token>.>` wildcard yields a deduplicated,
/// trivially-non-overlapping set (distinct first tokens never overlap); the
/// rules runner's own `match_event` still does the precise matching, and the
/// few extra subjects pulled in (e.g. `jobs.step.*` under `jobs.>`) match no
/// rule and are ACK'd as cheap no-ops. Growth-proof: a new `step.foo.*` rule
/// is already covered by `step.>`.
pub fn coarse_filter_subjects(subjects: &[String]) -> Vec<String> {
    let mut out: Vec<String> = subjects
        .iter()
        .filter_map(|s| s.split('.').next())
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}.>"))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_config_is_a_bounded_file_buffer() {
        let c = stream_config();
        assert_eq!(c.name, "BOSS_EVENTS");
        assert_eq!(
            c.subjects,
            vec![
                "jobs.>",
                "step.>",
                "inventory.>",
                "commerce.>",
                "ledger.>",
                "asset.>",
                "docs.>"
            ]
        );
        assert_eq!(c.retention, stream::RetentionPolicy::Limits);
        assert_eq!(c.storage, stream::StorageType::File);
        assert!(c.max_bytes > 0, "byte cap must bound the buffer");
        assert!(!c.max_age.is_zero(), "age cap must expire the buffer");
    }

    #[test]
    fn consumer_config_enables_explicit_ack_and_bounded_redelivery() {
        let c = consumer_config("dispatcher-rules", vec!["step.>".to_string()]);
        assert_eq!(c.durable_name.as_deref(), Some("dispatcher-rules"));
        assert!(matches!(c.ack_policy, AckPolicy::Explicit));
        assert_eq!(c.max_deliver, MAX_DELIVER);
        assert_eq!(c.filter_subjects, vec!["step.>".to_string()]);
        // A server-side backoff schedule REPLACES ack_wait as the
        // per-attempt ack deadline — backoff[0]=1s silently redelivered
        // every event slower than 1s and ran its side effects twice.
        // Retry pacing must ride the NAK (redelivery_delay), never the
        // consumer config.
        assert!(
            c.backoff.is_empty(),
            "consumer backoff overrides ack_wait; pacing belongs on the NAK"
        );
        assert_eq!(
            c.ack_wait, ACK_WAIT,
            "ack_wait is the crash backstop and must survive intact"
        );
    }

    #[test]
    fn redelivery_delay_walks_the_schedule_and_clamps_to_the_tail() {
        assert_eq!(redelivery_delay(1), Duration::from_secs(1));
        assert_eq!(redelivery_delay(2), Duration::from_secs(2));
        assert_eq!(redelivery_delay(3), Duration::from_secs(5));
        // Beyond the schedule (and any nonsense attempt number) → 30s tail.
        assert_eq!(redelivery_delay(7), Duration::from_secs(30));
        assert_eq!(redelivery_delay(100), Duration::from_secs(30));
        assert_eq!(redelivery_delay(0), Duration::from_secs(1));
        assert_eq!(
            redelivery_backoff().len() as i64,
            MAX_DELIVER - 1,
            "one gap per redelivery (deliveries = attempts; gaps = attempts-1)"
        );
    }

    #[test]
    fn dead_letter_only_at_the_budget() {
        assert!(!is_dead_letter(1), "first delivery is never a dead-letter");
        assert!(!is_dead_letter(MAX_DELIVER - 1));
        assert!(is_dead_letter(MAX_DELIVER), "the Nth delivery is the last");
        assert!(is_dead_letter(MAX_DELIVER + 1));
    }

    #[test]
    fn coarse_filters_collapse_to_nonoverlapping_first_tokens() {
        let subjects = vec![
            "step.done.*".to_string(),
            "step.ready.*".to_string(),
            "step.ready.delegate-subjob".to_string(),
            "inventory.item.consumed".to_string(),
            "jobs.job.closed".to_string(),
        ];
        // Sorted, deduped, one wildcard per distinct first token — and the
        // overlapping step.ready.* / step.ready.delegate-subjob collapse to
        // a single `step.>` that the server will accept.
        assert_eq!(
            coarse_filter_subjects(&subjects),
            vec![
                "inventory.>".to_string(),
                "jobs.>".to_string(),
                "step.>".to_string(),
            ]
        );
    }

    #[test]
    fn coarse_filters_handle_wildcards_and_single_tokens() {
        let subjects = vec![
            "jobs.step.>".to_string(),
            "jobs.job.closed".to_string(),
            "lonely".to_string(),
        ];
        assert_eq!(
            coarse_filter_subjects(&subjects),
            vec!["jobs.>".to_string(), "lonely.>".to_string()]
        );
    }

    #[test]
    fn coarse_filters_empty_input_is_empty() {
        assert!(coarse_filter_subjects(&[]).is_empty());
    }
}
