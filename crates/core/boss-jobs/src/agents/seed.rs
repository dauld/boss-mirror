//! The one loader for `seeds/agents.toml` — read by `boss tenant check`
//! (the verdict) and `boss tenant publish` (the batch body), so the
//! file the check passes is the file the door admits (CLAUDE.md §9a:
//! one definition; the sensors loader's shape). `[[agent]]` rows in
//! the [`AgentInput`] shape; every row runs [`validate_agent`] here,
//! so a bad declaration is refused with the row named before it
//! reaches any door.

use std::path::Path;

use serde::Deserialize;

use super::types::{AgentInput, validate_agent};

#[derive(Debug, Deserialize)]
struct AgentsFile {
    #[serde(default)]
    agent: Vec<AgentInput>,
}

/// Parse the file's text. Separate from the path-taking loader so the
/// check's "parsed to nothing" refusal and the tests can hand it text.
pub fn parse_agents_toml(text: &str) -> Result<Vec<AgentInput>, String> {
    let file: AgentsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for a in &file.agent {
        validate_agent(a)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for a in &file.agent {
        if !seen.insert(&a.id) {
            return Err(format!("agent {} is declared twice", a.id));
        }
    }
    let mut aliases = std::collections::BTreeMap::new();
    for a in &file.agent {
        for alias in &a.aliases {
            if let Some(other) = aliases.insert(alias, &a.id)
                && other != &a.id
            {
                return Err(format!(
                    "alias {alias} is declared for both {other} and {} — a login signs as ONE actor",
                    a.id
                ));
            }
        }
    }
    Ok(file.agent)
}

pub fn load_agents_toml(path: &Path) -> Result<Vec<AgentInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_agents_toml(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real tenant's shape — its first draft, role and department
    /// included, which the registry can hold since backlog ab192a9f.
    const FILE: &str = r#"
[[agent]]
id = "agent-claude"
display_name = "Claude (engineering)"
default_model = "opus-5[1m]"
aliases = ["claude@algedonic.dev"]
role = "engineering-agent"
department = "engineering"
"#;

    #[test]
    fn the_tenant_shape_parses_to_a_declaration() {
        let rows = parse_agents_toml(FILE).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "agent-claude");
        assert_eq!(rows[0].default_model, "opus-5[1m]");
        assert_eq!(rows[0].aliases, ["claude@algedonic.dev"]);
        assert_eq!(rows[0].role.as_deref(), Some("engineering-agent"));
        assert_eq!(rows[0].department.as_deref(), Some("engineering"));
        assert_eq!(rows[0].hourly_budget_usd_micros, None);
        // A row that declares neither holds neither: NULL, not "".
        let bare = FILE
            .lines()
            .filter(|l| !l.starts_with("role") && !l.starts_with("department"))
            .collect::<Vec<_>>()
            .join("\n");
        let rows = parse_agents_toml(&bare).unwrap();
        assert_eq!(
            (rows[0].role.as_deref(), rows[0].department.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn a_bad_row_a_twin_and_a_shared_alias_are_refused_by_name() {
        let bad = FILE.replace("agent-claude", "claude@algedonic.dev");
        assert!(
            parse_agents_toml(&bad)
                .unwrap_err()
                .contains("agent-<slug>")
        );
        let twice = format!("{FILE}\n{FILE}");
        assert!(parse_agents_toml(&twice).unwrap_err().contains("twice"));
        let shared = format!("{FILE}\n{}", FILE.replace("agent-claude", "agent-other"));
        let why = parse_agents_toml(&shared).unwrap_err();
        assert!(
            why.contains("claude@algedonic.dev") && why.contains("ONE actor"),
            "{why}"
        );
        // A field the registry still cannot hold is refused naming the
        // field, never silently dropped (the rule that once refused
        // role and department, before ab192a9f gave them columns).
        let drafted = format!("{FILE}manager_id = \"emp-david\"\n");
        assert!(
            parse_agents_toml(&drafted)
                .unwrap_err()
                .contains("manager_id")
        );
        let blank = FILE.replace("role = \"engineering-agent\"", "role = \"\"");
        let why = parse_agents_toml(&blank).unwrap_err();
        assert!(why.contains("role is empty"), "{why}");
        assert!(parse_agents_toml("").unwrap().is_empty());
    }
}
