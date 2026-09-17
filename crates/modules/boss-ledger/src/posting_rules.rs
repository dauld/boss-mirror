//! Posting rules as registry data — the fact→lines half of the
//! data-driven ledger (backlog a40541cb, decided on design 18cf4272).
//!
//! WHY. `BossRuleSet` (rules.rs) is a Rust `match` on fact kind. The
//! event→fact half has been data since 40-ledger.sql
//! (`gl_fact_projection_rules`), but a tenant that needed one more
//! posting rule — Algedonic's sponsorship receipt, the first revenue
//! line of design ffc83387 — had to fork core, against CLAUDE.md §9.
//! This module is the missing half: a versioned `gl_posting_rules`
//! registry (fact_kind, version, lines), a [`DataRuleSet`] that
//! evaluates a fact by the newest registry rule for its kind and falls
//! back to the code rules otherwise, the `when` filter that lets ONE
//! workflow's step become a fact, and the TOML loaders `boss tenant
//! check` and `boss tenant publish` share (one definition, §9a).
//!
//! BALANCED FOR EVERY FACT, NOT FOR ONE. A code rule is proven balanced
//! by its tests; a data rule has none, so the registry admits a rule
//! only when the multiset of pointers on its debit side equals the
//! multiset on its credit side — then whatever integers the payload
//! carries, debits and credits sum the same. That is a narrower
//! language than the code rules speak (payroll's `gross = net +
//! withheld` is not in it) and deliberately so: a rule that cannot be
//! shown balanced at publish is refused there, not discovered
//! unbalanced at the first fact. The balanced-draft check every code
//! rule passes still runs at evaluation (`rules::evaluate`).
//!
//! VERSIONS. A data rule's `version` is the tenant's edition of ONE
//! fact kind's lines; the newest wins. It is not a `gl_rule_versions`
//! row: that table names the interpreter (the RuleSet that ran), and
//! [`DataRuleSet::version`] reports the wrapped code rules' version so
//! `gl_journal_entries.rule_version_id` keeps its meaning. Which
//! edition produced an entry is written into the entry's memo.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::LedgerError;
use crate::rules::{BossRuleSet, RuleSet, cents_from_payload};
use crate::types::{FactRef, JournalEntryDraft, JournalLineDraft};

/// The fact a tenant's declaration leaves: one per posting rule the
/// batch INSERTED. Never per kept row, never per batch (the shape of
/// migration 20260917071313).
pub const POSTING_RULE_DECLARED: &str = "ledger.posting_rule.declared";
/// One per event→fact projection rule the batch INSERTED.
pub const FACT_PROJECTION_RULE_DECLARED: &str = "ledger.fact_projection_rule.declared";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Debit,
    Credit,
}

/// One journal line a data rule emits. `amount_path` is an RFC 6901
/// pointer into the fact payload resolving to integer cents; `memo`
/// may carry `{/pointer}` placeholders substituted from the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleLine {
    pub account_code: String,
    pub side: Side,
    pub amount_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// A posting rule as a tenant declares it (`[[posting_rule]]` in
/// `seeds/posting_rules.toml`) and as the batch door takes it.
/// `version` defaults to 1: a tenant that changes a rule publishes the
/// next version, never edits the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingRuleInput {
    pub fact_kind: String,
    #[serde(default = "version_one")]
    pub version: i32,
    pub lines: Vec<RuleLine>,
    #[serde(default = "accrual")]
    pub basis: String,
}

fn version_one() -> i32 {
    1
}

fn accrual() -> String {
    "accrual".to_string()
}

/// One registry row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingRule {
    pub fact_kind: String,
    pub version: i32,
    pub lines: Vec<RuleLine>,
    pub basis: String,
    /// `None` for a platform row, `tenant:<id>` for a published one.
    pub source: Option<String>,
}

/// One event→fact projection rule — a row of `gl_fact_projection_rules`
/// — as the tenant declares it (`[[projection]]`), as the batch takes
/// it and as the rebuild reads it back. `when` is a JSON object of
/// `{"<pointer>": <expected value>}`: every pointer must resolve to
/// exactly that value for the rule to fire; absent, it fires on every
/// event of the kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionRule {
    pub event_kind: String,
    pub fact_kind: String,
    pub source_table: String,
    pub source_id_path: String,
    #[serde(default)]
    pub happened_on_path: Option<String>,
    #[serde(default)]
    pub created_by_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Value>,
}

