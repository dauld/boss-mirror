use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ProductsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    #[serde(default)]
    pub nats_url: Option<String>,
    /// Base URL of the Class registry (`boss-classes-api`). The upsert
    /// path validates each product's `product_kind` + `package_unit`
    /// against `(subject_kind='product', code)`, so a running registry
    /// is required — no permissive fallback in the deployed binary.
    pub classes_api_url: String,
}

impl ProductsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for ProductsApiConfig {
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
                 backs product_kind + package_unit validation)"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::{scratch_dir, write_file};

    #[test]
    fn loads_valid_toml() {
        let path = scratch_dir("boss-products-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7840"
classes_api_url = "http://127.0.0.1:7800"
"#,
        );

        let cfg = ProductsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7840");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for
        // product_kind + package_unit now that neither has a DB CHECK.
        // Loading a config without classes_api_url must fail at startup,
        // not on the first upsert.
        let path = scratch_dir("boss-products-config-missing-classes").join("missing-classes.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7840"
"#,
        );

        let err = ProductsApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }
}
