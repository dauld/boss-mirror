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
    /// How far into the tree this tenant's IT department (its agents)
    /// may edit: a tier NAME from infra/platform/tiers.toml, judged by
    /// rank through `crate::tiers::TierMap::first_above` (a479faf7;
    /// design 01c3cc3f reader 3). `data` is data-only, `tenants` adds
    /// the tenant's own crate and site, `modules` the company layer,
    /// `core` everything — the operator's own instance. Read by `boss
    /// dispatch` (a packet declaring `metadata.paths` above the level
    /// is refused before the claim) and by the gate (a car whose diff
    /// crosses it is refused naming the path), both through the jobs
    /// API's `GET /api/tenant/edit-level`. Absent is NO level: nothing
    /// is enforced. The hosted default (`data`) is written where a
    /// hosted tenant is made — `boss tenant init` scaffolds it — not
    /// assumed by the reader, because every instance today is the
    /// operator's own and its manifest predates the field.
    #[serde(default)]
    pub edit_level: Option<String>,
}

/// `[gateway]` — what the instance's front door does for this tenant.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct TenantGateway {
    /// `public_reads` — the API reads this instance answers WITHOUT a
    /// session, by path. A per-instance declaration defaulting to
    /// NONE (design 11e60367 Q1, decided 2026-09-18; backlog
    /// b4afd7b9): until then the demo tenant's four landing-page reads
    /// were a constant in boss-gateway's route table, so the company's own
    /// instance answered `/api/workflows`, `/api/jobs/summary`,
    /// `/api/jobs/live` and `/api/events/public-tail` to anyone,
    /// with Cloudflare Access the only thing in front. The gateway
    /// resolves the list at boot against the reads that CAN be public
    /// (boss-gateway `public_reads.rs`) and refuses to start on one it
    /// does not know — a declaration cannot make a write public. A
    /// manifest that says nothing asks for nothing.
    #[serde(default)]
    pub public_reads: Vec<String>,
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
    /// `[gateway]` — the sessionless read set, and nothing else yet.
    #[serde(default)]
    pub gateway: TenantGateway,
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

    /// The reads an instance answers without a session are the
    /// tenant's declaration, not a gateway constant (design 11e60367
    /// Q1, decided 2026-09-18; backlog b4afd7b9). Absent means NONE:
    /// a manifest that says nothing about public reads asks for no
    /// sessionless read, which is what a company's instance wants
    /// and what the public demo tenant opts out of by naming its four.
    #[test]
    fn public_reads_are_declared_under_gateway_and_default_to_none() {
        let t = TenantToml::parse(
            "[meta]\ntenant_id = \"b\"\n[gateway]\npublic_reads = [\"/api/workflows\", \"/api/jobs/live\"]\n",
        )
        .unwrap();
        assert_eq!(
            t.gateway.public_reads,
            vec!["/api/workflows".to_string(), "/api/jobs/live".to_string()]
        );

        let none = TenantToml::parse("[meta]\ntenant_id = \"a\"\n").unwrap();
        assert!(none.gateway.public_reads.is_empty());
        let empty_section = TenantToml::parse("[gateway]\n").unwrap();
        assert!(empty_section.gateway.public_reads.is_empty());
    }

    /// The hosting edit level (a479faf7; design 01c3cc3f reader 3) is
    /// tenant data under `[meta]`: a tier name from
    /// infra/platform/tiers.toml. Absent is NO level — the doors that
    /// read it (boss dispatch, the gate) enforce nothing — because the
    /// only instances today are the operator's own, and a data-only
    /// default applied by the reader would have refused every code
    /// car on the instance's own pipeline the converge after it
    /// landed (the tenant repo's manifest declared none, measured
    /// 2026-09-19). The hosted default is written by the door that
    /// HOSTS: `boss tenant init` scaffolds `edit_level = "data"`
    /// explicitly, and the provisioning protocol will too.
    #[test]
    fn the_edit_level_is_declared_under_meta_and_absent_is_none() {
        let t = TenantToml::parse("[meta]\ntenant_id = \"a\"\nedit_level = \"tenants\"\n").unwrap();
        assert_eq!(t.meta.edit_level.as_deref(), Some("tenants"));
        let none = TenantToml::parse("[meta]\ntenant_id = \"a\"\n").unwrap();
        assert_eq!(none.meta.edit_level, None);
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
