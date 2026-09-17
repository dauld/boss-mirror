//! Rebuild `financial_facts` from `audit_log` via the
//! `gl_fact_projection_rules` registry.
//!
//! Each rule maps one real-world `audit_log.kind` to one
//! `financial_facts.kind`, optionally only for the events whose
//! payload matches the rule's `when` (`{"/pointer": value}`, every
//! pointer equal) — which is how ONE workflow's completed step becomes
//! a fact out of a `step.done.task` every workflow emits (backlog
//! a40541cb). The projection extracts `source_id`, `happened_on`, and
//! `created_by` from `event.payload` via JSON pointers (RFC 6901),
//! passes the payload through verbatim, and upserts via
//! `record_fact_in_tx` — idempotent on the natural key
//! `(kind, source_table, source_id)`.
//!
//! Determinism: `fact_id` derivation lives in `record_fact_in_tx`
//! (UUIDv5 over the natural key — see
//! `events::deterministic_fact_id`), shared with every live writer,
//! so a rebuilt fact carries the SAME id as its live twin and the
//! deep replay-check can compare journal entries keyed on
//! `(fact_id, rule_version_id)` across live and replay.
//!
//! v1 ships UPSERT-only semantics. Operators who want a clean
//! rebuild truncate `financial_facts` (cascading through
//! `gl_journal_entries`) externally first. The replay-check
//! (separate function) is the read-only verifier.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::error::LedgerError;
use crate::events::{FactWrite, record_fact_in_tx};
use crate::supersede::replay_supersede_events_in_tx;

/// Advisory-lock key for the fact-rebuild, derived from the projection
/// name. Serializes concurrent fact-rebuilds the same way `rebuild`
/// serializes concurrent ledger-rebuilds.
const REBUILD_FACTS_LOCK_KEY: i64 = boss_core::rebuild::lock_key("ledger-facts");

/// The registry row, shared with the tenant loader and the batch door
/// (one definition, CLAUDE.md §9a). Carries `when`.
pub use crate::posting_rules::ProjectionRule;

#[derive(Debug, Clone)]
pub struct RebuildFactsReport {
    pub rules_loaded: u64,
    pub events_scanned: u64,
    pub facts_written: u64,
    pub events_skipped_missing_field: u64,
    /// `ledger.fact.superseded` events applied after the projection
    /// pass. Non-zero values mean the operator retired one or more
    /// historically-bad facts via the supersede endpoint; the
    /// rebuild reproduces those retractions so the live state stays
    /// stable across rebuilds.
    pub supersedes_applied: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error(
        "event payload missing required field at {path} (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    MissingField {
        path: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
    #[error(
        "event payload field at {path} is not a string (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    WrongType {
        path: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
    #[error(
        "happened_on path {path} resolved to {value} which is not a valid YYYY-MM-DD date (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    InvalidDate {
        path: String,
        value: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
}

/// Project one audit_log event into a `FactWrite` per the rule.
/// Returns `None` if the rule doesn't apply (kind mismatch — this is
/// caller-side filtered already, but the check is cheap).
pub fn project_event(
    rule: &ProjectionRule,
    event_timestamp: DateTime<Utc>,
    event_source: &str,
    event_payload: &Value,
) -> Result<ProjectedFact, ProjectionError> {
    let source_id = pointer_string(event_payload, &rule.source_id_path).ok_or_else(|| {
        ProjectionError::MissingField {
            path: rule.source_id_path.clone(),
            rule_event_kind: rule.event_kind.clone(),
            rule_fact_kind: rule.fact_kind.clone(),
        }
    })?;

    let happened_on = match &rule.happened_on_path {
        Some(path) => {
            let raw = pointer_string(event_payload, path).ok_or_else(|| {
                ProjectionError::MissingField {
                    path: path.clone(),
                    rule_event_kind: rule.event_kind.clone(),
                    rule_fact_kind: rule.fact_kind.clone(),
                }
            })?;
            NaiveDate::parse_from_str(&raw, "%Y-%m-%d").map_err(|_| {
                ProjectionError::InvalidDate {
                    path: path.clone(),
                    value: raw,
                    rule_event_kind: rule.event_kind.clone(),
                    rule_fact_kind: rule.fact_kind.clone(),
                }
            })?
        }
        None => event_timestamp.date_naive(),
    };

    let created_by = match &rule.created_by_path {
        Some(path) => {
            pointer_string(event_payload, path).unwrap_or_else(|| event_source.to_string())
        }
        None => event_source.to_string(),
    };

    // Payload-authoritative source_table. The ledger movement
    // endpoints fold the caller's source_table INTO the payload
    // precisely so rebuild can reproduce the original provenance tag
    // (the 40-ledger.sql rule comments have claimed this since #51) —
    // but until 2026-07-09 the code always took the rule's fixed
    // value, so a live 'brewery_seed_opening_balance' fact rebuilt as
    // 'manual_inventory_transferred': a different natural key, a
    // different deterministic fact id, a live-vs-replay divergence on
    // every fresh (never-rebuilt) deployment. Events that don't carry
    // the key (produce/consume/absorb/invoice) fall back to the rule.
    let source_table = event_payload
        .get("source_table")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| rule.source_table.clone());

