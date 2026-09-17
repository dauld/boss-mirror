//! TOML configuration for the `boss-observability` service.
//!
//! Example:
//! ```toml
//! bind = "127.0.0.1:7880"  # loopback — the gateway proxies; widen deliberately
//! nats_url = "nats://os-manager-1:4222"
//! static_dir = "/var/lib/boss-observability/web"
//!
//! [[vms]]
//! id = "os-worker-1"
//! http_url = "http://os-worker-1:7700"
//!
//! [[vms]]
//! id = "os-worker-2"
//! http_url = "http://os-worker-2:7700"
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub bind: String,
    pub nats_url: String,
    /// Directory containing the compiled web dashboard. Optional — if absent,
    /// only the JSON API is served.
    #[serde(default)]
    pub static_dir: Option<String>,
    #[serde(default)]
    pub vms: Vec<VmEntry>,
    /// When set, /api/snapshot returns synthetic agent data
    /// instead of fanning out to real VMs, and the SSE channel
    /// emits a periodic stream of plausible telemetry events.
    /// For the brewery playground (and any deployment that
    /// hasn't wired up real cybernetics yet) so the /ops surface
    /// shows what real-agent oversight would look like. Tenants
    /// running real agents leave this absent and configure `vms`
    /// instead.
    #[serde(default)]
    pub demo_agents: Option<DemoAgentsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoAgentsConfig {
    /// Seconds between synthetic SSE telemetry ticks. Defaults to
    /// 8 seconds — fast enough that the operator sees activity,
    /// slow enough not to spam.
    #[serde(default = "default_tick_seconds")]
    pub tick_seconds: u64,
}

fn default_tick_seconds() -> u64 {
    8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmEntry {
    pub id: String,
    pub http_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("bind must not be empty")]
    EmptyBind,
    #[error("nats_url must not be empty")]
    EmptyNatsUrl,
    #[error("duplicate vm id `{0}`")]
    DuplicateVmId(String),
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.bind.is_empty() {
            return Err(ConfigError::EmptyBind);
        }
        if self.nats_url.is_empty() {
            return Err(ConfigError::EmptyNatsUrl);
        }
        let mut seen = std::collections::HashSet::new();
        for vm in &self.vms {
            if !seen.insert(vm.id.as_str()) {
                return Err(ConfigError::DuplicateVmId(vm.id.clone()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
bind = "0.0.0.0:7800"
nats_url = "nats://os-manager-1:4222"
static_dir = "/var/lib/boss-observability/web"

[[vms]]
id = "os-worker-1"
http_url = "http://os-worker-1:7700"

[[vms]]
id = "os-worker-2"
http_url = "http://os-worker-2:7700"
"#;

    #[test]
    fn parses_sample_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.vms.len(), 2);
        assert_eq!(cfg.vms[0].id, "os-worker-1");
        assert_eq!(
            cfg.static_dir.as_deref(),
            Some("/var/lib/boss-observability/web")
        );
    }

    #[test]
    fn rejects_duplicate_vm_ids() {
        let bad = r#"
bind = "0.0.0.0:7800"
nats_url = "nats://localhost:4222"
[[vms]]
id = "dup"
http_url = "http://a:7700"
[[vms]]
id = "dup"
http_url = "http://b:7700"
"#;
        let cfg: Config = toml::from_str(bad).unwrap();
        assert!(matches!(
            cfg.validate().unwrap_err(),
            ConfigError::DuplicateVmId(_)
        ));
    }

    #[test]
    fn rejects_empty_bind() {
        let bad = r#"
bind = ""
nats_url = "nats://localhost:4222"
"#;
        let cfg: Config = toml::from_str(bad).unwrap();
        assert!(matches!(
            cfg.validate().unwrap_err(),
            ConfigError::EmptyBind
        ));
    }

    /// An absent `[demo_agents]` block is OFF: no synthetic snapshot,
    /// no telemetry loop (the binary spawns it only on `Some`). Pinned
    /// on 2026-09-17 (backlog b03f38de) when the generator stopped
    /// writing the block for every tenant — the whole fix rests on
    /// "absent means off" being a property of the parse, not a habit.
    #[test]
    fn demo_agents_is_off_unless_the_block_is_present() {
        let off: Config = toml::from_str(
            r#"
bind = "0.0.0.0:7800"
nats_url = "nats://localhost:4222"
"#,
        )
        .unwrap();
        off.validate().unwrap();
        assert!(off.demo_agents.is_none());

        let on: Config = toml::from_str(
            r#"
bind = "0.0.0.0:7800"
nats_url = "nats://localhost:4222"

[demo_agents]
"#,
        )
        .unwrap();
        assert_eq!(on.demo_agents.map(|d| d.tick_seconds), Some(8));
    }

    #[test]
    fn static_dir_is_optional() {
        let cfg = r#"
bind = "0.0.0.0:7800"
nats_url = "nats://localhost:4222"
"#;
        let parsed: Config = toml::from_str(cfg).unwrap();
        parsed.validate().unwrap();
        assert!(parsed.static_dir.is_none());
        assert!(parsed.vms.is_empty());
    }
}
