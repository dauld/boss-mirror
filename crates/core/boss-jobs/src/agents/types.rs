//! The agents registry's shapes: a declaration as a tenant writes it
//! (`[[agent]]` in `seeds/agents.toml`) and as the batch door takes it,
//! a row as the registry holds it, and what a batch did.
//!
//! WHY A DECLARATION (backlog f56155f0, 2026-09-17). The one file the
//! tenant contract could not name was `seeds/agents.toml`: the real
//! tenant declared its agent there and nothing read it, so the
//! product's own agent was registered by a migration and the tenant's
//! declaration was prose. The shape below is the `agents` table's
//! columns plus the `actor_aliases` the row owns — and NOTHING the
//! table cannot hold: `deny_unknown_fields`, so a field the registry
//! would silently drop (the real tenant's first draft carried `role`
//! and `department`) is refused by name at `boss tenant check` rather
//! than believed.

use boss_core::actor::REGISTERED_AGENT_PREFIX;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One agent as a tenant declares it and as `POST /api/agents/batch`
/// takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    /// `agent-<slug>` — the SQL CHECK on `agents.id` spells the same
    /// prefix as `boss_core::actor::REGISTERED_AGENT_PREFIX`.
    pub id: String,
    pub display_name: String,
    /// The model a run uses when it does not say. Spelled as the rate
    /// card spells it (`opus-5[1m]`, no `claude-` prefix): the table's
    /// FK refuses a default that cannot be priced.
    pub default_model: String,
    /// The logins that sign as this agent (`actor_aliases`).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Caps, `None` while nothing reads them (budgets are 7dd9f28c).
    #[serde(default)]
    pub hourly_budget_usd_micros: Option<i64>,
    #[serde(default)]
    pub max_concurrent_runs: Option<i32>,
}

/// Why a declaration is refused. The same check runs in `boss tenant
/// check`, the batch door and the in-memory adapter, so the refusal
/// names the same row everywhere.
pub fn validate_agent(a: &AgentInput) -> Result<(), String> {
    if a.id.is_empty() {
        return Err("an agent needs an id (e.g. agent-claude)".into());
    }
    let slug = a.id.strip_prefix(REGISTERED_AGENT_PREFIX).unwrap_or("");
    let slug_ok = slug
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !slug_ok {
        return Err(format!(
            "agent {}: id must be {REGISTERED_AGENT_PREFIX}<slug> (lowercase, digits, hyphens) — \
             a login address or a model-qualified spelling is not an identity (design 6fda05ae)",
            a.id
        ));
    }
    if a.display_name.trim().is_empty() {
        return Err(format!("agent {}: display_name is required", a.id));
    }
    if a.default_model.trim().is_empty() {
        return Err(format!(
            "agent {}: default_model is required (a rate-card model, e.g. opus-5[1m])",
            a.id
        ));
    }
    if let Some(alias) = a.aliases.iter().find(|s| s.trim().is_empty()) {
        return Err(format!("agent {}: an alias is empty ({alias:?})", a.id));
    }
    if a.hourly_budget_usd_micros.is_some_and(|n| n < 0) {
        return Err(format!(
            "agent {}: hourly_budget_usd_micros must be >= 0",
            a.id
        ));
    }
    if a.max_concurrent_runs.is_some_and(|n| n < 0) {
        return Err(format!("agent {}: max_concurrent_runs must be >= 0", a.id));
    }
    Ok(())
}

/// One registry row, as `GET /api/agents` reads it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: String,
    pub display_name: String,
    pub default_model: String,
    pub hourly_budget_usd_micros: Option<i64>,
    pub max_concurrent_runs: Option<i32>,
    /// Every login that resolves to this id, sorted.
    pub aliases: Vec<String>,
}

/// One field a declaration changed on a row the registry already
/// held: the column, what it read, what it reads now. `from`/`to` are
/// JSON so a string, a number, a null and a list render the same way
/// everywhere ([`FieldChange::render`]), and so the fact recording the
/// change carries the values, not a rendering of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

/// A value on a publish line: a string bare, everything else as JSON.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl FieldChange {
    pub fn new(field: &str, from: impl Serialize, to: impl Serialize) -> Self {
        Self {
            field: field.to_string(),
            from: serde_json::to_value(from).unwrap_or(Value::Null),
            to: serde_json::to_value(to).unwrap_or(Value::Null),
        }
    }

    /// `location loc-hq → loc-algedonic-hq`.
    pub fn render(&self) -> String {
        format!("{} {} → {}", self.field, show(&self.from), show(&self.to))
    }
}

/// A row a publish UPDATED to the tenant's declaration (backlog
/// 09887242, 2026-09-17): the id and every field that changed. The
/// shape is the agents batch's answer AND the shape `boss tenant
/// publish` names an updated employee by — one rule, one rendering,
/// for both halves of the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatedRow {
    pub id: String,
    pub changes: Vec<FieldChange>,
}