    Ok(ProjectedFact {
        fact_kind: rule.fact_kind.clone(),
        happened_on,
        payload: strip_envelope(event_payload),
        source_table,
        source_id,
        created_by,
    })
}

/// Owned form of a projection result. The lifetime-bearing
/// `FactWrite` shape is too painful to thread out of an inner loop;
/// callers convert this to a `FactWrite` at the insertion site. The
/// fact id is not part of the projection: `record_fact_in_tx` derives
/// it from the natural key, identically for live writes and replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedFact {
    pub fact_kind: String,
    pub happened_on: NaiveDate,
    pub payload: Value,
    pub source_table: String,
    pub source_id: String,
    pub created_by: String,
}

impl ProjectedFact {
    pub fn as_write(&self) -> FactWrite<'_> {
        FactWrite {
            kind: &self.fact_kind,
            happened_on: self.happened_on,
            payload: &self.payload,
            source_table: Some(&self.source_table),
            source_id: Some(&self.source_id),
            created_by: &self.created_by,
        }
    }
}

/// Walk `audit_log` and project every event matching a registered
/// rule into `financial_facts`. UPSERT semantics — idempotent against
/// re-runs.
pub async fn rebuild_facts(pool: &PgPool) -> Result<RebuildFactsReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_FACTS_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let report = rebuild_facts_in_tx(&mut tx).await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(report)
}

