//! The tier map — which tier a path in the tree belongs to, read from
//! the ONE definition `infra/platform/tiers.toml` (design 01c3cc3f,
//! decided 2026-09-19; `infra/platform/tiers.md` is the prose).
//!
//! WHY A LOADER AND NOT A LIST. Two consumers were about to decide for
//! themselves what "tier" means — the arrival classifier (ba429e7f:
//! is the core settling while work moves outward) and the hosting
//! edit level (a479faf7: a hosted tenant may edit data only, or its
//! tenant tier, or modules) — while a third already had: the lint
//! `tier-import-audit.sh` carried the prefixes as its own text. Three
//! copies of one fact is the §9a defect class. The file is the fact;
//! this module is the Rust reader; `infra/lint/lib/tiers.sh` is the
//! shell reader, held equal to this one over a fixture of paths by
//! `boss-testing/tests/tiers_sh.rs`.
//!
//! Embedded at build time so a running binary answers without a
//! checkout (the dispatch door will run in a service), and PURE: the
//! same path always maps to the same tier.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::Deserialize;

/// The file, verbatim, as it was when this binary was built.
pub const TIERS_TOML: &str = include_str!("../../../../infra/platform/tiers.toml");

/// One row of the map.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tier {
    pub name: String,
    /// 1 is the innermost; the headline of a mixed change is the
    /// LOWEST rank present.
    pub rank: u8,
    /// Path prefixes this tier owns. A `**/` prefix matches the rest
    /// at any depth (`**/seeds/`).
    pub paths: Vec<String>,
}

/// The whole map, in file order — order is the tie-break among equal
/// ranks, so it is kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TierMap {
    #[serde(rename = "tier", default)]
    pub tiers: Vec<Tier>,
}

#[derive(Debug, thiserror::Error)]
pub enum TiersError {
    #[error("tiers.toml does not parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("tiers.toml declares no [[tier]] rows")]
    Empty,
    #[error("tiers.toml declares tier `{0}` twice")]
    Duplicate(String),
}

/// Parse a tier map from its TOML text.
pub fn parse(text: &str) -> Result<TierMap, TiersError> {
    let map: TierMap = toml::from_str(text)?;
    if map.tiers.is_empty() {
        return Err(TiersError::Empty);
    }
    let mut seen = BTreeSet::new();
    for t in &map.tiers {
        if !seen.insert(t.name.as_str()) {
            return Err(TiersError::Duplicate(t.name.clone()));
        }
    }
    Ok(map)
}

/// The tree's own map — the embedded file, parsed once. An embedded
/// file that does not parse is a build defect (`the_embedded_map_parses`
/// pins it), surfaced as an error rather than an empty map that would
/// answer "no tier" for every path.
pub fn tier_map() -> Result<&'static TierMap, &'static TiersError> {
    static MAP: OnceLock<Result<TierMap, TiersError>> = OnceLock::new();
    MAP.get_or_init(|| parse(TIERS_TOML)).as_ref()
}

/// How many characters of `path` the prefix `pat` claims, or `None`
/// when it does not match. A plain prefix claims its own length; a
/// `**/x/` glob claims up to and including the first `x/` directory
/// at any depth — so a deeper match is a MORE specific one, which is
/// what lets `examples/<tenant>/seeds/` be data over tenants.
fn claimed(pat: &str, path: &str) -> Option<usize> {
    if let Some(rest) = pat.strip_prefix("**/") {
        if let Some(r) = path.strip_prefix(rest) {
            return Some(path.len() - r.len());
        }
        let needle = format!("/{rest}");
        return path.find(&needle).map(|i| i + needle.len());
    }
    path.starts_with(pat).then_some(pat.len())
}

impl TierMap {
    /// The tier a path belongs to: the row whose prefix claims the
    /// most of the path. `None` for a path no row claims — a reader
    /// says "no tier" rather than guessing one.
    pub fn tier_of(&self, path: &str) -> Option<&Tier> {
        let path = path.trim_start_matches("./");
        self.tiers
            .iter()
            .filter_map(|t| {
                t.paths
                    .iter()
                    .filter_map(|p| claimed(p, path))
                    .max()
                    .map(|n| (n, t))
            })
            // max_by_key keeps the LAST maximum; the first row written
            // must win a tie, so compare in reverse.
            .rev()
            .max_by_key(|(n, _)| *n)
            .map(|(_, t)| t)
    }

    /// The SET of tiers a change touched, sorted by name — one entry
    /// per tier however many paths hit it. Paths no row claims are
    /// left out, so a root-only change is the empty set.
    pub fn tiers_of<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let set: BTreeSet<&str> = paths
            .into_iter()
            .filter_map(|p| self.tier_of(p))
            .map(|t| t.name.as_str())
            .collect();
        set.into_iter().map(str::to_string).collect()
    }

    /// The headline among a set of tier names: the LOWEST rank, ties
    /// broken by file order (core before infra, modules before
    /// orchestrators). `None` when no named tier is in the map.
    pub fn headline<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Option<&Tier> {
        let names: BTreeSet<&str> = names.into_iter().collect();
        self.tiers
            .iter()
            .filter(|t| names.contains(t.name.as_str()))
            .min_by_key(|t| t.rank)
    }

    /// The tier a name denotes, for callers holding a stamped label.
    pub fn by_name(&self, name: &str) -> Option<&Tier> {
        self.tiers.iter().find(|t| t.name == name)
    }
}

