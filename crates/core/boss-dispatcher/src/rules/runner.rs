//! NATS runner — subscribes to all topics referenced by the rule
//! registry, feeds incoming events through the matcher, and dispatches
//! matched rules to the handler registry.
//!
//! Separate from `dispatcher::run_loop` (which handles role-based
//! step assignment) so the two concerns stay decoupled. The main
//! binary starts both; they share the same NATS connection but
//! subscribe to disjoint topic sets.

use super::dead_letter::{
    DeadLetterClass, DeadLetterNote, DeadLetterSink, HandlerFailure, annotation_target,
};
use super::expr::HelperResolver;
use super::handler::{self, HandlerRegistry};
use super::registry::{self, Registry};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Max events processed in parallel. A strictly-serial loop awaits each
/// event's handlers (1-2 HTTP round-trips) before pulling the next, which
/// caps the runner at ~tens of events/sec; at warp that trails job
/// generation and lets ready markers + steps pile up over a long regen.
/// This is the side-effect path (invoice / COGS / shipping / ledger), so it
/// must keep pace with the 16-wide workforce's completions. Raised 6 → 12
/// now that the cluster runs max_connections=400 (was 100): the fan-out
/// across module services has pool headroom, and a transient "too many
/// clients" blip is NAK'd + redelivered by the JetStream layer rather than
/// lost. Saturate the DB, don't error it — dead-letters reappearing is the
/// signal this went too high.
const MAX_CONCURRENT_EVENTS: usize = 12;

/// What the rules runner needs to operate.
pub struct RulesRunner {
    pub registry: Registry,
    pub handlers: HandlerRegistry,
    pub helpers: Arc<dyn HelperResolver + Send + Sync>,
    /// Where a dead-letter is recorded so it outlives this pod (see
    /// [`super::dead_letter`]). `None` runs the loop with the log as the
    /// only record — the pre-`a9c498eb` behaviour, kept for the tests
    /// that are about matching and settling rather than about visibility.
    pub dead_letters: Option<Arc<dyn DeadLetterSink>>,
}

impl RulesRunner {
    /// Compute the set of NATS topics to subscribe to. Walks the
    /// rule registry, deduplicating identical `on_event` patterns.
    /// Schedule-triggered rules contribute nothing — they fire off the
    /// clock stream, not a NATS subscription.
    pub fn subscriptions(&self) -> HashSet<String> {
        self.registry
            .rules()
            .iter()
            .filter_map(|r| r.event_pattern().map(|p| p.raw().to_string()))
            .collect()
    }

