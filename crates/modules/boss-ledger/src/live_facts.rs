//! The live half of the fact projection (backlog 5621d166).
//!
//! `gl_fact_projection_rules` was read by the facts rebuild alone (and
//! the replay-check): a fact whose only writer is the registry — the
//! sponsorship receipt, a `step.done.task` picked out by `when` — reached
//! `financial_facts` at the next rebuild, not when the step completed.
//! The ADR named that the open half. This module closes it in the
//! smallest honest shape: the ledger binary binds a durable consumer on
//! the platform's event stream, and for every delivered event runs the
//! SAME [`project_event`] the rebuild runs, then posts the fact's journal
//! entry through the SAME `post_fact_in_tx` the domain writers and the
//! rebuild use. One definition each. Both are idempotent — the fact by
//! its UUIDv5 natural-key identity (`record_fact_in_tx`), the entry by
//! `UNIQUE (fact_id, rule_version_id)` — so a rebuild after a live
//! projection changes nothing, and a redelivery writes nothing twice.
//!
//! What the live path does NOT own: it is not the reconciliation. A
//! projection that fails (a payload missing the rule's field, a locked
//! period, a fact kind with no posting rule) is logged with the event
//! id and ACKed — it never stops the subscriber — and the nightly
//! `maintenance-ledger-replay` chore (`boss-ledger-replay-check --deep`)
//! remains the read that finds what the live path missed. A storage
//! failure NAKs for redelivery on the platform's schedule.
//!
//! Filtering: the consumer's `filter_subjects` are the coarse
//! first-token wildcards of the rule set's event kinds (the same
//! collapse the dispatcher's rules runner uses); every delivery is then
//! matched precisely against the rules by [`ProjectionRules::matching`]
//! — kind equality plus the rule's `when`. The rule set is re-read from
//! the registry every [`RULES_REFRESH`], so a newly published
//! projection fires without a restart; if the refresh introduces a
//! subject family the consumer does not yet filter on, the consumer is
//! re-opened (create-or-update keeps its cursor).

use std::time::Duration;

use boss_core::event::Event;
use sqlx::PgPool;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::error::LedgerError;
use crate::events::record_fact_in_tx;
use crate::postgres::post_fact_in_tx;
use crate::rebuild_facts::{ProjectedFact, ProjectionError, ProjectionRules, project_event};
use crate::types::FactRef;

/// The durable consumer's name on the platform stream. One per
/// deployment: two ledger binaries would share the cursor, which is
/// the right thing (each event projected once).
pub const DURABLE_NAME: &str = "ledger-facts";

/// How often the subscriber re-reads `gl_fact_projection_rules`.
pub const RULES_REFRESH: Duration = Duration::from_secs(60);

/// How long the subscriber waits before retrying a consumer it could
/// not open (NATS still starting, JetStream absent). It keeps trying
/// for the life of the process rather than disabling itself silently.
const REOPEN_DELAY: Duration = Duration::from_secs(30);

/// Project the facts one live event yields, without touching storage:
/// every rule matching `(kind, when)` fires, exactly as the rebuild's
/// per-row loop does. The event's own `timestamp`, `source` and
/// `payload` are passed through — the same three columns the audit
/// writer stores verbatim — which is what makes the live fact and the
/// rebuilt fact the same bytes.
pub fn project_live(
    rules: &ProjectionRules,
    event: &Event,
) -> Vec<Result<ProjectedFact, ProjectionError>> {
    rules
        .matching(&event.kind, &event.payload)
        .map(|rule| project_event(rule, event.timestamp, &event.source, &event.payload))
        .collect()
}

/// Rule event kinds the platform stream does not ingest. A durable
/// consumer may legally filter on a subject the stream never stores,
/// and the result is zero deliveries with zero errors — the silent
/// dead-air class `stream_subjects` documents. The stream ingests whole
/// families (`<token>.>`), so a kind is covered iff its family is
/// listed. Named at start and on every refresh so the gap is loud; the
/// facts those kinds carry are still written in-tx by their domain
/// writers and reproduced by the rebuild.
pub fn uncovered_by_stream(event_kinds: &[String]) -> Vec<String> {
    let stream = boss_nats::durable::stream_subjects();
    event_kinds
        .iter()
        .filter(|kind| {
            let family = boss_nats::durable::coarse_filter_subjects(std::slice::from_ref(kind));
            !family.iter().all(|f| stream.contains(f))
        })
        .cloned()
        .collect()
}

