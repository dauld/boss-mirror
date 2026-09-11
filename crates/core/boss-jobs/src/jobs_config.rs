//! Configuration for the `boss-jobs-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct JobsApiConfig {
    pub nats_url: String,
    pub http_bind: String,
    /// When set, the jobs API uses Postgres for durable storage.
    /// When absent, falls back to in-memory (jobs lost on restart).
    pub postgres_url: Option<String>,
    /// Base URL for the boss-calendar HTTP API. Optional —
    /// boss-jobs-api runs without it, and the calendar reservation
    /// hook on step transitions becomes a no-op. Set this once the
    /// calendar service is deployed.
    #[serde(default)]
    pub calendar_api_url: Option<String>,
    /// Base URL for the boss-subject-kinds HTTP API. Optional —
    /// when unset, Job creates accept any subject kind string. When
    /// set, the kind is validated against the registry and missing
    /// kinds are rejected with 400.
    #[serde(default)]
    pub subject_kinds_api_url: Option<String>,
    /// boss-classes HTTP API used at startup to seed the executive
    /// role cache (`metadata.is_executive = true`). Read by the
    /// escalation router so a `critical` Job on a platinum/gold
    /// account pages the tenant-defined executives. When absent,
    /// no roles are treated as executive.
    #[serde(default)]
    pub classes_api_url: Option<String>,
    /// Base URLs for the four upstream services the Subject
    /// existence checker needs. All four must be set for the
    /// checker to come up; missing any one leaves the checker
    /// disabled and the create-Job handler accepts any subject id.
    #[serde(default)]
    pub people_api_url: Option<String>,
    #[serde(default)]
    pub assets_api_url: Option<String>,
    #[serde(default)]
    pub locations_api_url: Option<String>,
    #[serde(default)]
    pub inventory_api_url: Option<String>,
}

impl JobsApiConfig {
    /// Load from a TOML file and validate non-empty fields.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for JobsApiConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.nats_url.is_empty() {
            return Err(ConfigError::Validation("nats_url must not be empty".into()));
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

    /// These fixtures write into a scratch root that is per-uid AND
    /// per-process. They used to share a fixed `/tmp/boss-jobs-config-test`,
    /// and on 2026-09-11 a leftover from another account made both of them
    /// fail with a bare `PermissionDenied` that named no path at all —
    /// `create_dir_all` returns `Ok` on an existing directory regardless of
    /// who owns it, so the failure surfaced at the first write instead.
    /// See `boss_testing::scratch`.
    /// `case` must be unique per test: `cargo test` runs these in
    /// parallel threads of ONE process, so two tests sharing a root would
    /// have one of them clear the other's file.
    fn fixture(case: &str, body: &str) -> std::path::PathBuf {
        let dir = boss_testing::scratch_dir(&format!("boss-jobs-config-{case}"));
        let path = dir.join("config.toml");
        boss_testing::write_file(&path, body);
        path
    }

    #[test]
    fn loads_valid_toml() {
        let path = fixture(
            "valid",
            r#"nats_url = "nats://127.0.0.1:4222"
http_bind = "0.0.0.0:7900"
"#,
        );

        let cfg = JobsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.nats_url, "nats://127.0.0.1:4222");
        assert_eq!(cfg.http_bind, "0.0.0.0:7900");
    }

    #[test]
    fn rejects_empty_nats_url() {
        let path = fixture(
            "empty-nats",
            r#"nats_url = ""
http_bind = "0.0.0.0:7900"
"#,
        );

        let err = JobsApiConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("nats_url"));
    }

    #[test]
    fn rejects_missing_file() {
        // Derived from this process's own scratch root so the path is
        // genuinely absent. A shared `/tmp/does-not-exist-jobs.toml` is an
        // absence assertion on a path any account can create.
        let path = boss_testing::scratch_path("boss-jobs-config-absent").join("no.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(JobsApiConfig::load(&path).is_err());
    }
}
