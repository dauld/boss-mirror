//! The one loader for `seeds/sensors.toml` — read by `boss tenant
//! check` (the verdict) and `boss tenant publish` (the batch body), so
//! the file the check passes is the file the door admits (CLAUDE.md
//! §9a: one definition). `[[sensor]]` rows in the [`SensorInput`]
//! shape; every row runs [`validate_sensor`] here, so a bad declaration
//! is refused with the row named before it reaches any door.

use std::path::Path;

use serde::Deserialize;

use super::types::{SensorInput, validate_sensor};

#[derive(Debug, Deserialize)]
struct SensorsFile {
    #[serde(default)]
    sensor: Vec<SensorInput>,
}

/// Parse the file's text. Separate from the path-taking loader so the
/// check's "parsed to nothing" refusal and the tests can hand it text.
pub fn parse_sensors_toml(text: &str) -> Result<Vec<SensorInput>, String> {
    let file: SensorsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for s in &file.sensor {
        validate_sensor(s)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &file.sensor {
        if !seen.insert(&s.id) {
            return Err(format!("sensor {} is declared twice", s.id));
        }
    }
    Ok(file.sensor)
}

pub fn load_sensors_toml(path: &Path) -> Result<Vec<SensorInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_sensors_toml(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[[sensor]]
id = "stripe-sponsorships"
source = "stripe"
credential = "stripe-restricted-read"
every_minutes = 15
opens = "receive-a-sponsorship"
subject_kind = "custom"
"#;

    #[test]
    fn the_tenant_shape_parses_to_a_declaration() {
        let rows = parse_sensors_toml(FILE).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "stripe-sponsorships");
        assert_eq!(rows[0].opens, "receive-a-sponsorship");
        assert_eq!(rows[0].every_minutes, 15);
        assert!(rows[0].enabled);
    }

    /// The tenant's spelling of the page-view sensor (backlog
    /// 0b5c5081): a push-only source, no credential, no period — the
    /// row `boss tenant check` passes and `boss tenant publish` sends.
    #[test]
    fn a_push_only_row_is_admitted_with_no_credential() {
        let file = format!(
            "{FILE}\n[[sensor]]\nid = \"www-visits\"\nsource = \"site\"\n\
             opens = \"marketing-weekly\"\nsubject_kind = \"custom\"\n"
        );
        let rows = parse_sensors_toml(&file).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].is_push_only());
        assert!(rows[1].credential.is_empty());
        assert_eq!(rows[1].every_minutes, 0);
        // The same row with a period is refused, named.
        let with_period = format!("{file}every_minutes = 15\n");
        let why = parse_sensors_toml(&with_period).unwrap_err();
        assert!(
            why.contains("www-visits") && why.contains("push-only"),
            "{why}"
        );
    }

    #[test]
    fn a_bad_row_is_refused_by_name_and_a_twin_is_refused() {
        let bad = FILE.replace("every_minutes = 15", "every_minutes = 0");
        assert!(
            parse_sensors_toml(&bad)
                .unwrap_err()
                .contains("stripe-sponsorships")
        );
        let twice = format!("{FILE}\n{FILE}");
        assert!(parse_sensors_toml(&twice).unwrap_err().contains("twice"));
        assert!(parse_sensors_toml("").unwrap().is_empty());
    }
}