/// What projecting one event did. `permanent` holds the failures that
/// would fail identically on redelivery (logged, never retried);
/// `retry` the first storage failure, which asks for redelivery.
#[derive(Debug, Default)]
pub struct LiveOutcome {
    /// Fact ids recorded (or found) and posted, one per matching rule.
    pub facts: Vec<Uuid>,
    pub permanent: Vec<String>,
    pub retry: Option<String>,
}

/// Project and post every fact `event` yields, one transaction per
/// matching rule so a rule that fails leaves its siblings' facts
/// committed. Idempotent end to end: a fact that exists resolves to
/// its id and is not posted again (see `record_and_post`).
pub async fn project_live_event(
    pool: &PgPool,
    rules: &ProjectionRules,
    event: &Event,
) -> LiveOutcome {
    let mut out = LiveOutcome::default();
    for projected in project_live(rules, event) {
        let projected = match projected {
            Ok(p) => p,
            Err(e) => {
                out.permanent.push(e.to_string());
                continue;
            }
        };
        match record_and_post(pool, &projected).await {
            Ok(id) => out.facts.push(id),
            // Only Postgres itself failing is transient; every other
            // LedgerError is a deterministic verdict on this fact
            // (locked period, unknown kind, unbalanced, cash floor).
            Err(LedgerError::Storage(e)) => {
                out.retry = Some(e);
                break;
            }
            Err(e) => out.permanent.push(e.to_string()),
        }
    }
    out
}

async fn record_and_post(pool: &PgPool, projected: &ProjectedFact) -> Result<Uuid, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let recorded = record_fact_in_tx(&mut tx, projected.as_write()).await?;
    // Post ONLY the fact this call inserted. A fact that already
    // existed was handled by whoever wrote it — its domain writer
    // posted it in-tx, a rebuild posted it, or an operator retired it
    // (a supersede marks the row and DROPS its entry; `post_fact_in_tx`
    // does not read `supersede_reason`, so posting here would
    // resurrect the retraction). The stream buffers three days and a
    // fresh consumer starts at the head, so pre-existing facts are the
    // common case on first start, not a corner.
    if recorded.inserted {
        post_fact_in_tx(
            &mut tx,
            &FactRef {
                id: recorded.id,
                kind: &projected.fact_kind,
                happened_on: projected.happened_on,
                payload: &projected.payload,
            },
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(recorded.id)
}

/// Settle classification for one delivery: ACK when nothing asks for a
/// redelivery (a permanent failure is logged here, with the event id,
/// and ACKed — it fails the same way every time), NAK on a storage
/// failure.
fn settle_for(event: &Event, outcome: &LiveOutcome) -> boss_nats::durable::Settle {
    for why in &outcome.permanent {
        error!(
            event_id = %event.id,
            kind = %event.kind,
            class = "permanent",
            error = %why,
            "live fact projection failed for this event; the nightly replay-check is the reconciliation"
        );
    }
    match &outcome.retry {
        Some(e) => boss_nats::durable::Settle::Retry(e.clone()),
        None => boss_nats::durable::Settle::Ack,
    }
}

async fn load_rules(pool: &PgPool) -> Result<ProjectionRules, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let rules = ProjectionRules::load_in_tx(&mut tx).await?;
    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(rules)
}