/// Caller-controlled-transaction variant. Useful when composing with
/// `rebuild` for an audit-log-rooted replay-check that wants both
/// stages inside one rollback boundary.
pub async fn rebuild_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<RebuildFactsReport, LedgerError> {
    let rules = load_rules_in_tx(tx).await?;
    let event_kinds: Vec<String> = rules.keys().cloned().collect();

    // TRUNCATE-then-replay model. financial_facts is a pure
    // projection of audit_log; no row may live here that doesn't
    // trace back to an event. The gl_journal_entries FK references
    // financial_facts.id so the CASCADE drops the JE rows in the
    // same step (the ledger-journal rebuild step in rebuild-all
    // re-projects them right after). Opening balances survive the
    // truncate the same way everything else does: the endpoint or
    // in-process writer that records the fact emits the matching
    // `ledger.inventory.transferred` event, and this replay
    // re-projects it — never via source_table-scoped exclusion.
    sqlx::query("TRUNCATE financial_facts CASCADE")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // The gl_account_daily rollup is NOT reachable by the CASCADE above
    // (it FKs gl_accounts, not the journal), but the journal it summarizes
    // was just wiped. Clear it too so the full journal re-post below
    // (post_fact_in_tx increments it per entry) rebuilds it from a clean
    // slate — keeping rebuild_facts self-consistent even when run without
    // a following `rebuild()` (which would also re-aggregate it).
    sqlx::query("TRUNCATE gl_account_daily")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // `ORDER BY id` — audit_log id order IS commit order (the chain
    // trigger allocates ids under the advisory lock), which is the
    // order the live path recorded facts in. Ordering by `timestamp`
    // stopped being meaningful when sim time was retired from the
    // record (David, 2026-08-22, packet a7a4cae5): a log holding
    // old sim-stamped rows next to new wall-stamped rows has no
    // coherent timestamp order, and the old tiebreak (`event_id`, a
    // random UUID) never matched commit order on equal stamps anyway.
    let event_rows = sqlx::query(
        "SELECT event_id, timestamp, source, kind, payload \
         FROM audit_log \
         WHERE kind = ANY($1) \
         ORDER BY id",
    )
    .bind(&event_kinds)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut events_scanned: u64 = 0;
    let mut facts_written: u64 = 0;
    let mut events_skipped_missing_field: u64 = 0;

    for row in &event_rows {
        events_scanned += 1;
        let timestamp: DateTime<Utc> = row.get("timestamp");
        let source: String = row.get("source");
        let kind: String = row.get("kind");
        let payload: Value = row.get("payload");

        let Some(candidates) = rules.get(&kind) else {
            continue;
        };

        // One event kind may carry several rules (one per `when`);
        // every rule whose filter matches fires.
        for rule in candidates.iter().filter(|r| r.when_matches(&payload)) {
            let projected = match project_event(rule, timestamp, &source, &payload) {
                Ok(p) => p,
                Err(ProjectionError::MissingField { .. }) => {
                    events_skipped_missing_field += 1;
                    continue;
                }
                Err(e) => return Err(LedgerError::Storage(e.to_string())),
            };

            record_fact_in_tx(tx, projected.as_write()).await?;
            facts_written += 1;
        }
    }

    // GL-inert reprojection pass — kept OFF the `gl_fact_projection_rules`
    // registry on purpose. The registry exists to drive the GL: every
    // fact kind it reconstructs is also posted to a journal by the active
    // RuleSet. `finance.inventory.received` is the lone fact that must be
    // reconstructable from the log AND post nothing — the goods-receipt's
    // DR-1300 rides the idempotent bill-approval path, so a GL entry here
    // would double-post it. Registering it would force exactly that. So
    // its reprojection lives here, hardcoded, and the journal-posting
    // stage (`post_fact_in_tx`) skips the inert kind. The result is
    // symmetric with consume's INVENTORY_TRANSFERRED reconstruction
    // (emit event → rebuild the fact from it), minus the GL leg.
    facts_written += rebuild_inert_received_facts_in_tx(tx).await?;

    // After projection, re-apply every recorded supersede so the
    // rebuilt set matches the live state. Without this pass, a
    // rebuild from audit_log would resurrect rows that the operator
    // had explicitly retired via the supersede endpoint, and the
    // replay-check would flag false-positive divergences.
    let supersedes_applied = replay_supersede_events_in_tx(tx).await?;

    Ok(RebuildFactsReport {
        rules_loaded: rules.values().map(Vec::len).sum::<usize>() as u64,
        events_scanned,
        facts_written,
        events_skipped_missing_field,
        supersedes_applied,
    })
}

/// Audit-log kind the inert receive reprojection consumes.
const RECEIVE_EVENT_KIND: &str = "inventory.item.received";
/// The GL-inert dedup-fact kind it reconstructs. Deliberately absent
/// from `gl_fact_projection_rules` AND from the RuleSet match in
/// `rules.rs`, so the journal-posting path (`post_fact_in_tx`) skips it
/// via `crate::rules::is_gl_inert`.
const RECEIVE_FACT_KIND: &str = "finance.inventory.received";
/// `source_table` written verbatim — matches the in-tx
/// `insert_dedup_fact` call in boss-inventory so the live fact and the
/// rebuilt fact share a natural key and the replay-check diff is clean.
const RECEIVE_SOURCE_TABLE: &str = "inventory_receipt";

