//! Configuration for the `boss-shipping-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ShippingApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events are not published.
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Base URL for the boss-classes HTTP API. Every shipment write
    /// that carries a `carrier` validates it against the Class
    /// registry before commit. Required: `Carrier` is a free-text
    /// wrapper, so registry validation is the only gate keeping a
    /// typo'd or unregistered carrier code out of the table. The
    /// startup-time validate() rejects an empty value rather than
    /// silently no-op'ing the check.
    pub classes_api_url: String,
}

impl ShippingApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for ShippingApiConfig {
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
        if self.classes_api_url.is_empty() {
            return Err(ConfigError::Validation(
                "classes_api_url must not be empty (Class registry \
                 validation is mandatory; carrier validation lives in \
                 the app layer)"
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
        let path = scratch_dir("boss-shipping-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7100"
classes_api_url = "http://127.0.0.1:7800"
"#,
        );

        let cfg = ShippingApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7100");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for carrier
        // values now that the schema CHECK is gone. Loading a config
        // without classes_api_url must fail at startup, not later.
        let path = scratch_dir("boss-shipping-config-missing-classes").join("missing-classes.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7100"
"#,
        );

        let err = ShippingApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-shipping-config-absent").join("shipping.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(ShippingApiConfig::load(&path).is_err());
    }
}
