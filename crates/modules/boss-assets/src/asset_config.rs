//! Configuration for the `boss-assets-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AssetsApiConfig {
    pub nats_url: String,
    pub http_bind: String,
    /// When set, the assets API uses Postgres for durable storage.
    /// When absent, falls back to in-memory (events lost on restart).
    pub postgres_url: Option<String>,
    /// Base URL of the people service. Used by the cross-service
    /// guard that validates `AssetEvent.actor_id` (when not None)
    /// at write time. Required — assets can't safely accept events
    /// without it.
    pub people_api_url: String,
    /// Base URL for the boss-classes HTTP API. Every asset event is
    /// gated at the ingest boundary: whichever taxonomy field it
    /// carries (`source` on `Received`, `coverage` on
    /// `WarrantyStarted`, `condition` on `Sold`) is validated against
    /// the Class registry under `(subject_kind='asset')`. These fields
    /// live inside the `AssetEvent` JSONB payload — there is no flat
    /// column and no schema CHECK to backstop them, so this gate is the
    /// only thing keeping a typo'd or retired code out of the event
    /// log. Required: the startup-time validate() rejects an empty
    /// value rather than silently no-op'ing the check.
    pub classes_api_url: String,
    /// Base URLs for the device-insights projection fan-out. All
    /// three must be configured for `/api/assets/{asset_id}/insights`
    /// to respond; otherwise the route returns 503.
    pub catalog_api_url: String,
    pub jobs_api_url: String,
    pub inventory_api_url: String,
}

impl AssetsApiConfig {
    /// Load from a TOML file and validate non-empty fields.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for AssetsApiConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.nats_url.is_empty() {
            return Err(ConfigError::Validation("nats_url must not be empty".into()));
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
                 validation is mandatory; asset event taxonomy fields \
                 live in the JSONB payload with no schema CHECK, so the \
                 ingest gate is the only defense)"
                    .into(),
            ));
        }
        for (name, val) in [
            ("catalog_api_url", &self.catalog_api_url),
            ("jobs_api_url", &self.jobs_api_url),
            ("inventory_api_url", &self.inventory_api_url),
        ] {
            if val.is_empty() {
                return Err(ConfigError::Validation(format!("{name} must not be empty")));
            }
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
        let path = scratch_dir("boss-assets-config-valid").join("valid.toml");
        write_file(
            &path,
            r#"nats_url = "nats://127.0.0.1:4222"
http_bind = "0.0.0.0:7600"
people_api_url = "http://127.0.0.1:7500"
classes_api_url = "http://127.0.0.1:7800"
catalog_api_url = "http://127.0.0.1:7750"
jobs_api_url = "http://127.0.0.1:7900"
inventory_api_url = "http://127.0.0.1:7300"
"#,
        );

        let cfg = AssetsApiConfig::load(&path).unwrap();
        assert_eq!(cfg.nats_url, "nats://127.0.0.1:4222");
        assert_eq!(cfg.http_bind, "0.0.0.0:7600");
        assert_eq!(cfg.classes_api_url, "http://127.0.0.1:7800");
    }

    #[test]
    fn rejects_empty_nats_url() {
        let path = scratch_dir("boss-assets-config-empty-nats").join("empty_nats.toml");
        write_file(
            &path,
            r#"nats_url = ""
http_bind = "0.0.0.0:7600"
people_api_url = "http://127.0.0.1:7500"
"#,
        );

        let err = AssetsApiConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("nats_url"));
    }

    #[test]
    fn rejects_missing_classes_api_url() {
        // Class registry validation is the only defense for the asset
        // event taxonomy fields — they live in JSONB with no schema
        // CHECK. Loading a config without classes_api_url must fail at
        // startup, not later at the first bad event.
        let path = scratch_dir("boss-assets-config-missing-classes").join("missing-classes.toml");
        write_file(
            &path,
            r#"nats_url = "nats://127.0.0.1:4222"
http_bind = "0.0.0.0:7600"
people_api_url = "http://127.0.0.1:7500"
catalog_api_url = "http://127.0.0.1:7750"
jobs_api_url = "http://127.0.0.1:7900"
inventory_api_url = "http://127.0.0.1:7300"
"#,
        );

        let err = AssetsApiConfig::load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("classes_api_url"),
            "expected classes_api_url error, got: {msg}"
        );
    }

    #[test]
    fn rejects_missing_file() {
        let path = scratch_path("boss-assets-config-absent").join("assets.toml");
        assert!(!path.exists(), "{} must not exist", path.display());
        assert!(AssetsApiConfig::load(&path).is_err());
    }
}
