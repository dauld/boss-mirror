//! The one loader for the tree's estate declaration — the `[[node]]`
//! rows of infra/estate/estate.toml (backlog ee368d0c; the file H10
//! made the one place the estate's addresses live, 5222163e). Read by
//! `boss estate declare` (the launcher's publish on every pod start)
//! and by the tests that hold the tree's declaration to what a fresh
//! database reads back, so the file the verb sends is the file the
//! door admits (CLAUDE.md §9a).
//!
//! The file's other keys (`sor_url`, `forge_host`, …) are the shell
//! renderer's and are ignored here; a `[[node]]` row is refused by
//! name on the first bad field, and a key the row shape does not name
//! is refused by the parser — observed state (`free_gb`) is not a
//! declaration.

use std::path::Path;

use serde::Deserialize;

use crate::port::{EstateNodeInput, validate_estate_node};

#[derive(Debug, Deserialize)]
struct EstateFile {
    #[serde(default)]
    node: Vec<EstateNodeInput>,
}

/// Parse the file's text. Separate from the path-taking loader so the
/// tests can hand it text.
pub fn parse_estate_toml(text: &str) -> Result<Vec<EstateNodeInput>, String> {
    let file: EstateFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for n in &file.node {
        validate_estate_node(n)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for n in &file.node {
        if !seen.insert(&n.id) {
            return Err(format!("node {} is declared twice", n.id));
        }
    }
    Ok(file.node)
}

pub fn load_estate_toml(path: &Path) -> Result<Vec<EstateNodeInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_estate_toml(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
# the addresses, read by the shell renderer
sor_url = "http://192.0.2.34:7900"
forge_host = "192.0.2.15"

[[node]]
id = "cp-1"
label = "cp-1"
address = "192.0.2.11"
role = "talos-control-plane"
cpu = 8
memory_gb = 15
disk_gb = 236

[[node]]
id = "forge"
label = "Forge host"
address = "192.0.2.15"
role = "forge"
roles = ["cluster-operator"]
notes = "the pipeline host"
"#;

    #[test]
    fn the_node_rows_parse_and_the_address_keys_are_left_to_the_renderer() {
        let rows = parse_estate_toml(FILE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "cp-1");
        assert_eq!(rows[0].cpu, Some(8));
        assert!(rows[0].roles.is_empty());
        assert_eq!(rows[1].roles, vec!["cluster-operator".to_string()]);
        assert_eq!(rows[1].notes.as_deref(), Some("the pipeline host"));
        assert!(parse_estate_toml("sor_url = \"x\"\n").unwrap().is_empty());
    }

    #[test]
    fn a_bad_row_is_refused_by_name_and_observed_state_is_not_a_declaration() {
        let bad = FILE.replace("role = \"forge\"", "role = \"Forge Host\"");
        let why = parse_estate_toml(&bad).unwrap_err();
        assert!(why.contains("forge") && why.contains("kebab-case"), "{why}");
        let observed = format!("{FILE}free_gb = 100\n");
        assert!(
            parse_estate_toml(&observed)
                .unwrap_err()
                .contains("free_gb")
        );
        let twice = format!(
            "{FILE}\n[[node]]\nid = \"cp-1\"\nlabel = \"x\"\naddress = \"y\"\nrole = \"talos-worker\"\n"
        );
        assert!(parse_estate_toml(&twice).unwrap_err().contains("twice"));
    }
}
