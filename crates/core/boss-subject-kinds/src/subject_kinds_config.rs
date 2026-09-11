//! Configuration for the `boss-subject-kinds-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SubjectKindsApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
}

impl SubjectKindsApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for SubjectKindsApiConfig {
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
        let path = scratch_dir("boss-subject-kinds-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7830"
"#,
        );

        let cfg = SubjectKindsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7830");
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-subject-kinds-config-absent").join("subject-kinds.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(SubjectKindsApiConfig::load(&path).is_err());
    }
}
