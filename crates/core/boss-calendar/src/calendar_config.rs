//! Configuration for the `boss-calendar-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

/// Re-exported under the crate's historical name. The shared loader's
/// [`ConfigError`] now backs every service config; this alias keeps
/// `boss_calendar::CalendarConfigError` resolving for downstream users.
pub type CalendarConfigError = ConfigError;

#[derive(Debug, Clone, Deserialize)]
pub struct CalendarApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for domain event publishing. If omitted, events
    /// are not published. Production binaries always set this so
    /// reservation events land in `audit_log`.
    #[serde(default)]
    pub nats_url: Option<String>,
}

impl CalendarApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for CalendarApiConfig {
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
        let path = scratch_dir("boss-calendar-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"postgres_url = "postgres://localhost/boss"
http_bind = "0.0.0.0:7860"
"#,
        );

        let cfg = CalendarApiConfig::load(&path).unwrap();
        assert_eq!(cfg.http_bind, "0.0.0.0:7860");
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-calendar-config-absent").join("calendar.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(CalendarApiConfig::load(&path).is_err());
    }

    #[test]
    fn rejects_empty_postgres_url() {
        let path = scratch_dir("boss-calendar-config-bad").join("bad.toml");
        write_file(
            &path,
            r#"postgres_url = ""
http_bind = "0.0.0.0:7860"
"#,
        );
        assert!(CalendarApiConfig::load(&path).is_err());
    }
}