impl UpdatedRow {
    /// `emp-david (location loc-hq → loc-algedonic-hq)`.
    pub fn render(&self) -> String {
        format!(
            "{} ({})",
            self.id,
            self.changes
                .iter()
                .map(FieldChange::render)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// `updated 2: emp-david (location loc-hq → loc-algedonic-hq); emp-two
/// (department it → ops)` — the publish line's tail for the rows a
/// declaration changed. Empty when nothing was.
pub fn render_updated(rows: &[UpdatedRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "updated {}: {}",
        rows.len(),
        rows.iter()
            .map(UpdatedRow::render)
            .collect::<Vec<_>>()
            .join("; ")
    )
}

impl AgentRow {
    /// Every field where `after` reads differently from this row —
    /// what a publish that applied a declaration over it changed. Both
    /// alias lists are sorted, so they compare as sets.
    pub fn changes_to(&self, after: &AgentRow) -> Vec<FieldChange> {
        let mut out = Vec::new();
        if self.display_name != after.display_name {
            out.push(FieldChange::new(
                "display_name",
                &self.display_name,
                &after.display_name,
            ));
        }
        if self.default_model != after.default_model {
            out.push(FieldChange::new(
                "default_model",
                &self.default_model,
                &after.default_model,
            ));
        }
        if self.hourly_budget_usd_micros != after.hourly_budget_usd_micros {
            out.push(FieldChange::new(
                "hourly_budget_usd_micros",
                self.hourly_budget_usd_micros,
                after.hourly_budget_usd_micros,
            ));
        }
        if self.max_concurrent_runs != after.max_concurrent_runs {
            out.push(FieldChange::new(
                "max_concurrent_runs",
                self.max_concurrent_runs,
                after.max_concurrent_runs,
            ));
        }
        if self.aliases != after.aliases {
            out.push(FieldChange::new("aliases", &self.aliases, &after.aliases));
        }
        out
    }
}

/// What a batch did: rows received, rows inserted, rows the registry
/// already held that were UPDATED to the declaration (each change
/// named), and rows already registered exactly as declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub updated: Vec<UpdatedRow>,
    pub unchanged: usize,
}

impl AgentsBatchOutcome {
    /// One line for a publish report: counts, then each updated row
    /// with its changes — so a declaration that moved the registry is
    /// read in the plan line field by field, never as a bare count.
    pub fn summary(&self) -> String {
        let mut s = format!("received {}, inserted {}", self.received, self.inserted);
        if !self.updated.is_empty() {
            s.push_str(", ");
            s.push_str(&render_updated(&self.updated));
        }
        if self.unchanged > 0 {
            s.push_str(&format!(", {} already as declared", self.unchanged));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AgentInput {
        AgentInput {
            id: "agent-claude".into(),
            display_name: "Claude (engineering)".into(),
            default_model: "opus-5[1m]".into(),
            aliases: vec!["claude@algedonic.dev".into()],
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
        }
    }

    #[test]
    fn a_declaration_is_an_agent_id_a_name_and_a_priced_model() {
        assert!(validate_agent(&input()).is_ok());
        for bad_id in [
            "claude@algedonic.dev",
            "claude:opus-5",
            "emp-claude",
            "agent-",
            "agent-Claude",
        ] {
            let mut bad = input();
            bad.id = bad_id.into();
            let why = validate_agent(&bad).unwrap_err();
            assert!(why.contains("agent-<slug>"), "{bad_id}: {why}");
        }
        let mut bad = input();
        bad.default_model = String::new();
        assert!(validate_agent(&bad).unwrap_err().contains("default_model"));
        let mut bad = input();
        bad.hourly_budget_usd_micros = Some(-1);
        assert!(validate_agent(&bad).unwrap_err().contains("hourly_budget"));
        let mut bad = input();
        bad.id = String::new();
        assert!(validate_agent(&bad).unwrap_err().contains("needs an id"));
    }

    /// A field the table cannot hold is refused by name, not dropped.
    #[test]
    fn a_field_the_registry_cannot_hold_is_refused_by_name() {
        let err = toml::from_str::<AgentInput>(
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\nrole = \"engineering-agent\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("role"), "{err}");
        let ok: AgentInput = toml::from_str(
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\n",
        )
        .unwrap();
        assert!(ok.aliases.is_empty());
        assert_eq!(ok.hourly_budget_usd_micros, None);
    }

    /// A row updated to the declaration names every field that moved,
    /// with what it read and what it reads now (backlog 09887242): the
    /// line is `updated N: id (field from → to, …)`, strings bare,
    /// nulls and lists as JSON — and a row already as declared is a
    /// count, never a silent nothing.
    #[test]
    fn an_updated_row_names_each_field_from_and_to() {
        let before = AgentRow {
            id: "agent-claude".into(),
            display_name: "Claude (Claude Code sessions on the dev pod)".into(),
            default_model: "opus-5[1m]".into(),
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: vec!["claude@algedonic.dev".into()],
        };
        let mut after = before.clone();
        after.display_name = "Claude (engineering)".into();
        let changes = before.changes_to(&after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field, "display_name");
        assert_eq!(
            changes[0].render(),
            "display_name Claude (Claude Code sessions on the dev pod) → Claude (engineering)"
        );
        assert!(before.changes_to(&before).is_empty());

        after.max_concurrent_runs = Some(2);
        after.aliases.push("other@algedonic.dev".into());
        let changes = before.changes_to(&after);
        let fields: Vec<&str> = changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["display_name", "max_concurrent_runs", "aliases"]);
        assert_eq!(changes[1].render(), "max_concurrent_runs null → 2");
        assert_eq!(
            changes[2].render(),
            "aliases [\"claude@algedonic.dev\"] → [\"claude@algedonic.dev\",\"other@algedonic.dev\"]"
        );

        let out = AgentsBatchOutcome {
            received: 2,
            inserted: 1,
            updated: vec![UpdatedRow {
                id: "agent-claude".into(),
                changes: vec![FieldChange::new(
                    "display_name",
                    "Claude (Claude Code sessions on the dev pod)",
                    "Claude (engineering)",
                )],
            }],
            unchanged: 0,
        };
        assert_eq!(
            out.summary(),
            "received 2, inserted 1, updated 1: agent-claude (display_name Claude (Claude Code sessions on the dev pod) → Claude (engineering))"
        );
        let out = AgentsBatchOutcome {
            received: 1,
            inserted: 0,
            updated: vec![],
            unchanged: 1,
        };
        assert_eq!(
            out.summary(),
            "received 1, inserted 0, 1 already as declared"
        );
        assert_eq!(render_updated(&[]), "");
    }
}