/// Run the subscriber until `cancel` flips. Opens (create-or-update) the
/// durable consumer over the rule set's subject families, projects and
/// posts every delivery, re-reads the rules every [`RULES_REFRESH`], and
/// re-opens the consumer when the families change. A consumer that
/// cannot be opened is retried every [`REOPEN_DELAY`] — loudly, never
/// silently off.
pub async fn run(
    bus: std::sync::Arc<boss_nats::NatsEventBus>,
    pool: PgPool,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    use futures::StreamExt;

    let js = bus.jetstream();
    let mut rules = match load_rules(&pool).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "live facts: cannot read gl_fact_projection_rules; subscriber off until restart");
            return;
        }
    };
    'consumer: loop {
        let filters = rules.filter_subjects();
        let uncovered = uncovered_by_stream(&rules.event_kinds());
        if !uncovered.is_empty() {
            warn!(
                kinds = ?uncovered,
                "live facts: projection rules on event kinds the platform stream does not ingest — \
                 those facts arrive only from their domain writer or the rebuild"
            );
        }
        let messages = match boss_nats::durable::open_durable(
            &js,
            boss_nats::durable::STREAM_NAME,
            DURABLE_NAME,
            filters.clone(),
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, retry_in_secs = REOPEN_DELAY.as_secs(),
                    "live facts: durable consumer unavailable; facts reach the ledger at the next rebuild until it opens");
                tokio::select! {
                    _ = cancel.changed() => { if *cancel.borrow() { return; } }
                    _ = tokio::time::sleep(REOPEN_DELAY) => {}
                }
                continue 'consumer;
            }
        };
        info!(
            ?filters,
            rules = rules.len(),
            durable = DURABLE_NAME,
            "live facts: durable consumer open"
        );
        let mut messages = std::pin::pin!(messages);
        let mut refresh = tokio::time::interval(RULES_REFRESH);
        refresh.tick().await; // the first tick is immediate; the rules were just read
        loop {
            tokio::select! {
                _ = cancel.changed() => {
                    if *cancel.borrow() { return; }
                }
                _ = refresh.tick() => {
                    match load_rules(&pool).await {
                        Ok(fresh) => {
                            let changed = fresh.filter_subjects() != filters;
                            rules = fresh;
                            if changed {
                                info!("live facts: rule subject families changed; re-opening the consumer");
                                continue 'consumer;
                            }
                        }
                        Err(e) => warn!(error = %e, "live facts: rules refresh failed; keeping the last set"),
                    }
                }
                maybe_msg = messages.next() => {
                    let Some(msg) = maybe_msg else {
                        warn!("live facts: message stream ended; re-opening");
                        continue 'consumer;
                    };
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, "live facts: message stream error");
                            continue;
                        }
                    };
                    let event: Event = match serde_json::from_slice(&msg.payload) {
                        Ok(ev) => ev,
                        Err(e) => {
                            warn!(error = %e, subject = %msg.subject, "live facts: non-Event payload (ACK)");
                            let _ = msg.ack().await;
                            continue;
                        }
                    };
                    let outcome = project_live_event(&pool, &rules, &event).await;
                    if !outcome.facts.is_empty() {
                        info!(event_id = %event.id, kind = %event.kind, facts = ?outcome.facts, "live facts: projected and posted");
                    }
                    boss_nats::durable::settle_classified(&msg, settle_for(&event, &outcome)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posting_rules::ProjectionRule;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    fn sponsorship_rule() -> ProjectionRule {
        ProjectionRule {
            event_kind: "step.done.task".into(),
            fact_kind: "finance.sponsorship.received".into(),
            source_table: "jobs".into(),
            source_id_path: "/job_id".into(),
            happened_on_path: Some("/completed_on".into()),
            created_by_path: None,
            when: Some(
                json!({"/workflow_kind": "receive-a-sponsorship", "/spec_slug": "recognize"}),
            ),
        }
    }

    fn invoice_rule() -> ProjectionRule {
        ProjectionRule {
            event_kind: "commerce.invoice.created".into(),
            fact_kind: "finance.invoice.issued".into(),
            source_table: "invoices".into(),
            source_id_path: "/id".into(),
            happened_on_path: Some("/issued_on".into()),
            created_by_path: None,
            when: None,
        }
    }

    fn recognize_event(workflow_kind: &str, slug: &str) -> Event {
        let ts: DateTime<Utc> = "2026-09-17T18:20:16Z".parse().unwrap();
        Event::new(
            "jobs",
            "step.done.task",
            json!({
                "job_id": "job-sponsor", "step_id": "s1", "kind": "task",
                "workflow_kind": workflow_kind, "spec_slug": slug,
                "completed_on": "2026-09-17",
                "metadata": {"amount_cents": "100", "fee_cents": "33"},
                "_actor": "emp-david",
            }),
            ts,
        )
    }

    #[test]
    fn the_filter_fires_on_kind_and_when_only() {
        let rules = ProjectionRules::new(vec![sponsorship_rule(), invoice_rule()]);
        // The sponsorship workflow's recognize step: one fact.
        let hit = project_live(
            &rules,
            &recognize_event("receive-a-sponsorship", "recognize"),
        );
        assert_eq!(hit.len(), 1);
        assert_eq!(
            hit[0].as_ref().unwrap().fact_kind,
            "finance.sponsorship.received"
        );
        // Same slug on another workflow, another slug on the same
        // workflow, a kind no rule names: nothing.
        assert!(project_live(&rules, &recognize_event("close-the-month", "recognize")).is_empty());
        assert!(
            project_live(
                &rules,
                &recognize_event("receive-a-sponsorship", "reconcile")
            )
            .is_empty()
        );
        let other = Event::new(
            "jobs",
            "jobs.job.closed",
            json!({"job_id": "x"}),
            Utc::now(),
        );
        assert!(project_live(&rules, &other).is_empty());
    }

    #[test]
    fn the_live_fact_is_the_rebuilt_fact_byte_for_byte() {
        // The rebuild reads (timestamp, source, kind, payload) off the
        // audit_log row, and the audit writer stores exactly the Event's
        // fields. So the rebuild's per-row projection over those four
        // must equal the live projection over the Event — id, payload
        // (envelope stripped), happened_on, source_table, created_by.
        let rules = ProjectionRules::new(vec![sponsorship_rule()]);
        let event = recognize_event("receive-a-sponsorship", "recognize");
        let live = project_live(&rules, &event).pop().unwrap().unwrap();
        // What rebuild_facts_in_tx does with the row.
        let rebuilt = rules
            .matching(&event.kind, &event.payload)
            .map(|r| project_event(r, event.timestamp, &event.source, &event.payload).unwrap())
            .next()
            .unwrap();
        assert_eq!(live, rebuilt);
        assert_eq!(live.source_id, "job-sponsor");
        assert_eq!(live.created_by, "jobs");
        assert!(
            live.payload.get("_actor").is_none(),
            "envelope stripped on both paths"
        );
        // And the identity the row will carry is the natural key's.
        assert_eq!(
            crate::events::deterministic_fact_id(
                &live.fact_kind,
                &live.source_table,
                &live.source_id
            ),
            crate::events::deterministic_fact_id(
                "finance.sponsorship.received",
                "jobs",
                "job-sponsor"
            )
        );
    }

    #[test]
    fn a_missing_field_is_a_permanent_failure_not_a_match() {
        let rules = ProjectionRules::new(vec![sponsorship_rule()]);
        let mut event = recognize_event("receive-a-sponsorship", "recognize");
        event.payload["job_id"] = serde_json::Value::Null;
        let out = project_live(&rules, &event);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Err(ProjectionError::MissingField { .. })));
    }

    #[test]
    fn filter_subjects_are_the_rules_families_and_uncovered_kinds_are_named() {
        let rules = ProjectionRules::new(vec![
            sponsorship_rule(),
            invoice_rule(),
            ProjectionRule {
                event_kind: "products.consumed".into(),
                fact_kind: "finance.cogs.recognized".into(),
                source_table: "products_consume".into(),
                source_id_path: "/id".into(),
                happened_on_path: None,
                created_by_path: None,
                when: None,
            },
        ]);
        assert_eq!(
            rules.filter_subjects(),
            vec![
                "commerce.>".to_string(),
                "products.>".to_string(),
                "step.>".to_string()
            ]
        );
        // `products.>` is not a family the platform stream ingests
        // (40-ledger.sql seeds two rules on it; their facts are written
        // in-tx by boss-products) — named, not silently dead air.
        assert_eq!(
            uncovered_by_stream(&rules.event_kinds()),
            vec!["products.consumed".to_string()]
        );
    }

    #[test]
    fn settle_is_ack_unless_storage_asked_for_redelivery() {
        let event = recognize_event("receive-a-sponsorship", "recognize");
        let clean = LiveOutcome::default();
        assert!(matches!(
            settle_for(&event, &clean),
            boss_nats::durable::Settle::Ack
        ));
        let permanent = LiveOutcome {
            permanent: vec!["no posting rule".into()],
            ..Default::default()
        };
        assert!(matches!(
            settle_for(&event, &permanent),
            boss_nats::durable::Settle::Ack
        ));
        let transient = LiveOutcome {
            retry: Some("connection reset".into()),
            ..Default::default()
        };
        assert!(matches!(
            settle_for(&event, &transient),
            boss_nats::durable::Settle::Retry(_)
        ));
    }
}
