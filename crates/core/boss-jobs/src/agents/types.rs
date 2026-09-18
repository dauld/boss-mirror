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
//! would silently drop is refused by name at `boss tenant check` rather
//! than believed.
//!
//! WHY A ROLE AND A DEPARTMENT (backlog ab192a9f, 2026-09-17). The
//! real tenant's first draft carried both and was refused for it. But
//! an agent is an executor like a person (CLAUDE.md: humans and agents
//! are CPUs in the same machine), and a step whose audience is
//! `{ role = X }` resolves to the HOLDERS of X — every reader that
//! enumerated holders read the employees roster only, so an agent could
//! be reached by a role audience never, and by id only. Both columns
//! are Class codes under `(employee, role)` / `(employee, department)`,
//! the registry rows an employee's are validated against, and the
//! batch door checks them with the same client (`http.rs`).

use boss_core::actor::REGISTERED_AGENT_PREFIX;
use serde::{Deserialize, Serialize};

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
    /// The role this agent holds — a Class code under `(employee,
    /// role)`, checked against the registry at the batch door. `None`
    /// is "holds no role": reachable by id, never by a role audience.
    #[serde(default)]
    pub role: Option<String>,
    /// Where it sits in the org — a Class code under `(employee,
    /// department)`. Carried and validated like an employee's; nothing
    /// routes on it until design f5ebd2e1 car 2 makes department a
    /// selector.
    #[serde(default)]
    pub department: Option<String>,
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
    for (attribute, code) in [("role", &a.role), ("department", &a.department)] {
        if code.as_deref().is_some_and(|c| c.trim().is_empty()) {
            return Err(format!(
                "agent {}: {attribute} is empty — name a Class code under (employee, {attribute}) \
                 or omit the key",
                a.id
            ));
        }
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
    /// Always present on the wire, `null` until declared — a reader
    /// (the dispatcher's roster union, a probe) tells "holds no role"
    /// from "a registry that predates the column" by the key.
    pub role: Option<String>,
    pub department: Option<String>,
    pub hourly_budget_usd_micros: Option<i64>,
    pub max_concurrent_runs: Option<i32>,
    /// Every login that resolves to this id, sorted.
    pub aliases: Vec<String>,
}

