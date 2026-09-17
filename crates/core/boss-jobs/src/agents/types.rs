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

impl AgentRow {
    /// The fields where `declared` disagrees with this row — what a
    /// publish names when it keeps a row the platform already
    /// registered (insert-if-absent never overwrites). `aliases`
    /// differs when a declared alias does not resolve to this id.
    pub fn differs_from(&self, declared: &AgentInput) -> Vec<String> {
        let mut out = Vec::new();
        if self.display_name != declared.display_name {
            out.push("display_name".to_string());
        }
        if self.default_model != declared.default_model {
            out.push("default_model".to_string());
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

/// A declared row the registry already held: kept as registered, with
/// the fields the declaration disagrees on named (empty = identical).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptAgent {
    pub id: String,
    pub differs: Vec<String>,
}

/// What a batch did: rows received, rows inserted, and the rows kept
/// as the platform registered them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub kept: Vec<KeptAgent>,
}

impl AgentsBatchOutcome {
    /// One line for a publish report: counts, then each kept row that
    /// differs, by field — so a tenant whose declaration disagrees with
    /// the registry reads it in the plan line, never in silence.
    pub fn summary(&self) -> String {
        let mut s = format!("received {}, inserted {}", self.received, self.inserted);
        let differing: Vec<String> = self
            .kept
            .iter()
            .filter(|k| !k.differs.is_empty())
            .map(|k| format!("{} ({} differs)", k.id, k.differs.join(", ")))
            .collect();
        let same = self.kept.len() - differing.len();
        if same > 0 {
            s.push_str(&format!(", {same} already registered as declared"));
        }
        if !differing.is_empty() {
            s.push_str(&format!(
                "; kept as the platform registered, not as declared: {}",
                differing.join("; ")
            ));
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

    #[test]
    fn a_kept_row_names_the_fields_the_declaration_disagrees_on() {
        let row = AgentRow {
            id: "agent-claude".into(),
            display_name: "Claude (Claude Code sessions on the dev pod)".into(),
            default_model: "opus-5[1m]".into(),
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: vec!["claude@algedonic.dev".into()],
        };
        assert_eq!(row.differs_from(&input()), ["display_name"]);
        let mut same = input();
        same.display_name = row.display_name.clone();
        assert!(row.differs_from(&same).is_empty());
        let mut more = input();
        more.aliases.push("other@algedonic.dev".into());
        assert_eq!(more.aliases.len(), 2);
        assert_eq!(row.differs_from(&more), ["display_name", "aliases"]);

        let out = AgentsBatchOutcome {
            received: 2,
            inserted: 1,
            kept: vec![KeptAgent {
                id: "agent-claude".into(),
                differs: vec!["display_name".into()],
            }],
        };
        assert_eq!(
            out.summary(),
            "received 2, inserted 1; kept as the platform registered, not as declared: agent-claude (display_name differs)"
        );
        let out = AgentsBatchOutcome {
            received: 1,
            inserted: 0,
            kept: vec![KeptAgent {
                id: "agent-claude".into(),
                differs: vec![],
            }],
        };
        assert_eq!(
            out.summary(),
            "received 1, inserted 0, 1 already registered as declared"
        );
    }
}