impl ProjectionRule {
    /// Does this rule fire on `payload`? A rule without `when` always
    /// does; with one, every pointer must resolve to its expected
    /// value (JSON equality — `"1"` and `1` differ).
    pub fn when_matches(&self, payload: &Value) -> bool {
        match self.when.as_ref().and_then(Value::as_object) {
            None => true,
            Some(conds) => conds
                .iter()
                .all(|(pointer, expected)| payload.pointer(pointer) == Some(expected)),
        }
    }
}

/// What a batch did: how many rows it received, how many it inserted,
/// and the kept rows whose declaration DIFFERS from the registry's —
/// named, so a tenant that edited a rule without bumping its version
/// reads why nothing changed instead of a silent count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchOutcome {
    pub received: usize,
    pub inserted: usize,
    #[serde(default)]
    pub differs: Vec<String>,
}

fn is_pointer(p: &str) -> bool {
    p.starts_with('/')
}

/// Why a posting rule is refused. Runs in `boss tenant check`, at the
/// batch door and (the balance half) again at evaluation.
pub fn validate_posting_rule(r: &PostingRuleInput) -> Result<(), String> {
    let name = format!("posting rule {} v{}", r.fact_kind, r.version);
    if r.fact_kind.is_empty() {
        return Err("a posting rule needs a fact_kind (e.g. finance.sponsorship.received)".into());
    }
    if !r
        .fact_kind
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        || !r.fact_kind.contains('.')
    {
        return Err(format!(
            "{name}: fact_kind is not a dotted lowercase kind (e.g. finance.sponsorship.received)"
        ));
    }
    if r.version < 1 {
        return Err(format!("{name}: version must be >= 1"));
    }
    if !matches!(r.basis.as_str(), "cash" | "accrual") {
        return Err(format!(
            "{name}: basis `{}` is neither cash nor accrual",
            r.basis
        ));
    }
    if r.lines.len() < 2 {
        return Err(format!("{name}: a rule needs at least two lines"));
    }
    for (i, l) in r.lines.iter().enumerate() {
        if l.account_code.trim().is_empty() {
            return Err(format!("{name}: lines[{i}].account_code is required"));
        }
        if !is_pointer(&l.amount_path) {
            return Err(format!(
                "{name}: lines[{i}].amount_path `{}` is not a JSON pointer (starts with /)",
                l.amount_path
            ));
        }
    }
    let mut debits: BTreeMap<&str, usize> = BTreeMap::new();
    let mut credits: BTreeMap<&str, usize> = BTreeMap::new();
    for l in &r.lines {
        let side = match l.side {
            Side::Debit => &mut debits,
            Side::Credit => &mut credits,
        };
        *side.entry(l.amount_path.as_str()).or_insert(0) += 1;
    }
    if debits != credits {
        return Err(format!(
            "{name} is not balanced for every fact: debit pointers {:?} vs credit pointers {:?} — \
             each amount pointer must appear as often on both sides",
            expand(&debits),
            expand(&credits)
        ));
    }
    Ok(())
}

fn expand(side: &BTreeMap<&str, usize>) -> Vec<String> {
    side.iter()
        .flat_map(|(p, n)| std::iter::repeat_n((*p).to_string(), *n))
        .collect()
}