/// The change vocabulary is the platform's (`boss_core::publish`,
/// design e187198f): one shape for every door and for the verb's own
/// employee overlay — one rule, one rendering, for both halves of the
/// roster. Re-exported so the registry's callers keep their paths.
pub use boss_core::publish::{FieldChange, KeptRow, UpdatedRow, render_updated};

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
        if self.role != after.role {
            out.push(FieldChange::new("role", &self.role, &after.role));
        }
        if self.department != after.department {
            out.push(FieldChange::new(
                "department",
                &self.department,
                &after.department,
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

    /// Every declared field `declared` disagrees with this row on —
    /// what a take WOULD change, named on a kept row under the default
    /// insert-if-absent (design e187198f). The six columns compare
    /// exactly; `aliases` differs when a declared login does not sign
    /// as this id (a login the tenant does not declare is never a
    /// difference — it is kept under either mode).
    pub fn differs_from(&self, declared: &AgentInput) -> Vec<String> {
        let mut out = Vec::new();
        if self.display_name != declared.display_name {
            out.push("display_name".to_string());
        }
        if self.default_model != declared.default_model {
            out.push("default_model".to_string());
        }
        if self.role != declared.role {
            out.push("role".to_string());
        }
        if self.department != declared.department {
            out.push("department".to_string());
        }
        if self.hourly_budget_usd_micros != declared.hourly_budget_usd_micros {
            out.push("hourly_budget_usd_micros".to_string());
        }
        if self.max_concurrent_runs != declared.max_concurrent_runs {
            out.push("max_concurrent_runs".to_string());
        }
        if declared.aliases.iter().any(|a| !self.aliases.contains(a)) {
            out.push("aliases".to_string());
        }
        out
    }
}

/// What a batch did: rows received, rows inserted, rows the registry
/// already held that a take UPDATED to the declaration (each change
/// named), rows it held and KEPT under insert-if-absent with the
/// differing fields named (design e187198f), and rows already
/// registered exactly as declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub updated: Vec<UpdatedRow>,
    /// Absent on the wire from a door that predates the field.
    #[serde(default)]
    pub kept: Vec<KeptRow>,
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
        let kept = boss_core::publish::render_kept(&self.kept, Some("agents"));
        if !kept.is_empty() {
            s.push_str("; ");
            s.push_str(&kept);
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
            role: None,
            department: None,
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
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\nmanager_id = \"emp-david\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("manager_id"), "{err}");
        let ok: AgentInput = toml::from_str(
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\n",
        )
        .unwrap();
        assert!(ok.aliases.is_empty());
        assert_eq!(ok.hourly_budget_usd_micros, None);
    }

    /// An agent holds a role and sits in a department the way an
    /// employee does (backlog ab192a9f): both parse from the tenant's
    /// shape, both are optional (NULL until declared), an empty string
    /// is refused as a code that names nothing, and a field the table
    /// still cannot hold is refused by name as before.
    #[test]
    fn a_declaration_may_carry_a_role_and_a_department() {
        let a: AgentInput = toml::from_str(
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\n\
             role = \"engineering-agent\"\ndepartment = \"engineering\"\n",
        )
        .unwrap();
        assert_eq!(a.role.as_deref(), Some("engineering-agent"));
        assert_eq!(a.department.as_deref(), Some("engineering"));
        assert!(validate_agent(&a).is_ok());
        let bare = input();
        assert_eq!((bare.role, bare.department), (None, None));
        let mut blank = input();
        blank.role = Some("  ".into());
        let why = validate_agent(&blank).unwrap_err();
        assert!(why.contains("role") && why.contains("Class"), "{why}");
        let mut blank = input();
        blank.department = Some(String::new());
        assert!(validate_agent(&blank).unwrap_err().contains("department"));
        let err = toml::from_str::<AgentInput>(
            "id = \"agent-claude\"\ndisplay_name = \"Claude\"\ndefault_model = \"opus-5[1m]\"\nteam = \"it\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("team"), "{err}");

        // The row reads both back, and a declaration that moves either
        // names the change like any other field.
        let before = AgentRow {
            id: "agent-claude".into(),
            display_name: "Claude".into(),
            default_model: "opus-5[1m]".into(),
            role: None,
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: vec![],
        };
        let mut after = before.clone();
        after.role = Some("engineering-agent".into());
        after.department = Some("engineering".into());
        let changes = before.changes_to(&after);
        let rendered: Vec<String> = changes.iter().map(FieldChange::render).collect();
        assert_eq!(
            rendered,
            [
                "role null → engineering-agent",
                "department null → engineering"
            ]
        );
        let json = serde_json::to_value(&before).unwrap();
        assert!(
            json.get("role").is_some_and(serde_json::Value::is_null),
            "a row without a role still carries the key, so a reader can tell null from absent: {json}"
        );
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
            role: None,
            department: None,
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
            kept: vec![],
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
            kept: vec![],
            unchanged: 1,
        };
        assert_eq!(
            out.summary(),
            "received 1, inserted 0, 1 already as declared"
        );
        assert_eq!(render_updated(&[]), "");
    }

    /// The default's answer (design e187198f): a held row the
    /// declaration disagrees with is kept, the differing fields are
    /// named, and the line says which flag would overwrite it. A
    /// declared login that does not sign as the id is a difference; a
    /// login the tenant does not declare is not.
    #[test]
    fn a_kept_row_names_its_differing_fields_and_the_take_flag() {
        let held = AgentRow {
            id: "agent-claude".into(),
            display_name: "Claude (Claude Code sessions on the dev pod)".into(),
            default_model: "opus-5[1m]".into(),
            role: None,
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(8),
            aliases: vec![
                "claude@algedonic.dev".into(),
                "ops-added@algedonic.dev".into(),
            ],
        };
        let mut declared = input();
        declared.max_concurrent_runs = Some(3);
        assert_eq!(
            held.differs_from(&declared),
            ["display_name", "max_concurrent_runs"]
        );
        declared.aliases.push("new@algedonic.dev".into());
        assert_eq!(
            held.differs_from(&declared),
            ["display_name", "max_concurrent_runs", "aliases"]
        );
        let mut same = input();
        same.display_name = held.display_name.clone();
        same.max_concurrent_runs = Some(8);
        assert!(held.differs_from(&same).is_empty());

        let out = AgentsBatchOutcome {
            received: 1,
            inserted: 0,
            updated: vec![],
            kept: vec![KeptRow {
                id: "agent-claude".into(),
                differs: vec!["display_name".into(), "max_concurrent_runs".into()],
            }],
            unchanged: 0,
        };
        assert_eq!(
            out.summary(),
            "received 1, inserted 0; kept: agent-claude differs on display_name, \
             max_concurrent_runs (the instance is the truth; --take agents overwrites)"
        );
        // A door that predates the field answers without it.
        let old: AgentsBatchOutcome =
            serde_json::from_str(r#"{"received":1,"inserted":0,"updated":[],"unchanged":1}"#)
                .unwrap();
        assert!(old.kept.is_empty());
    }
}
