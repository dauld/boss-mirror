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
    /// An edit level that is not a tier name. The message lists the
    /// names so a tenant.toml author sees the vocabulary, not a guess.
    #[error("edit level `{0}` is not a tier in infra/platform/tiers.toml (one of: {1})")]
    NoSuchLevel(String, String),
}

/// The first path a set of changes touches ABOVE an edit level — the
/// hosting door's one finding (a479faf7, design 01c3cc3f reader 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Above<'a> {
    /// The offending path, as given.
    pub path: &'a str,
    /// Its tier, or `None` when no row claims it (the tree's own root:
    /// `Cargo.toml`, `README.md`, `.forgejo/`).
    pub tier: Option<&'a Tier>,
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

    /// The innermost rank the map declares — 1 today (core, infra).
    /// A level at this rank admits everything, root files included.
    pub fn innermost_rank(&self) -> u8 {
        self.tiers.iter().map(|t| t.rank).min().unwrap_or(u8::MAX)
    }

    /// The tier names, in file order, comma-joined — for a refusal
    /// that has to say what the vocabulary is.
    fn names(&self) -> String {
        self.tiers
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// THE HOSTING PREDICATE (a479faf7; design 01c3cc3f reader 3): a
    /// change is admitted for a tenant whose `edit_level` is `level`
    /// iff every path it touches is in a tier of rank >= the level's
    /// rank. Returns the FIRST path, in the order given, that is not
    /// — its tier beside it — or `Ok(None)` when every path is
    /// admitted. A path no row claims is the tree's own root and is
    /// admitted only by a level at the innermost rank: `Cargo.toml` is
    /// not a tenant's to edit at `tenants`, and `core` (the operator's
    /// own instance) admits it like everything else.
    ///
    /// The level is a TIER NAME (`data`, `tenants`, `modules`, `core`
    /// are the four David named as data-only / tenant / modules /
    /// full; any name in the map is a level, judged by its rank), and
    /// one that is not in the map is an error naming the vocabulary,
    /// never a level that admits nothing. Mirrored in shell by
    /// `edit_level_first_above` in infra/lint/lib/tiers.sh, held equal
    /// to this by boss-testing/tests/tiers_sh.rs.
    pub fn first_above<'a>(
        &'a self,
        level: &str,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<Option<Above<'a>>, TiersError> {
        let floor = self
            .by_name(level)
            .ok_or_else(|| TiersError::NoSuchLevel(level.to_string(), self.names()))?
            .rank;
        let innermost = self.innermost_rank();
        Ok(paths.into_iter().find_map(|path| {
            let tier = self.tier_of(path);
            let admitted = match tier {
                Some(t) => t.rank >= floor,
                None => floor <= innermost,
            };
            (!admitted).then_some(Above { path, tier })
        }))
    }

    /// The refusal a door prints for an `Above`: the path, what it is
    /// (its tier and rank, or that no tier claims it), and the level
    /// with its rank — every fact the author needs to see why, and
    /// the same sentence at the dispatch door and the gate.
    pub fn level_refusal(&self, level: &str, above: &Above<'_>) -> String {
        let rank = self.by_name(level).map(|t| t.rank);
        let level_text = match rank {
            Some(r) => format!("edit level `{level}` (rank {r})"),
            None => format!("edit level `{level}`"),
        };
        match above.tier {
            Some(t) => format!(
                "{level_text} does not admit `{}` — it is `{}` (rank {}), closer to the core \
                 than the level allows this tenant's changes to reach",
                above.path, t.name, t.rank
            ),
            None => {
                let innermost: Vec<&str> = self
                    .tiers
                    .iter()
                    .filter(|t| t.rank == self.innermost_rank())
                    .map(|t| t.name.as_str())
                    .collect();
                format!(
                    "{level_text} does not admit `{}` — no tier claims it (the tree's own \
                     root), which only the innermost level admits ({})",
                    above.path,
                    innermost
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
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

    // ---- the edit level (a479faf7, design 01c3cc3f reader 3) --------------

    fn above(level: &str, paths: &[&str]) -> Option<String> {
        map()
            .first_above(level, paths.iter().copied())
            .expect("a level the map names")
            .map(|a| a.path.to_string())
    }

    #[test]
    fn each_level_admits_its_own_rank_and_outward_and_refuses_the_first_path_inward() {
        // One representative path per tier: core, infra (rank 1),
        // modules, orchestrators (2), tenants, frontend (3), data (4).
        let core = "crates/core/boss-a/src/lib.rs";
        let infra = "infra/lint/a-lint.sh";
        let modules = "crates/modules/boss-b/src/lib.rs";
        let orchestrators = "crates/orchestrators/boss-c/src/main.rs";
        let tenants = "crates/tenants/boss-acme-engine/src/main.rs";
        let frontend = "apps/web/src/a-page/page.ts";
        let data = "infra/platform/workflows/a-protocol.toml";

        // data: only data.
        assert_eq!(above("data", &[data]), None);
        for p in [core, infra, modules, orchestrators, tenants, frontend] {
            assert_eq!(above("data", &[data, p]), Some(p.to_string()), "{p}");
        }
        // tenants: tenants, frontend, data.
        assert_eq!(above("tenants", &[tenants, frontend, data]), None);
        for p in [core, infra, modules, orchestrators] {
            assert_eq!(above("tenants", &[data, p]), Some(p.to_string()), "{p}");
        }
        // modules: modules, orchestrators and outward.
        assert_eq!(
            above(
                "modules",
                &[modules, orchestrators, tenants, frontend, data]
            ),
            None
        );
        for p in [core, infra] {
            assert_eq!(above("modules", &[tenants, p]), Some(p.to_string()), "{p}");
        }
        // core: everything.
        assert_eq!(
            above(
                "core",
                &[core, infra, modules, orchestrators, tenants, frontend, data]
            ),
            None
        );
    }

    #[test]
    fn the_first_offending_path_in_the_order_given_is_the_one_named() {
        let paths = [
            "docs/a.md",
            "crates/modules/boss-b/x.rs",
            "crates/core/boss-a/x.rs",
        ];
        let a = map().first_above("tenants", paths).unwrap().unwrap();
        assert_eq!(a.path, "crates/modules/boss-b/x.rs");
        assert_eq!(a.tier.map(|t| t.name.as_str()), Some("modules"));
    }

    #[test]
    fn a_path_no_tier_claims_is_admitted_only_by_the_innermost_level() {
        // The tree's own root files: Cargo.toml, README.md, .forgejo/.
        assert_eq!(above("core", &["Cargo.toml"]), None);
        assert_eq!(above("infra", &["Cargo.toml"]), None);
        for level in ["modules", "tenants", "data"] {
            assert_eq!(
                above(level, &["Cargo.toml"]),
                Some("Cargo.toml".into()),
                "{level}"
            );
        }
        let a = map().first_above("data", ["README.md"]).unwrap().unwrap();
        assert!(a.tier.is_none());
    }

    #[test]
    fn no_paths_is_admitted_at_every_level() {
        for t in &map().tiers {
            assert_eq!(above(&t.name, &[]), None, "{}", t.name);
        }
    }

    #[test]
    fn a_level_that_is_not_a_tier_is_refused_by_name() {
        let err = map()
            .first_above("full", ["docs/a.md"])
            .expect_err("full is not a tier name");
        assert!(
            matches!(&err, TiersError::NoSuchLevel(n, _) if n == "full"),
            "{err}"
        );
        assert!(err.to_string().contains("core"), "{err}");
    }

    #[test]
    fn the_refusal_names_the_path_its_tier_and_the_level_with_both_ranks() {
        let a = map()
            .first_above("tenants", ["crates/core/boss-a/x.rs"])
            .unwrap()
            .unwrap();
        let text = map().level_refusal("tenants", &a);
        assert!(text.contains("crates/core/boss-a/x.rs"), "{text}");
        assert!(text.contains("`core` (rank 1)"), "{text}");
        assert!(text.contains("edit level `tenants` (rank 3)"), "{text}");
        let a = map().first_above("data", ["Cargo.toml"]).unwrap().unwrap();
        let text = map().level_refusal("data", &a);
        assert!(text.contains("no tier claims it"), "{text}");
        assert!(text.contains("`core`"), "{text}");
    }

    #[test]
    fn a_map_with_no_rows_or_a_duplicate_name_is_refused() {
        assert!(matches!(parse(""), Err(TiersError::Empty)));
        let dup = "[[tier]]\nname = \"a\"\nrank = 1\npaths = [\"a/\"]\n\
                   [[tier]]\nname = \"a\"\nrank = 2\npaths = [\"b/\"]\n";
        assert!(matches!(parse(dup), Err(TiersError::Duplicate(n)) if n == "a"));
    }
}
