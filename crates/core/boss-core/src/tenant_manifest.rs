//! The tenant manifest — `tenant.toml`, the file that names a tenant.
//!
//! One shape, one reader. The gateway answers `/api/tenant/manifest`
//! from it (and inlines it into `index.html`), and `boss tenant check`
//! validates a tenant directory against it (backlog fcc1d57b,
//! 2026-09-16). Until that car the struct was private to the gateway,
//! so a second reader would have been a second definition — CLAUDE.md
//! §9a. The simulator's `TenantConfig` (boss-sim) is a STRICTER,
//! sim-only reading of the same file (`seed`, `start_date`,
//! `[job_rates]`, …) that only a tenant with an engine needs; a real
//! company's manifest carries none of that, and this type is what the
//! platform reads from it.

use std::collections::BTreeMap;

use serde::Deserialize;

/// `[meta]` — how the tenant names itself. Both fields are optional
/// because a deployment that has not named itself is still a
/// deployment: the SPA falls back to "BOSS".
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct TenantMeta {
    /// The tenant's own name for itself, e.g. "Algedonic Ales". The
    /// SPA chrome once had a brewery's name hardcoded at three render
    /// sites; branding is tenant data, core does not know it.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Stable slug (`brewery`, `algedonic`) — the `owning_team` its
    /// Workflows are stamped with.
    #[serde(default)]
    pub tenant_id: Option<String>,
}

/// The whole file as the platform reads it. Every section is
/// optional and unknown sections are ignored, which is what lets the
/// simulator's sections ride in the same file.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct TenantToml {
    #[serde(default)]
    pub meta: TenantMeta,
    /// `[modules]` — which SPA modules this tenant shows.
    #[serde(default)]
    pub modules: BTreeMap<String, bool>,
    /// `[labels]` — display strings keyed by dotted path
    /// (`finance.revenue_category.<code>` rows become the revenue
    /// category list, for one).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl TenantToml {
    /// Parse the file's text. The error is toml's own, with its line
    /// and column — a check that reports it must not rephrase it.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_meta_block_parses_and_everything_else_defaults() {
        let t = TenantToml::parse("[meta]\ntenant_id = \"x\"\ndisplay_name = \"X\"\n").unwrap();
        assert_eq!(t.meta.tenant_id.as_deref(), Some("x"));
        assert_eq!(t.meta.display_name.as_deref(), Some("X"));
        assert!(t.modules.is_empty());
        assert!(t.labels.is_empty());
    }

    #[test]
    fn an_empty_file_is_an_unnamed_tenant_not_an_error() {
        let t = TenantToml::parse("").unwrap();
        assert_eq!(t, TenantToml::default());
    }

    #[test]
    fn sim_only_sections_are_ignored_not_refused() {
        let t = TenantToml::parse(
            "[meta]\ntenant_id = \"b\"\nseed = 7\n[job_rates.x]\nrate = 1.0\n[modules]\nledger = false\n",
        )
        .unwrap();
        assert_eq!(t.modules.get("ledger"), Some(&false));
    }
}
