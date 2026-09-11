//! Configuration for the `boss-locations-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LocationsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
}

impl LocationsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for LocationsApiConfig {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::{scratch_dir, scratch_path, write_file};

    #[test]
    fn loads_valid_toml() {
        let path = scratch_dir("boss-locations-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7820"
"#,
        );

        let cfg = LocationsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7820");
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-locations-config-absent").join("locations.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(LocationsApiConfig::load(&path).is_err());
    }
}