/// Why a projection rule is refused.
pub fn validate_projection_rule(r: &ProjectionRule) -> Result<(), String> {
    let name = format!("projection {} -> {}", r.event_kind, r.fact_kind);
    if r.event_kind.trim().is_empty() {
        return Err("a projection needs an event_kind (an audit_log kind)".into());
    }
    if r.fact_kind.trim().is_empty() {
        return Err(format!("{name}: fact_kind is required"));
    }
    if r.source_table.trim().is_empty() {
        return Err(format!("{name}: source_table is required"));
    }
    if !is_pointer(&r.source_id_path) {
        return Err(format!(
            "{name}: source_id_path `{}` is not a JSON pointer",
            r.source_id_path
        ));
    }
    for (field, v) in [
        ("happened_on_path", &r.happened_on_path),
        ("created_by_path", &r.created_by_path),
    ] {
        if let Some(p) = v
            && !is_pointer(p)
        {
            return Err(format!("{name}: {field} `{p}` is not a JSON pointer"));
        }
    }
    if let Some(when) = &r.when {
        let Some(conds) = when.as_object() else {
            return Err(format!(
                "{name}: when must be a table of {{\"/pointer\" = value}}"
            ));
        };
        if conds.is_empty() {
            return Err(format!(
                "{name}: when is empty — omit it to fire on every event"
            ));
        }
        for (pointer, expected) in conds {
            if !is_pointer(pointer) {
                return Err(format!(
                    "{name}: when key `{pointer}` is not a JSON pointer"
                ));
            }
            if !(expected.is_string() || expected.is_number() || expected.is_boolean()) {
                return Err(format!(
                    "{name}: when value for `{pointer}` must be a string, number or bool"
                ));
            }
        }
    }
    Ok(())
}

/// Evaluate one fact under one data rule. Pure: same rule + same fact
/// → same draft. A zero amount omits its line (the journal's CHECK
/// refuses a zero line; a sponsorship with no fee has no fee legs),
/// which keeps the entry balanced because the same pointer is omitted
/// on both sides.
pub fn evaluate_data_rule(
    rule: &PostingRule,
    fact: &FactRef<'_>,
) -> Result<JournalEntryDraft, LedgerError> {
    let err = |reason: String| LedgerError::InvalidPayload {
        kind: fact.kind.to_string(),
        reason,
    };
    let mut lines = Vec::with_capacity(rule.lines.len());
    for l in &rule.lines {
        let amount = cents_from_payload(fact.payload.pointer(&l.amount_path)).ok_or_else(|| {
            err(format!(
                "posting rule v{}: amount at {} missing or not integer cents",
                rule.version, l.amount_path
            ))
        })?;
        if amount < 0 {
            return Err(err(format!(
                "posting rule v{}: amount at {} is negative ({amount})",
                rule.version, l.amount_path
            )));
        }
        if amount == 0 {
            continue;
        }
        let memo = l.memo.as_deref().map(|t| render_memo(t, fact.payload));
        let sort_order = lines.len() as i16;
        lines.push(match l.side {
            Side::Debit => JournalLineDraft {
                memo,
                ..JournalLineDraft::debit(l.account_code.clone(), amount, sort_order)
            },
            Side::Credit => JournalLineDraft {
                memo,
                ..JournalLineDraft::credit(l.account_code.clone(), amount, sort_order)
            },
        });
    }
    if lines.is_empty() {
        return Err(err(format!(
            "posting rule v{}: every amount is zero — nothing to post",
            rule.version
        )));
    }
    Ok(JournalEntryDraft {
        posted_on: fact.happened_on,
        memo: Some(format!(
            "{} — posting rule v{}",
            rule.fact_kind, rule.version
        )),
        lines,
    })
}