/// `tier_map().tier_of(path)` over the embedded map — the one call
/// most readers need.
pub fn tier_of(path: &str) -> Option<&'static Tier> {
    tier_map().ok()?.tier_of(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> &'static TierMap {
        tier_map().expect("the embedded tiers.toml parses")
    }

    fn name_of(path: &str) -> Option<&'static str> {
        tier_of(path).map(|t| t.name.as_str())
    }

    #[test]
    fn the_embedded_map_parses_and_names_the_seven_tiers() {
        let names: Vec<&str> = map().tiers.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "core",
                "modules",
                "orchestrators",
                "tenants",
                "frontend",
                "infra",
                "data"
            ]
        );
    }

    #[test]
    fn a_crate_path_maps_to_its_tier_directory() {
        assert_eq!(name_of("crates/core/boss-jobs/src/lib.rs"), Some("core"));
        assert_eq!(
            name_of("crates/modules/boss-people/Cargo.toml"),
            Some("modules")
        );
        assert_eq!(
            name_of("crates/orchestrators/boss-cli/src/channels.rs"),
            Some("orchestrators")
        );
        assert_eq!(
            name_of("crates/tenants/boss-acme-engine/src/main.rs"),
            Some("tenants")
        );
        assert_eq!(name_of("examples/acme/DOMAIN.md"), Some("tenants"));
        assert_eq!(name_of("apps/web/src/it/yard/yard.ts"), Some("frontend"));
        assert_eq!(name_of("libs/web-kit/src/index.ts"), Some("frontend"));
        assert_eq!(name_of("infra/lint/gate.sh"), Some("infra"));
        assert_eq!(name_of("docs/design/x.md"), Some("data"));
    }

    #[test]
    fn the_most_specific_prefix_wins_so_platform_data_is_data_not_infra() {
        assert_eq!(name_of("infra/platform/tiers.toml"), Some("data"));
        assert_eq!(
            name_of("infra/platform/workflows/ship-a-change.toml"),
            Some("data")
        );
        assert_eq!(
            name_of("infra/dispatcher/rules/converge-on-merge.toml"),
            Some("data")
        );
        // The dispatcher's other files are still infra: only rules/ is data.
        assert_eq!(name_of("infra/dispatcher/README.md"), Some("infra"));
    }

    #[test]
    fn a_seeds_directory_at_any_depth_is_data_over_its_tier() {
        assert_eq!(name_of("examples/acme/seeds/tenant.toml"), Some("data"));
        assert_eq!(name_of("seeds/x.sql"), Some("data"));
        // `seeds` as a file name, not a directory, is not the glob.
        assert_eq!(name_of("crates/core/boss-jobs/src/seeds.rs"), Some("core"));
    }

    #[test]
    fn a_path_no_row_claims_is_no_tier() {
        assert_eq!(name_of("README.md"), None);
        assert_eq!(name_of(".forgejo/workflows/ci.yml"), None);
        assert_eq!(name_of("Cargo.toml"), None);
    }

    #[test]
    fn tiers_of_is_the_sorted_set_and_headline_is_the_lowest_rank() {
        let paths = [
            "apps/web/src/a.ts",
            "crates/core/boss-jobs/src/lib.rs",
            "apps/web/src/b.ts",
            "README.md",
        ];
        let set = map().tiers_of(paths);
        assert_eq!(set, ["core", "frontend"]);
        assert_eq!(
            map()
                .headline(set.iter().map(String::as_str))
                .map(|t| t.name.as_str()),
            Some("core")
        );
        assert_eq!(map().tiers_of(["docs/a.md", "docs/b.md"]), ["data"]);
        assert!(map().tiers_of(["README.md"]).is_empty());
        assert!(map().headline([]).is_none());
    }

    #[test]
    fn an_equal_rank_tie_goes_to_the_first_row() {
        // core and infra are both rank 1; core is written first.
        assert_eq!(
            map().headline(["infra", "core"]).map(|t| t.name.as_str()),
            Some("core")
        );
        // modules and orchestrators are both rank 2.
        assert_eq!(
            map()
                .headline(["orchestrators", "modules"])
                .map(|t| t.name.as_str()),
            Some("modules")
        );
    }

    #[test]
    fn a_map_with_no_rows_or_a_duplicate_name_is_refused() {
        assert!(matches!(parse(""), Err(TiersError::Empty)));
        let dup = "[[tier]]\nname = \"a\"\nrank = 1\npaths = [\"a/\"]\n\
                   [[tier]]\nname = \"a\"\nrank = 2\npaths = [\"b/\"]\n";
        assert!(matches!(parse(dup), Err(TiersError::Duplicate(n)) if n == "a"));
    }
}
