//! Configuration for the `boss-commerce-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CommerceApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Base URL of the people service (e.g. http://127.0.0.1:7500).
    /// Used by the cross-service guard that validates `account_id` on
    /// opportunity creation. Loud failure on startup if missing —
    /// commerce can't safely accept opportunities without it.
    pub people_api_url: String,
    /// Base URL for the boss-classes HTTP API. Every invoice create
    /// validates its `status` against the Class registry under
    /// `(subject_kind='invoice')`. Required: `InvoiceStatus` is a
    /// free-text wrapper, so registry validation is the only gate
    /// keeping a typo'd or unregistered status out of the table. The
    /// startup-time validate() rejects an empty value rather than
    /// silently no-op'ing the check.
    pub classes_api_url: String,
}

impl CommerceApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for CommerceApiConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.postgres_url.is_empty() {
            return Err(ConfigError::Validation(
                "postgres_url must not be empty".into(),
            ));
        }
        if self.http_bind.is_empty() {
            return Err(ConfigError::Validation(
                "http_bind must not be empty".into(),
            ));
        }
        if self.people_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "people_api_url must not be empty".into(),
            ));
        }
        if self.classes_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "classes_api_url must not be empty (Class registry \
                 validation is mandatory; invoice-status validation \
                 lives in the app layer)"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::{scratch_dir, scratch_path, write_file};

    #[test]
    fn loads_valid_toml() {
        let path = scratch_dir("boss-commerce-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7400"
people_api_url = "http://127.0.0.1:7500"
classes_api_url = "http://127.0.0.1:7800"
"#,
        );

        let cfg = CommerceApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7400");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for invoice
        // status now that the schema CHECK is gone. Loading a config
        // without classes_api_url must fail at startup, not later.
        let path = scratch_dir("boss-commerce-config-missing-classes").join("missing-classes.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7400"
people_api_url = "http://127.0.0.1:7500"
"#,
        );

        let err = CommerceApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-commerce-config-absent").join("commerce.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(CommerceApiConfig::load(&path).is_err());
    }
}