/// Substitute `{/pointer}` placeholders with the payload's value at
/// that pointer (scalars only; an unresolved placeholder stays as
/// written so the memo says what it could not find).
fn render_memo(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{/") {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let pointer = &rest[start + 1..start + len];
        match payload.pointer(pointer) {
            Some(Value::String(s)) => out.push_str(s),
            Some(Value::Number(n)) => out.push_str(&n.to_string()),
            Some(Value::Bool(b)) => out.push_str(&b.to_string()),
            _ => out.push_str(&rest[start..=start + len]),
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// The active RuleSet: the registry's newest rule per fact kind when
/// one exists, the code rules (`BossRuleSet`) otherwise. Built per
/// evaluation from the rows loaded for the fact's kind, so it holds
/// no connection and stays pure.
pub struct DataRuleSet {
    rules: Vec<PostingRule>,
}

impl DataRuleSet {
    pub fn new(rules: Vec<PostingRule>) -> Self {
        Self { rules }
    }

    /// The newest registry rule for `kind`, if any.
    pub fn rule_for(&self, kind: &str) -> Option<&PostingRule> {
        self.rules
            .iter()
            .filter(|r| r.fact_kind == kind)
            .max_by_key(|r| r.version)
    }
}

impl RuleSet for DataRuleSet {
    /// The interpreter's version — the wrapped code rules' — so an
    /// entry's `rule_version_id` keeps naming the RuleSet that ran.
    fn version(&self) -> i32 {
        BossRuleSet.version()
    }

    fn evaluate(&self, fact: &FactRef<'_>) -> Result<JournalEntryDraft, LedgerError> {
        match self.rule_for(fact.kind) {
            Some(rule) => evaluate_data_rule(rule, fact),
            None => BossRuleSet.evaluate(fact),
        }
    }
}

// --- The tenant's files -----------------------------------------------

#[derive(Debug, Deserialize)]
struct PostingRulesFile {
    #[serde(default)]
    posting_rule: Vec<PostingRuleInput>,
}

/// Parse `seeds/posting_rules.toml`: `[[posting_rule]]` rows, each
/// validated, no (fact_kind, version) twice.
pub fn parse_posting_rules_toml(text: &str) -> Result<Vec<PostingRuleInput>, String> {
    let file: PostingRulesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut seen = std::collections::BTreeSet::new();
    for r in &file.posting_rule {
        validate_posting_rule(r)?;
        if !seen.insert((r.fact_kind.as_str(), r.version)) {
            return Err(format!(
                "posting rule {} v{} is declared twice",
                r.fact_kind, r.version
            ));
        }
    }
    Ok(file.posting_rule)
}

pub fn load_posting_rules_toml(path: &std::path::Path) -> Result<Vec<PostingRuleInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_posting_rules_toml(&text)
}

#[derive(Debug, Deserialize)]
struct ProjectionsFile {
    #[serde(default)]
    projection: Vec<ProjectionRule>,
}

/// Parse `seeds/fact_projection_rules.toml`: `[[projection]]` rows,
/// each validated, no (event_kind, when) twice.
pub fn parse_projection_rules_toml(text: &str) -> Result<Vec<ProjectionRule>, String> {
    let file: ProjectionsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut seen = std::collections::BTreeSet::new();
    for r in &file.projection {
        validate_projection_rule(r)?;
        let key = format!(
            "{} {}",
            r.event_kind,
            r.when.as_ref().map(Value::to_string).unwrap_or_default()
        );
        if !seen.insert(key) {
            return Err(format!(
                "projection {} (same when) is declared twice",
                r.event_kind
            ));
        }
    }
    Ok(file.projection)
}

pub fn load_projection_rules_toml(path: &std::path::Path) -> Result<Vec<ProjectionRule>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_projection_rules_toml(&text)
}

// --- Postgres -----------------------------------------------------------

#[cfg(feature = "postgres")]
pub use pg::*;

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use boss_core::publisher::EventStamp;
    use sqlx::{PgPool, Postgres, Row, Transaction};

    fn parse_lines(v: Value) -> Result<Vec<RuleLine>, LedgerError> {
        serde_json::from_value(v)
            .map_err(|e| LedgerError::Storage(format!("gl_posting_rules.lines: {e}")))
    }

    /// The newest registry rule for `kind`, or `None` — the one read
    /// the posting path makes per fact (primary-key range, one row).
    pub async fn load_newest_rule_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        kind: &str,
    ) -> Result<Option<PostingRule>, LedgerError> {
        let row = sqlx::query(
            "SELECT fact_kind, version, lines, basis, source FROM gl_posting_rules \
             WHERE fact_kind = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(kind)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        row.map(|r| {
            Ok(PostingRule {
                fact_kind: r.get("fact_kind"),
                version: r.get("version"),
                lines: parse_lines(r.get("lines"))?,
                basis: r.get("basis"),
                source: r.get("source"),
            })
        })
        .transpose()
    }

    /// Every registry row, newest first within a kind.
    pub async fn list_posting_rules(pool: &PgPool) -> Result<Vec<PostingRule>, LedgerError> {
        let rows = sqlx::query(
            "SELECT fact_kind, version, lines, basis, source FROM gl_posting_rules \
             ORDER BY fact_kind, version DESC",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| {
                Ok(PostingRule {
                    fact_kind: r.get("fact_kind"),
                    version: r.get("version"),
                    lines: parse_lines(r.get("lines"))?,
                    basis: r.get("basis"),
                    source: r.get("source"),
                })
            })
            .collect()
    }

    /// Land a tenant's posting rules: insert-if-absent by (fact_kind,
    /// version), one [`POSTING_RULE_DECLARED`] per inserted row on the
    /// outbox in the same transaction. Rows arrive validated. A kept
    /// row whose lines or basis differ is named in the outcome.
    pub async fn publish_posting_rules_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        stamp: &EventStamp,
        source: Option<&str>,
        rows: &[PostingRuleInput],
    ) -> Result<BatchOutcome, LedgerError> {
        let mut inserted = 0;
        let mut differs = Vec::new();
        for r in rows {
            let lines =
                serde_json::to_value(&r.lines).map_err(|e| LedgerError::Storage(e.to_string()))?;
            let landed = sqlx::query(
                "INSERT INTO gl_posting_rules (fact_kind, version, lines, basis, source) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (fact_kind, version) DO NOTHING",
            )
            .bind(&r.fact_kind)
            .bind(r.version)
            .bind(&lines)
            .bind(&r.basis)
            .bind(source)
            .execute(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?
            .rows_affected();
            if landed == 1 {
                let mut payload = serde_json::json!({
                    "fact_kind": r.fact_kind,
                    "version": r.version,
                    "lines": lines,
                    "basis": r.basis,
                    "source": source,
                });
                payload["declared_by"] = Value::String(stamp.actor().to_string());
                boss_events::outbox::record_event_in_tx(
                    tx,
                    &stamp.event(POSTING_RULE_DECLARED, payload),
                )
                .await
                .map_err(LedgerError::Storage)?;
                inserted += 1;
                continue;
            }
            let existing = sqlx::query(
                "SELECT lines, basis FROM gl_posting_rules WHERE fact_kind = $1 AND version = $2",
            )
            .bind(&r.fact_kind)
            .bind(r.version)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            let (kept_lines, kept_basis): (Value, String) =
                (existing.get("lines"), existing.get("basis"));
            if kept_lines != lines || kept_basis != r.basis {
                differs.push(format!(
                    "{} v{}: the registry's row differs (lines or basis) — publish version {}",
                    r.fact_kind,
                    r.version,
                    r.version + 1
                ));
            }
        }
        Ok(BatchOutcome {
            received: rows.len(),
            inserted,
            differs,
        })
    }

    fn projection_from_row(r: &sqlx::postgres::PgRow) -> ProjectionRule {
        ProjectionRule {
            event_kind: r.get("event_kind"),
            fact_kind: r.get("fact_kind"),
            source_table: r.get("source_table"),
            source_id_path: r.get("source_id_path"),
            happened_on_path: r.get("happened_on_path"),
            created_by_path: r.get("created_by_path"),
            when: r.get("when_filter"),
        }
    }

    /// Every projection rule, as the rebuild reads them.
    pub async fn load_projection_rules_in_tx(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<ProjectionRule>, LedgerError> {
        let rows = sqlx::query(
            "SELECT event_kind, fact_kind, source_table, source_id_path, \
                    happened_on_path, created_by_path, when_filter \
             FROM gl_fact_projection_rules ORDER BY event_kind",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        Ok(rows.iter().map(projection_from_row).collect())
    }

    pub async fn list_projection_rules(pool: &PgPool) -> Result<Vec<ProjectionRule>, LedgerError> {
        let rows = sqlx::query(
            "SELECT event_kind, fact_kind, source_table, source_id_path, \
                    happened_on_path, created_by_path, when_filter \
             FROM gl_fact_projection_rules ORDER BY event_kind",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        Ok(rows.iter().map(projection_from_row).collect())
    }

    /// Land projection rules: insert-if-absent by (event_kind, when),
    /// one [`FACT_PROJECTION_RULE_DECLARED`] per inserted row, same
    /// transaction. A kept row whose target differs is named.
    pub async fn publish_projection_rules_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        stamp: &EventStamp,
        rows: &[ProjectionRule],
    ) -> Result<BatchOutcome, LedgerError> {
        let mut inserted = 0;
        let mut differs = Vec::new();
        for r in rows {
            let landed = sqlx::query(
                "INSERT INTO gl_fact_projection_rules \
                    (event_kind, fact_kind, source_table, source_id_path, \
                     happened_on_path, created_by_path, when_filter) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&r.event_kind)
            .bind(&r.fact_kind)
            .bind(&r.source_table)
            .bind(&r.source_id_path)
            .bind(&r.happened_on_path)
            .bind(&r.created_by_path)
            .bind(&r.when)
            .execute(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?
            .rows_affected();
            if landed == 1 {
                let mut payload =
                    serde_json::to_value(r).map_err(|e| LedgerError::Storage(e.to_string()))?;
                payload["declared_by"] = Value::String(stamp.actor().to_string());
                boss_events::outbox::record_event_in_tx(
                    tx,
                    &stamp.event(FACT_PROJECTION_RULE_DECLARED, payload),
                )
                .await
                .map_err(LedgerError::Storage)?;
                inserted += 1;
                continue;
            }
            let kept = sqlx::query(
                "SELECT event_kind, fact_kind, source_table, source_id_path, \
                        happened_on_path, created_by_path, when_filter \
                 FROM gl_fact_projection_rules \
                 WHERE event_kind = $1 \
                   AND COALESCE(when_filter, '{}'::jsonb) = COALESCE($2, '{}'::jsonb)",
            )
            .bind(&r.event_kind)
            .bind(&r.when)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            if &projection_from_row(&kept) != r {
                differs.push(format!(
                    "{} (when {}): the registry's row differs — a projection is replaced by retiring it, not by re-publishing",
                    r.event_kind,
                    r.when.as_ref().map(Value::to_string).unwrap_or_else(|| "always".into())
                ));
            }
        }
        Ok(BatchOutcome {
            received: rows.len(),
            inserted,
            differs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::evaluate;

    fn fact<'a>(kind: &'a str, payload: &'a Value) -> FactRef<'a> {
        FactRef {
            id: uuid::Uuid::new_v4(),
            kind,
            happened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            payload,
        }
    }

    /// Algedonic's sponsorship rule, as the packet states it:
    /// DR 1010 gross / CR 4100 gross, DR 6100 fee / CR 1010 fee.
    const SPONSORSHIP: &str = r#"
[[posting_rule]]
fact_kind = "finance.sponsorship.received"
basis = "cash"
lines = [
  { account_code = "1010", side = "debit",  amount_path = "/amount_cents", memo = "Sponsorship {/charge_id}" },
  { account_code = "4100", side = "credit", amount_path = "/amount_cents" },
  { account_code = "6100", side = "debit",  amount_path = "/fee_cents", memo = "Stripe fee" },
  { account_code = "1010", side = "credit", amount_path = "/fee_cents" },
]
"#;

    fn sponsorship_rule() -> PostingRule {
        let input = parse_posting_rules_toml(SPONSORSHIP).unwrap().remove(0);
        PostingRule {
            fact_kind: input.fact_kind,
            version: input.version,
            lines: input.lines,
            basis: input.basis,
            source: Some("tenant:algedonic".into()),
        }
    }

    #[test]
    fn the_data_rule_posts_the_sponsorship_fact() {
        let rules = DataRuleSet::new(vec![sponsorship_rule()]);
        let payload =
            serde_json::json!({"amount_cents": 100, "fee_cents": 33, "charge_id": "ch_1"});
        let draft = evaluate(&rules, &fact("finance.sponsorship.received", &payload)).unwrap();
        let lines: Vec<(&str, i64, i64)> = draft
            .lines
            .iter()
            .map(|l| (l.account_code.as_ref(), l.debit_cents, l.credit_cents))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("1010", 100, 0),
                ("4100", 0, 100),
                ("6100", 33, 0),
                ("1010", 0, 33)
            ]
        );
        assert!(draft.is_balanced());
        assert_eq!(draft.lines[0].memo.as_deref(), Some("Sponsorship ch_1"));
        assert_eq!(
            draft.memo.as_deref(),
            Some("finance.sponsorship.received — posting rule v1")
        );
        assert_eq!(draft.posted_on.to_string(), "2026-09-17");
    }

    #[test]
    fn a_zero_amount_omits_its_lines_on_both_sides() {
        let rules = DataRuleSet::new(vec![sponsorship_rule()]);
        let payload = serde_json::json!({"amount_cents": "2550", "fee_cents": 0});
        let draft = evaluate(&rules, &fact("finance.sponsorship.received", &payload)).unwrap();
        assert_eq!(draft.lines.len(), 2);
        assert_eq!(draft.total_debits(), 2550);
        assert!(draft.is_balanced());
    }

    #[test]
    fn a_kind_with_no_data_rule_falls_back_to_the_code_rules() {
        let rules = DataRuleSet::new(vec![sponsorship_rule()]);
        let payload = serde_json::json!({"invoice_id": "inv-1", "amount_cents": 500});
        let draft = evaluate(&rules, &fact("finance.invoice.paid", &payload)).unwrap();
        assert_eq!(draft.lines[0].account_code.as_ref(), "1000");
        assert_eq!(draft.lines[1].account_code.as_ref(), "1100");
        assert_eq!(rules.version(), BossRuleSet.version());
        assert!(matches!(
            evaluate(&rules, &fact("finance.nobody.knows", &payload)),
            Err(LedgerError::UnknownFactKind(_))
        ));
    }

    #[test]
    fn the_newest_version_of_a_kind_wins() {
        let v1 = sponsorship_rule();
        let mut v2 = sponsorship_rule();
        v2.version = 2;
        v2.lines[1].account_code = "4110".into();
        let rules = DataRuleSet::new(vec![v1, v2]);
        let payload = serde_json::json!({"amount_cents": 100, "fee_cents": 0});
        let draft = evaluate(&rules, &fact("finance.sponsorship.received", &payload)).unwrap();
        assert_eq!(draft.lines[1].account_code.as_ref(), "4110");
        assert_eq!(
            draft.memo.as_deref().unwrap(),
            "finance.sponsorship.received — posting rule v2"
        );
    }

    #[test]
    fn an_unbalanced_rule_is_refused_at_publish_by_name() {
        let text = SPONSORSHIP.replace(
            r#"{ account_code = "1010", side = "credit", amount_path = "/fee_cents" },"#,
            "",
        );
        let why = parse_posting_rules_toml(&text).unwrap_err();
        assert!(why.contains("finance.sponsorship.received v1"), "{why}");
        assert!(why.contains("not balanced for every fact"), "{why}");
        assert!(why.contains("/fee_cents"), "{why}");
    }

    #[test]
    fn an_unbalanced_draft_is_refused_at_evaluation_too() {
        // The registry check is the front door; the evaluation check is
        // the lock on the back one — a row that reached the table by
        // any other path is still refused.
        let mut rule = sponsorship_rule();
        rule.lines.pop();
        let payload = serde_json::json!({"amount_cents": 100, "fee_cents": 33});
        let rules = DataRuleSet::new(vec![rule]);
        match evaluate(&rules, &fact("finance.sponsorship.received", &payload)) {
            Err(LedgerError::Unbalanced {
                debits, credits, ..
            }) => {
                assert_eq!((debits, credits), (133, 100));
            }
            other => panic!("expected Unbalanced, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_or_negative_amount_is_refused_naming_the_pointer() {
        let rules = DataRuleSet::new(vec![sponsorship_rule()]);
        let payload = serde_json::json!({"amount_cents": 100});
        let why = evaluate(&rules, &fact("finance.sponsorship.received", &payload))
            .unwrap_err()
            .to_string();
        assert!(why.contains("/fee_cents"), "{why}");
        let payload = serde_json::json!({"amount_cents": -5, "fee_cents": 0});
        let why = evaluate(&rules, &fact("finance.sponsorship.received", &payload))
            .unwrap_err()
            .to_string();
        assert!(why.contains("negative"), "{why}");
    }

    #[test]
    fn the_file_is_validated_row_by_row() {
        let rows = parse_posting_rules_toml(SPONSORSHIP).unwrap();
        assert_eq!(rows[0].version, 1);
        assert_eq!(rows[0].basis, "cash");
        assert!(parse_posting_rules_toml("").unwrap().is_empty());
        let twice = format!("{SPONSORSHIP}\n{SPONSORSHIP}");
        assert!(
            parse_posting_rules_toml(&twice)
                .unwrap_err()
                .contains("twice")
        );
        let bad = SPONSORSHIP.replace("basis = \"cash\"", "basis = \"modified-cash\"");
        assert!(
            parse_posting_rules_toml(&bad)
                .unwrap_err()
                .contains("basis")
        );
        let bad = SPONSORSHIP.replace(
            "amount_path = \"/fee_cents\"",
            "amount_path = \"fee_cents\"",
        );
        assert!(
            parse_posting_rules_toml(&bad)
                .unwrap_err()
                .contains("JSON pointer")
        );
        let bad = SPONSORSHIP.replace(
            "fact_kind = \"finance.sponsorship.received\"",
            "fact_kind = \"Sponsorship\"",
        );
        assert!(
            parse_posting_rules_toml(&bad)
                .unwrap_err()
                .contains("dotted")
        );
    }

    const PROJECTION: &str = r#"
[[projection]]
event_kind = "step.done.task"
when = { "/workflow_kind" = "receive-a-sponsorship", "/spec_slug" = "recognize" }
fact_kind = "finance.sponsorship.received"
source_table = "jobs"
source_id_path = "/job_id"
happened_on_path = "/completed_on"
"#;

    #[test]
    fn when_fires_only_on_the_event_whose_pointers_match() {
        let rule = parse_projection_rules_toml(PROJECTION).unwrap().remove(0);
        assert!(rule.when_matches(&serde_json::json!({
            "workflow_kind": "receive-a-sponsorship", "spec_slug": "recognize", "job_id": "j1"
        })));
        // A different workflow's recognize step, or the same workflow's
        // other steps, do not.
        assert!(!rule.when_matches(&serde_json::json!({
            "workflow_kind": "close-the-month", "spec_slug": "recognize"
        })));
        assert!(!rule.when_matches(&serde_json::json!({
            "workflow_kind": "receive-a-sponsorship", "spec_slug": "reconcile"
        })));
        assert!(!rule.when_matches(&serde_json::json!({"spec_slug": "recognize"})));
        // JSON equality, not string coercion.
        let n = ProjectionRule {
            when: Some(serde_json::json!({"/n": 1})),
            ..rule.clone()
        };
        assert!(n.when_matches(&serde_json::json!({"n": 1})));
        assert!(!n.when_matches(&serde_json::json!({"n": "1"})));
        // No `when` fires on everything.
        let always = ProjectionRule { when: None, ..rule };
        assert!(always.when_matches(&serde_json::json!({})));
    }

    #[test]
    fn a_projection_file_is_validated_row_by_row() {
        let rows = parse_projection_rules_toml(PROJECTION).unwrap();
        assert_eq!(rows[0].fact_kind, "finance.sponsorship.received");
        assert_eq!(rows[0].happened_on_path.as_deref(), Some("/completed_on"));
        let bad = PROJECTION.replace(
            "source_id_path = \"/job_id\"",
            "source_id_path = \"job_id\"",
        );
        assert!(
            parse_projection_rules_toml(&bad)
                .unwrap_err()
                .contains("JSON pointer")
        );
        let bad = PROJECTION.replace("\"/spec_slug\"", "\"spec_slug\"");
        assert!(
            parse_projection_rules_toml(&bad)
                .unwrap_err()
                .contains("when key")
        );
        let bad = PROJECTION.replace("when = { \"/workflow_kind\" = \"receive-a-sponsorship\", \"/spec_slug\" = \"recognize\" }", "when = {}");
        assert!(
            parse_projection_rules_toml(&bad)
                .unwrap_err()
                .contains("empty")
        );
        let twice = format!("{PROJECTION}\n{PROJECTION}");
        assert!(
            parse_projection_rules_toml(&twice)
                .unwrap_err()
                .contains("twice")
        );
    }

    #[test]
    fn memo_placeholders_read_the_payload() {
        let p = serde_json::json!({"charge_id": "ch_9", "n": 3, "ok": true, "o": {}});
        assert_eq!(
            render_memo("Sponsorship {/charge_id} x{/n} {/ok}", &p),
            "Sponsorship ch_9 x3 true"
        );
        assert_eq!(
            render_memo("{/missing} and {/o}", &p),
            "{/missing} and {/o}"
        );
        assert_eq!(render_memo("plain", &p), "plain");
        assert_eq!(render_memo("open {/x", &p), "open {/x");
    }
}