    /// Bind a durable JetStream consumer covering every topic the registry
    /// references and run the match-then-dispatch loop. Runs until the
    /// message stream ends (NATS disconnect).
    ///
    /// Durability is the point: each event is ACK'd only after its handlers
    /// succeed. A handler failure NAKs the message, so the server redelivers
    /// it on a backoff schedule and the work self-heals once a transient
    /// condition clears — instead of the silent drop that orphaned Jobs
    /// under plain core-NATS subscribe.
    /// Consume the rules feed from `audit_log` by id cursor instead
    /// of JetStream — log-as-the-bus, stage 1 (transactional-audit-log
    /// Q6). Same `handle`, same Settle semantics; delivery is the
    /// [`crate::rules::log_tail`] contract: per-item durable advance,
    /// retry blocks the cursor on the JetStream pacing schedule,
    /// budget exhaustion dead-letters loudly and moves on. Selected
    /// by `BOSS_RULES_SOURCE=log`; the JetStream path below remains
    /// the default until the cutover is observed.
    pub async fn run_log_tail(
        &self,
        pool: sqlx::PgPool,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
    ) -> Result<()> {
        use crate::rules::log_tail::{LogTail, retry_delay};
        let mut tail = LogTail::new(pool, "dispatcher-rules-log");
        tail.ensure_cursor().await?;
        live.mark_rules_running();
        info!("rules runner: tailing audit_log as 'dispatcher-rules-log' (BOSS_RULES_SOURCE=log)");
        loop {
            let report = tail
                .drain_once(500, |topic, event_id, payload, attempt| async move {
                    self.handle(&topic, &event_id, &payload, attempt).await
                })
                .await;
            match report {
                Ok(r) => {
                    if r.processed > 0 || r.dead_lettered > 0 {
                        live.record_rules();
                    }
                    match r.blocked {
                        Some(b) => tokio::time::sleep(retry_delay(b.attempts)).await,
                        // Idle: one poll interval behind the relay's
                        // own ~200ms lag — side effects stay
                        // sub-second behind the write.
                        None => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                    }
                }
                Err(e) => {
                    warn!(error = %e, "rules log tail: drain error; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    pub async fn run(
        &self,
        js: async_nats::jetstream::Context,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
    ) -> Result<()> {
        let subjects: Vec<String> = self.subscriptions().into_iter().collect();
        if subjects.is_empty() {
            info!("rules runner: no rules registered, nothing to consume");
            return Ok(());
        }
        // One consumer over the coarse (first-token) wildcards covering the
        // registry's subjects — non-overlapping, so the server accepts it and
        // each event arrives exactly once. `handle` re-matches precisely via
        // `match_event`; the few extra subjects pulled in match no rule and
        // ACK as no-ops.
        let filters = boss_nats::durable::coarse_filter_subjects(&subjects);
        info!(
            ?filters,
            durable = "dispatcher-rules",
            "rules runner: opening durable consumer"
        );
        let messages = boss_nats::durable::open_durable(
            &js,
            boss_nats::durable::STREAM_NAME,
            "dispatcher-rules",
            filters,
        )
        .await
        .context("opening durable rules consumer")?;
        // Consumer bound — the step-completion side-effect path is live (see
        // crate::liveness::DispatcherLiveness; a process that's health-200 but
        // whose rules consumer died runs zero side-effects and stalls Jobs).
        live.mark_rules_running();

        // Process events with bounded concurrency (see MAX_CONCURRENT_EVENTS).
        // `handle` only reads shared state (the rule registry + stateless
        // Arc'd handlers), so distinct events process safely in parallel;
        // a single Job's step chain stays ordered because each event only
        // fires after its predecessor's completion lands.
        messages
            .for_each_concurrent(MAX_CONCURRENT_EVENTS, |msg| {
                let live = live.clone();
                async move {
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, "rules runner: message stream error");
                            return;
                        }
                    };
                    let subject = msg.subject.to_string();
                    let envelope: Value = match serde_json::from_slice(&msg.payload) {
                        Ok(v) => v,
                        Err(e) => {
                            // Unparseable: redelivery can't help — ACK so it
                            // doesn't loop, and move on.
                            debug!(error = %e, subject = %subject, "skip non-JSON event (ACK)");
                            let _ = msg.ack().await;
                            return;
                        }
                    };
                    // events come as {id, timestamp, source, kind, payload}.
                    let event_id = envelope
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let payload = envelope.get("payload").cloned().unwrap_or(envelope);
                    // The delivery count `settle` will read, read here so
                    // `handle` knows whether a failure now is the last
                    // one the budget allows. Same fallback as `settle`:
                    // an unreadable count counts as the final attempt, so
                    // a broken message records its dead-letter rather
                    // than vanishing into a Term.
                    let attempt = msg
                        .info()
                        .map(|i| i.delivered)
                        .unwrap_or(boss_nats::durable::MAX_DELIVER)
                        .max(1) as u32;
                    let outcome = self.handle(&subject, &event_id, &payload, attempt).await;
                    // ACK on success; NAK (→ redeliver) on transient failure —
                    // dead-letter once the budget is spent; immediate Term on a
                    // permanent failure (deterministic data error — every
                    // redelivery fails identically, so the budget is noise).
                    // `settle_classified` does the failure logging
                    // (deliberately NOT phrased as the health-gate pattern for
                    // NAKs — a transient that self-heals is not a defect; only
                    // a dead-letter is, and permanent failures log as one).
                    boss_nats::durable::settle_classified(&msg, outcome).await;
                    live.record_rules();
                }
            })
            .await;
        live.mark_rules_stopped();
        info!("rules runner: message stream ended");
        Ok(())
    }

    /// Match and dispatch one event.
    ///
    /// `attempt` is the 1-based delivery count of THIS presentation, and
    /// it is a parameter rather than something the transport keeps to
    /// itself because the dead-letter has to be recorded here: this is
    /// the only moment the rule, the handler, the attempt count and the
    /// error are all in one place (`a9c498eb`). The transport still owns
    /// the settling; `handle` only has to know that a failure now is the
    /// last one the budget allows.
    async fn handle(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
        attempt: u32,
    ) -> boss_nats::durable::Settle {
        use boss_nats::durable::Settle;
        let registry::MatchOutcome { matched, skipped } =
            registry::match_event(&self.registry, topic, payload, self.helpers.as_ref());
        // A rule that cannot be evaluated is skipped, LOUDLY, and its
        // neighbours still fire. It used to abort matching for the whole
        // topic, so one bad arg reference silenced every rule bound to
        // that subject until the event dead-lettered.
        //
        // These never retry. A predicate that will not evaluate or an
        // arg that names a field the payload does not carry fails
        // identically on every redelivery - it is a defect in the rule
        // text, not a transient. Burning the redelivery budget on it
        // only delays the same outcome, which is what the eight NAKs
        // before the 2026-08-24 dead-letter bought us.
        for s in &skipped {
            error!(
                rule = %s.rule,
                topic = %topic,
                triggering_event = %event_id,
                class = "permanent",
                error = %s.err,
                "RULE-SKIPPED: rule could not be evaluated and did not fire; \
                 other rules on this topic were unaffected (fix the rule and republish)"
            );
        }
        if matched.is_empty() {
            return Settle::Ack;
        }
        let results =
            match handler::dispatch(&matched, &self.handlers, event_id, topic, payload).await {
                Ok(r) => r,
                Err(e) => {
                    return Settle::Retry(format!("dispatching matched rules for {topic}: {e}"));
                }
            };
        // Collect handler failures and propagate them: a failed handler must
        // surface as an `Err` here so the caller NAKs the message and the
        // server redelivers it. The previous version logged failures but
        // returned `Ok`, which under JetStream would ACK away a dropped side
        // effect — exactly the silent-orphaning this layer exists to stop.
        //
        // NOTE: a NAK redelivers the whole event, re-running EVERY matched
        // handler — including any that already succeeded. For multi-handler
        // subjects (`step.done.production-produce`, `step.done.shipment`)
        // the handlers must therefore be idempotent on their source key, or
        // a partial failure double-applies the survivors on retry.
        let mut failures: Vec<HandlerFailure> = Vec::new();
        let mut all_permanent = true;
        for r in results {
            match &r.outcome {
                Ok(()) => {
                    // Per-fire log at DEBUG, not INFO: at warp the runner
                    // fires tens of rules/sec, and an INFO line each
                    // flooded syslog (26G incident, 2026-06-23). Failures
                    // still surface via the `bail!` below.
                    debug!(
                        rule = %r.rule_name,
                        handler = %r.handler,
                        triggering_event = %event_id,
                        "rule fired"
                    );
                }
                Err(e) => {
                    if !e.is_permanent() {
                        all_permanent = false;
                    }
                    failures.push(HandlerFailure {
                        rule: r.rule_name.clone(),
                        handler: r.handler.clone(),
                        error: e.to_string(),
                    });
                }
            }
        }
        if !failures.is_empty() {
            let msg = format!(
                "{} handler(s) failed on {topic}: {}",
                failures.len(),
                failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            // Term only when EVERY failure is deterministic: if any
            // transient failure is present, NAK — the idempotent re-run
            // lets the transients converge, and the ride-along permanent
            // failures re-fail harmlessly until the event either fully
            // converges or Terms on a later all-permanent pass.
            let settle = if all_permanent {
                boss_nats::durable::Settle::Permanent(msg)
            } else {
                boss_nats::durable::Settle::Retry(msg)
            };
            // A dead-letter lands on its packet (`a9c498eb`). The settle
            // is already decided above and this cannot change it — see
            // `record_dead_letter`.
            let class = match &settle {
                boss_nats::durable::Settle::Permanent(_) => Some(DeadLetterClass::Permanent),
                boss_nats::durable::Settle::Retry(_)
                    if boss_nats::durable::is_dead_letter(attempt as i64) =>
                {
                    Some(DeadLetterClass::BudgetExhausted)
                }
                // Budget left: the NAK will redeliver and the work may
                // still self-heal. Annotating here would mark a packet
                // troubled that is about to be fine.
                _ => None,
            };
            if let Some(class) = class {
                self.record_dead_letter(topic, event_id, payload, attempt, class, failures)
                    .await;
            }
            return settle;
        }
        boss_nats::durable::Settle::Ack
    }

    /// Land a dead-letter on the packet whose step the handler owed.
    ///
    /// BEST-EFFORT, DELIBERATELY, in three ways — this is the arm, and an
    /// arm that needs the patient is not an arm (CLAUDE.md §Diagnosis):
    ///
    /// 1. It returns `()`. The settle is computed by the caller BEFORE
    ///    this runs and is returned regardless, so a failed annotation
    ///    can never re-enter the redelivery budget it is reporting on —
    ///    which would mean a down jobs API turned every dead-letter into
    ///    eight more deliveries of the same event.
    /// 2. Its own failure is logged and dropped. The `DEAD-LETTER:` line
    ///    the transport writes is still the floor; this only adds a
    ///    record that outlives the pod.
    /// 3. The write carries a timeout (see the sink), so a jobs API that
    ///    accepts and never answers costs the loop seconds, not the loop.
    ///
    /// With no target (a topic whose subject is not a packet) there is
    /// nothing to annotate, and saying so out loud is the honest answer —
    /// pointing the annotation at whatever packet shares the subject's
    /// uuid would file a false record.
    async fn record_dead_letter(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
        attempt: u32,
        class: DeadLetterClass,
        failures: Vec<HandlerFailure>,
    ) {
        let Some(sink) = &self.dead_letters else {
            return;
        };
        let Some(target) = annotation_target(topic, payload) else {
            warn!(
                topic = %topic,
                triggering_event = %event_id,
                attempts = attempt,
                class = class.as_str(),
                "dead-letter carries no packet to annotate: this topic's subject is not a Job, \
                 so the log line above is the only record (a dead-letter counter is the follow-up)"
            );
            return;
        };
        let note = DeadLetterNote {
            topic: topic.to_string(),
            event_id: event_id.to_string(),
            attempts: attempt,
            class,
            failures,
            // The same read `handler::dispatch` makes per invocation, so
            // an annotation about simulated work is simulated state.
            simulated: payload
                .get("_simulated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        match sink.record(&target, &note).await {
            Ok(()) => info!(
                job_id = %target.job_id,
                step_id = ?target.step_id,
                topic = %topic,
                attempts = attempt,
                class = class.as_str(),
                "dead-letter recorded on its packet"
            ),
            // Not an error! return and not a retry: see (1) above.
            Err(e) => error!(
                job_id = %target.job_id,
                topic = %topic,
                attempts = attempt,
                error = %e,
                "DEAD-LETTER annotation failed; the packet still looks untouched and this \
                 pod's log is the only record"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::NoHelpers;
    use super::*;
    use crate::rules::dead_letter::Target;

    /// The first delivery — a failure here still has budget left.
    const FIRST: u32 = 1;
    /// The last delivery the budget allows: a failure here dead-letters.
    const FINAL: u32 = boss_nats::durable::MAX_DELIVER as u32;

    /// A sink that records what it was asked to land, and can be made to
    /// fail the way a down jobs API would.
    struct RecordingDeadLetters {
        landed: tokio::sync::Mutex<Vec<(Target, DeadLetterNote)>>,
        fails: bool,
    }

    impl RecordingDeadLetters {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                landed: tokio::sync::Mutex::new(Vec::new()),
                fails: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                landed: tokio::sync::Mutex::new(Vec::new()),
                fails: true,
            })
        }
        async fn landed(&self) -> Vec<(Target, DeadLetterNote)> {
            self.landed.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl DeadLetterSink for RecordingDeadLetters {
        async fn record(&self, target: &Target, note: &DeadLetterNote) -> Result<(), String> {
            self.landed
                .lock()
                .await
                .push((target.clone(), note.clone()));
            if self.fails {
                return Err("PATCH /api/jobs/j-1/metadata: connection refused".into());
            }
            Ok(())
        }
    }

    #[test]
    fn subscriptions_dedupes_repeated_topics() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r2"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h2"
[[rule]]
name = "r3"
on_event = "inventory.parts.consumed"
[[rule.do]]
handler = "h3"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let runner = RulesRunner {
            registry: reg,
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
        };
        let subs = runner.subscriptions();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains("step.done.procurement"));
        assert!(subs.contains("inventory.parts.consumed"));
    }

    #[test]
    fn empty_registry_has_no_subscriptions() {
        let runner = RulesRunner {
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
        };
        assert!(runner.subscriptions().is_empty());
    }

    #[tokio::test]
    async fn handle_propagates_handler_failure() {
        // The load-bearing durable contract: a failed handler must surface
        // as `Err` from `handle` so the caller NAKs the message and the
        // server redelivers it. The pre-JetStream version logged the failure
        // and returned `Ok`, which under a durable consumer would ACK away a
        // dropped side effect — the exact silent-orphaning this layer ends.
        let toml = r#"
[[rule]]
name = "r-fail"
on_event = "step.done.billing"
[[rule.do]]
handler = "boom"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler::FailingHandler::new("boom", "policy unreachable"));
        let runner = RulesRunner {
            registry: reg,
            handlers,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
        };
        let payload = serde_json::json!({
            "job_id": "j1", "step_id": "s1", "kind": "billing"
        });
        let res = runner
            .handle("step.done.billing", "evt-1", &payload, FIRST)
            .await;
        // A transient failure must surface as Retry so the message NAKs.
        match res {
            boss_nats::durable::Settle::Retry(msg) => assert!(
                msg.contains("boom"),
                "the propagated error should name the failed handler: {msg}"
            ),
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn handle_is_ok_when_no_rule_matches() {
        // No matched rule = nothing to do = ACK (not a failure). Keeps the
        // consumer from NAK-looping the many events no rule cares about.
        let runner = RulesRunner {
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
        };
        let res = runner
            .handle("step.done.unmatched", "evt", &serde_json::json!({}), FIRST)
            .await;
        assert!(
            matches!(res, boss_nats::durable::Settle::Ack),
            "an unmatched event must ACK, not retry"
        );
    }

    struct FixedHandler {
        name: &'static str,
        result: fn() -> Result<(), crate::rules::handler::HandlerError>,
    }
    #[async_trait::async_trait]
    impl crate::rules::handler::Handler for FixedHandler {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn invoke(
            &self,
            _args: &[(String, crate::rules::expr::Value)],
            _ctx: &crate::rules::handler::InvocationContext,
        ) -> Result<(), crate::rules::handler::HandlerError> {
            (self.result)()
        }
    }

    fn runner_with(
        handlers: Vec<(
            &'static str,
            fn() -> Result<(), crate::rules::handler::HandlerError>,
        )>,
        rules_toml: &str,
    ) -> RulesRunner {
        let mut reg = HandlerRegistry::new();
        for (name, result) in handlers {
            reg.register(std::sync::Arc::new(FixedHandler { name, result }));
        }
        RulesRunner {
            registry: Registry::from_toml(rules_toml).unwrap(),
            handlers: reg,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
        }
    }

    const TWO_HANDLER_RULES: &str = r#"
[[rule]]
name = "r-perm"
on_event = "step.done.x"
[[rule.do]]
handler = "h.perm"
[[rule]]
name = "r-trans"
on_event = "step.done.x"
[[rule.do]]
handler = "h.trans"
"#;

    /// Queue item 4 (HandlerError::Permanent): all-deterministic
    /// failures Term immediately; ANY transient in the mix NAKs so the
    /// idempotent re-run can converge the transients.
    #[tokio::test]
    async fn all_permanent_failures_term_immediately() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Permanent("422 second typo".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}), FIRST)
            .await
        {
            boss_nats::durable::Settle::Permanent(msg) => {
                assert!(msg.contains("seed typo"), "{msg}");
            }
            other => panic!("expected Permanent, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn any_transient_failure_naks_for_redelivery() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Downstream("503 not yet".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}), FIRST)
            .await
        {
            boss_nats::durable::Settle::Retry(msg) => {
                assert!(msg.contains("2 handler(s) failed"), "{msg}");
            }
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn success_acks() {
        let runner = runner_with(
            vec![("h.perm", || Ok(())), ("h.trans", || Ok(()))],
            TWO_HANDLER_RULES,
        );
        assert!(matches!(
            runner
                .handle("step.done.x", "e1", &serde_json::json!({}), FIRST)
                .await,
            boss_nats::durable::Settle::Ack
        ));
    }

    // -----------------------------------------------------------------
    // A dead-letter lands on its packet (`a9c498eb`)
    // -----------------------------------------------------------------

    const ONE_RULE: &str = r#"
[[rule]]
name = "inspect-empty-decisions-sweep-on-step-ready"
on_event = "step.ready.checklist"
[[rule.do]]
handler = "maintenance.sweep.inspect"
"#;

    fn sweep_payload() -> Value {
        // The shape `step.ready.*` publishes, as the measured instance
        // carried it: packet f8dedadf's Inspect checklist.
        serde_json::json!({
            "job_id": "f8dedadf-0000-0000-0000-000000000001",
            "step_id": "5ca1ab1e-0000-0000-0000-000000000002",
            "kind": "checklist",
        })
    }

    fn sweep_runner(
        result: fn() -> Result<(), crate::rules::handler::HandlerError>,
        sink: Arc<RecordingDeadLetters>,
    ) -> RulesRunner {
        let mut runner = runner_with(vec![("maintenance.sweep.inspect", result)], ONE_RULE);
        runner.dead_letters = Some(sink);
        runner
    }

    /// THE DEFECT. A handler that fails past its budget left the packet
    /// exactly as it was: the step sitting `ready`, and the only record of
    /// the failure in a pod log that a converge roll erases. The
    /// dead-letter must land on the packet, naming the rule, the handler,
    /// the attempt count and the error.
    #[tokio::test]
    async fn a_budget_exhausted_failure_lands_on_the_packet() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(
            || {
                Err(HandlerError::Downstream(
                    "GET /api/jobs returned 503".into(),
                ))
            },
            sink.clone(),
        );
        let settle = runner
            .handle("step.ready.checklist", "evt-sweep", &sweep_payload(), FINAL)
            .await;
        assert!(
            matches!(settle, boss_nats::durable::Settle::Retry(_)),
            "the settle is unchanged: the transport still Terms the spent message"
        );
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 1, "one dead-letter, landed once");
        let (target, note) = &landed[0];
        assert_eq!(target.job_id, "f8dedadf-0000-0000-0000-000000000001");
        assert_eq!(
            target.step_id.as_deref(),
            Some("5ca1ab1e-0000-0000-0000-000000000002"),
            "the troubled step is named: it is the one a reader finds `ready`"
        );
        assert_eq!(note.attempts, FINAL);
        assert_eq!(note.class, DeadLetterClass::BudgetExhausted);
        assert_eq!(note.topic, "step.ready.checklist");
        assert_eq!(note.event_id, "evt-sweep");
        let f = note.failures.first().expect("one failure");
        assert_eq!(f.rule, "inspect-empty-decisions-sweep-on-step-ready");
        assert_eq!(f.handler, "maintenance.sweep.inspect");
        assert!(
            f.error.contains("503"),
            "the note carries what failed, not that something did: {}",
            f.error
        );
    }

    /// A permanent failure dead-letters on its FIRST delivery (it skips
    /// the budget), so the annotation must not wait for the count.
    #[tokio::test]
    async fn a_permanent_failure_lands_on_its_first_delivery() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(
            || Err(HandlerError::Permanent("422 unknown sweep target".into())),
            sink.clone(),
        );
        let settle = runner
            .handle("step.ready.checklist", "evt-sweep", &sweep_payload(), FIRST)
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Permanent(_)));
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 1, "a Term IS a dead-letter: {landed:?}");
        assert_eq!(landed[0].1.class, DeadLetterClass::Permanent);
        assert_eq!(landed[0].1.attempts, FIRST);
    }

    /// With budget left the NAK may still converge. Marking the packet
    /// troubled now would cry wolf on work that is about to be fine.
    #[tokio::test]
    async fn a_transient_failure_with_budget_left_annotates_nothing() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        for attempt in FIRST..FINAL {
            let settle = runner
                .handle(
                    "step.ready.checklist",
                    "evt-sweep",
                    &sweep_payload(),
                    attempt,
                )
                .await;
            assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        }
        assert!(
            sink.landed().await.is_empty(),
            "nothing is annotated while redelivery can still succeed"
        );
    }

    /// BEST-EFFORT. The annotation is itself a jobs-API write, and the
    /// jobs API may be the very thing that failed. Its failure must not
    /// change the settle — or a down API would turn one dead-letter into
    /// eight more deliveries of the same event.
    #[tokio::test]
    async fn an_annotation_that_fails_does_not_change_the_settle() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::failing();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        let settle = runner
            .handle("step.ready.checklist", "evt-sweep", &sweep_payload(), FINAL)
            .await;
        match settle {
            boss_nats::durable::Settle::Retry(msg) => assert!(
                msg.contains("maintenance.sweep.inspect"),
                "the settle still names the handler that failed: {msg}"
            ),
            other => panic!(
                "a failed annotation must leave the settle alone, got {}",
                settle_name(&other)
            ),
        }
        assert_eq!(
            sink.landed().await.len(),
            1,
            "it was attempted once and not retried"
        );
    }

    /// IDEMPOTENCE. A redelivery (or a restart re-presenting the row with
    /// a fresh budget) must leave one annotation's worth of metadata: one
    /// top-level key, overwritten. See `dead_letter::annotation_patch`
    /// for the merge itself.
    #[tokio::test]
    async fn two_dead_letter_deliveries_write_one_metadata_key() {
        use crate::rules::dead_letter::{METADATA_KEY, annotation_patch};
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        for _ in 0..2 {
            runner
                .handle("step.ready.checklist", "evt-sweep", &sweep_payload(), FINAL)
                .await;
        }
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 2, "both deliveries annotate");
        let mut metadata = serde_json::Map::new();
        for (_, note) in &landed {
            for (k, v) in annotation_patch(note, chrono::Utc::now())
                .as_object()
                .expect("patch is an object")
            {
                metadata.insert(k.clone(), v.clone());
            }
        }
        assert_eq!(
            metadata.keys().collect::<Vec<_>>(),
            vec![METADATA_KEY],
            "two deliveries, one annotation's worth of metadata"
        );
    }

    /// A topic whose subject is not a packet has nothing to annotate. The
    /// honest answer is the log line, NOT a guess at which job shares the
    /// subject's uuid.
    #[tokio::test]
    async fn an_event_with_no_packet_annotates_nothing() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let mut runner = runner_with(
            vec![("h.invoice", || Err(HandlerError::Downstream("503".into())))],
            r#"
[[rule]]
name = "r-invoice"
on_event = "commerce.invoice.paid"
[[rule.do]]
handler = "h.invoice"
"#,
        );
        runner.dead_letters = Some(sink.clone());
        let settle = runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv",
                &serde_json::json!({"id": "inv-1"}),
                FINAL,
            )
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        assert!(
            sink.landed().await.is_empty(),
            "no packet means no annotation, not a fabricated one"
        );
    }

    /// Sim-ness is inherited from the triggering event, the same read
    /// `handler::dispatch` makes, so an annotation about simulated work
    /// is simulated state.
    #[tokio::test]
    async fn the_annotation_inherits_the_events_sim_ness() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        let mut payload = sweep_payload();
        payload["_simulated"] = serde_json::json!(true);
        runner
            .handle("step.ready.checklist", "evt-sweep", &payload, FINAL)
            .await;
        assert!(sink.landed().await[0].1.simulated);
    }

    /// Both transports must agree on when the budget is spent, because
    /// the runner predicts the dead-letter from the attempt count. The
    /// log tail derives its budget from the JetStream one; this is the
    /// assertion that the derivation is the whole of it.
    #[test]
    fn both_transports_share_one_retry_budget() {
        assert_eq!(
            crate::rules::log_tail::MAX_ATTEMPTS as i64,
            boss_nats::durable::MAX_DELIVER
        );
        assert!(boss_nats::durable::is_dead_letter(FINAL as i64));
        assert!(!boss_nats::durable::is_dead_letter(FINAL as i64 - 1));
    }

    fn settle_name(s: &boss_nats::durable::Settle) -> &'static str {
        match s {
            boss_nats::durable::Settle::Ack => "Ack",
            boss_nats::durable::Settle::Retry(_) => "Retry",
            boss_nats::durable::Settle::Permanent(_) => "Permanent",
        }
    }
}