/// Reproject `inventory.item.received` audit events into the GL-inert
/// `finance.inventory.received` dedup-fact. Mirrors the registry pass
/// (`record_fact_in_tx`, idempotent on the natural key, deterministic
/// `fact_id`) but stays hardcoded here precisely BECAUSE it must not be
/// in `gl_fact_projection_rules` — see the call site for why. The fact's
/// `(source_table, source_id, happened_on, payload, created_by)` are
/// reproduced byte-for-byte from what boss-inventory wrote in-tx
/// (`inventory_receipt` / `/source_id` / `/received_on` / payload
/// verbatim / `inventory`), so a live receive and a rebuilt receive land
/// the identical row. Returns the number of facts written.
async fn rebuild_inert_received_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<u64, LedgerError> {
    // `ORDER BY id` = commit order — same reasoning as the registry
    // pass above; timestamp order is incoherent across the sim-to-
    // wall stamp transition.
    let rows = sqlx::query(
        "SELECT event_id, timestamp, payload \
         FROM audit_log \
         WHERE kind = $1 \
         ORDER BY id",
    )
    .bind(RECEIVE_EVENT_KIND)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut written: u64 = 0;
    for row in &rows {
        let timestamp: DateTime<Utc> = row.get("timestamp");
        let payload: Value = row.get("payload");

        // `source_id` keys the dedup-fact; an event without it is malformed
        // — skip rather than fail the whole rebuild (same leniency the
        // registry pass gives a missing source_id field).
        let Some(source_id) = pointer_string(&payload, "/source_id") else {
            continue;
        };
        // `received_on` carries the dedup-fact's happened_on. Fall back to
        // the event date if (impossibly) absent, mirroring the registry's
        // NULL-happened_on_path behavior.
        let happened_on = pointer_string(&payload, "/received_on")
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .unwrap_or_else(|| timestamp.date_naive());

        // Match the in-tx live fact, which never carries the publisher's
        // `_actor`/`_simulated` envelope keys (same reason as the registry
        // pass — see `strip_envelope`).
        let fact_payload = strip_envelope(&payload);
        record_fact_in_tx(
            tx,
            FactWrite {
                kind: RECEIVE_FACT_KIND,
                happened_on,
                payload: &fact_payload,
                source_table: Some(RECEIVE_SOURCE_TABLE),
                source_id: Some(&source_id),
                // Matches the `created_by` the in-tx `insert_dedup_fact`
                // stamps ('inventory'); keeps the replay-check diff clean.
                created_by: "inventory",
            },
        )
        .await?;
        written += 1;
    }
    Ok(written)
}

/// The registry grouped by event kind, each group in registry order.
async fn load_rules_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<HashMap<String, Vec<ProjectionRule>>, LedgerError> {
    let mut out: HashMap<String, Vec<ProjectionRule>> = HashMap::new();
    for rule in crate::posting_rules::load_projection_rules_in_tx(tx).await? {
        out.entry(rule.event_kind.clone()).or_default().push(rule);
    }
    Ok(out)
}

