//! The one loader for `seeds/credentials.toml` — read by `boss tenant
//! check` (the verdict) and `boss tenant publish` (the batch body), so
//! the file the check passes is the file the door admits (CLAUDE.md
//! §9a: one definition; the sensors loader's shape). `[[credential]]`
//! rows in the [`CredentialInput`] shape; every row runs
//! [`validate_credential`] here, so a bad declaration is refused with
//! the row named before it reaches any door — and a row carrying a
//! VALUE under any key the shape does not name is refused by the
//! parser, which is the registry's one rule enforced at the file.

use std::path::Path;

use serde::Deserialize;

use super::types::{CredentialInput, validate_credential};

#[derive(Debug, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    credential: Vec<CredentialInput>,
}

/// Parse the file's text. Separate from the path-taking loader so the
/// check's "parsed to nothing" refusal and the tests can hand it text.
pub fn parse_credentials_toml(text: &str) -> Result<Vec<CredentialInput>, String> {
    // The parser's Display quotes the offending LINE — which, for the
    // one refusal this loader exists to make (an unknown key such as
    // `value`), would print the very text the refusal is protecting.
    // Message and line number only, never the source.
    let file: CredentialsFile = toml::from_str(text).map_err(|e| {
        let line = e
            .span()
            .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
        match line {
            Some(n) => format!("line {n}: {}", e.message()),
            None => e.message().to_string(),
        }
    })?;
    for c in &file.credential {
        validate_credential(c)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &file.credential {
        if !seen.insert(&c.id) {
            return Err(format!("credential {} is declared twice", c.id));
        }
    }
    Ok(file.credential)
}

pub fn load_credentials_toml(path: &Path) -> Result<Vec<CredentialInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_credentials_toml(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[[credential]]
id = "stripe-restricted-read"
kind = "stripe-restricted-key"
issuer = "stripe (minted by the operator in the dashboard, restricted key)"
principal = "the Stripe account that receives sponsorships"
scopes = ["charges: read", "checkout_sessions: read", "customers: read"]
storage_location = "k8s Secret boss/boss-credential-broker-root key stripe-restricted-read"
consumers = [
  { kind = "env", location = "dispatcher env BOSS_BROKER_STRIPE_KEY in the boss pod" },
]
notes = "read-only by construction"
"#;

    #[test]
    fn the_tenant_shape_parses_to_a_declaration() {
        let rows = parse_credentials_toml(FILE).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "stripe-restricted-read");
        assert_eq!(rows[0].rotation_policy, "on-demand", "the default posture");
        assert_eq!(rows[0].scopes.len(), 3);
        assert_eq!(rows[0].consumers[0].kind, "env");
    }

    /// The registry's one rule, at the file: a key the shape does not
    /// name is refused, so a value cannot ride in under `value`,
    /// `token` or `secret` and land in a registry row.
    #[test]
    fn a_row_carrying_a_value_is_refused_by_shape() {
        let with_value = format!("{FILE}value = \"sk_live_not_a_real_key\"\n");
        let why = parse_credentials_toml(&with_value).unwrap_err();
        assert!(why.contains("value"), "{why}");
        assert!(
            !why.contains("sk_live"),
            "the refusal names the key, never the text under it: {why}"
        );
    }

    #[test]
    fn a_bad_row_is_refused_by_name_and_a_twin_is_refused() {
        let bad = FILE.replace(
            "storage_location = \"k8s Secret boss/boss-credential-broker-root key stripe-restricted-read\"",
            "storage_location = \"\"",
        );
        assert!(
            parse_credentials_toml(&bad)
                .unwrap_err()
                .contains("stripe-restricted-read")
        );
        let twice = format!("{FILE}\n{FILE}");
        assert!(
            parse_credentials_toml(&twice)
                .unwrap_err()
                .contains("twice")
        );
        assert!(parse_credentials_toml("").unwrap().is_empty());
        let weekly = FILE.replace(
            "notes = \"read-only by construction\"",
            "rotation_policy = \"weekly\"",
        );
        assert!(
            parse_credentials_toml(&weekly)
                .unwrap_err()
                .contains("weekly")
        );
    }
}
