//! Configuration for the `boss-ledger-api` binary.

use std::path::Path;

use boss_core::config::{ConfigError, Validate, load_toml};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LedgerApiConfig {
    pub postgres_url: String,
    pub http_bind: String,
    /// NATS URL for event publishing + audit_log mirroring. When
    /// absent, the ledger runs without emitting domain events (the
    /// fact-write path still works; the projection rebuilder just
    /// won't have audit_log rows to replay).
    #[serde(default)]
    pub nats_url: Option<String>,
}

impl LedgerApiConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        load_toml(path)
    }
}

impl Validate for LedgerApiConfig {
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