/// Strip the publisher-injected event-envelope keys (`_actor`,
/// `_simulated`) from a payload so a rebuilt fact matches the live in-tx
/// fact, which never carries them. The `EventStamp` envelope
/// stamps `_actor` onto every event payload (and `_simulated` under a
/// SimulatedProbe) for provenance and sim-filtering — that is envelope
/// metadata, not domain-fact data, so it must not leak into
/// `financial_facts.payload` (doing so makes the rebuilt fact diverge
/// from the live one and breaks the fact-level replay-check).
/// Non-object payloads (arrays, scalars) pass through unchanged.
fn strip_envelope(payload: &Value) -> Value {
    match payload {
        Value::Object(map) => {
            let mut out = map.clone();
            out.remove("_actor");
            out.remove("_simulated");
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn pointer_string(value: &Value, path: &str) -> Option<String> {
    let v = value.pointer(path)?;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(event_kind: &str, fact_kind: &str) -> ProjectionRule {
        ProjectionRule {
            event_kind: event_kind.into(),
            fact_kind: fact_kind.into(),
            source_table: "invoices".into(),
            source_id_path: "/id".into(),
            happened_on_path: Some("/issued_on".into()),
            created_by_path: None,
            when: None,
        }
    }

    #[test]
    fn projects_invoice_created_to_invoice_issued() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let ts: DateTime<Utc> = "2026-04-01T12:00:00Z".parse().unwrap();
        let payload = serde_json::json!({
            "id": "inv-123",
            "issued_on": "2026-04-01",
            "amount_cents": 50000,
        });
        let projected = project_event(&r, ts, "commerce", &payload).unwrap();
        assert_eq!(projected.fact_kind, "finance.invoice.issued");
        assert_eq!(projected.source_id, "inv-123");
        assert_eq!(projected.happened_on.to_string(), "2026-04-01");
        assert_eq!(projected.created_by, "commerce");
        assert_eq!(projected.payload, payload);
    }

    #[test]
    fn payload_source_table_overrides_the_rule_fixed_value() {
        // The ledger movement endpoints (and the atomic opening-JE
        // writers) fold the caller\'s source_table into the payload so
        // rebuild reproduces the original provenance tag. Before
        // 2026-07-09 the rule\'s fixed value always won: a live
        // \'brewery_seed_opening_balance\' fact rebuilt as
        // \'manual_inventory_transferred\' — a different natural key and
        // deterministic fact id, i.e. a live-vs-replay divergence on
        // any never-rebuilt deployment.
        let r = rule(
            "ledger.inventory.transferred",
            "finance.inventory.transferred",
        );
        let ts: DateTime<Utc> = "2025-04-01T00:00:00Z".parse().unwrap();
        let payload = serde_json::json!({
            "id": "opening-raw-ING-MALT-2ROW-50",
            "issued_on": "2025-04-01",
            "source_table": "brewery_seed_opening_balance",
            "total_cost_cents": 490_000,
        });
        let projected = project_event(&r, ts, "inventory", &payload).unwrap();
        assert_eq!(projected.source_table, "brewery_seed_opening_balance");
        // Events that don\'t carry the key keep the rule\'s value.
        let bare = serde_json::json!({ "id": "x-1", "issued_on": "2025-04-01" });
        let projected = project_event(&r, ts, "inventory", &bare).unwrap();
        assert_eq!(projected.source_table, "invoices");
    }

    #[test]
    fn strips_envelope_keys_from_projected_payload() {
        // The publisher injects `_actor`/`_simulated` into every event
        // payload; the live in-tx fact never has them, so the projection
        // must drop them or the fact-level replay-check diverges.
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({
            "id": "inv-9",
            "issued_on": "2026-05-01",
            "amount_cents": 12345,
            "_actor": "sim:workforce",
            "_simulated": true,
        });
        let projected = project_event(&r, Utc::now(), "commerce", &payload).unwrap();
        assert_eq!(
            projected.payload,
            serde_json::json!({
                "id": "inv-9",
                "issued_on": "2026-05-01",
                "amount_cents": 12345,
            })
        );
        // source_id is still extracted from the (pre-strip) event payload.
        assert_eq!(projected.source_id, "inv-9");
    }

    #[test]
    fn strip_envelope_passes_through_clean_and_non_object_payloads() {
        assert_eq!(
            strip_envelope(&serde_json::json!({"a": 1})),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            strip_envelope(&serde_json::json!([1, 2])),
            serde_json::json!([1, 2])
        );
        assert_eq!(
            strip_envelope(&serde_json::json!("x")),
            serde_json::json!("x")
        );
    }

    #[test]
    fn missing_source_id_field_is_an_error() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"issued_on": "2026-04-01"});
        let result = project_event(&r, Utc::now(), "commerce", &payload);
        assert!(matches!(result, Err(ProjectionError::MissingField { .. })));
    }

    #[test]
    fn missing_happened_on_field_is_an_error() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"id": "inv-123"});
        let result = project_event(&r, Utc::now(), "commerce", &payload);
        assert!(matches!(result, Err(ProjectionError::MissingField { .. })));
    }

    #[test]
    fn null_happened_on_path_falls_back_to_event_timestamp() {
        let r = ProjectionRule {
            happened_on_path: None,
            ..rule("ledger.manual_entry.submitted", "finance.manual.entry")
        };
        let ts: DateTime<Utc> = "2026-04-01T15:30:00Z".parse().unwrap();
        let payload = serde_json::json!({"id": "ent-1"});
        let projected = project_event(&r, ts, "ledger", &payload).unwrap();
        assert_eq!(projected.happened_on.to_string(), "2026-04-01");
    }

    #[test]
    fn null_created_by_path_falls_back_to_event_source() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"id": "inv-1", "issued_on": "2026-01-01"});
        let projected = project_event(&r, Utc::now(), "commerce", &payload).unwrap();
        assert_eq!(projected.created_by, "commerce");
    }
}
