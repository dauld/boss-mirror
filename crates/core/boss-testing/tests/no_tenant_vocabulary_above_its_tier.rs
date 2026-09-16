//! `infra/lint/no-tenant-vocabulary-above-its-tier.sh` is RUN, not read
//! — against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog be39298f). The tier rule — no tenant-specific
//! assumptions in BOSS core (CLAUDE.md §10) — was enforced only as a
//! Cargo dependency audit. Nothing counted tenant VOCABULARY, so on
//! 2026-09-16 a grep for the brewery's words outside the brewery found
//! 362 files across every tier above it: kegs in core registries, excise
//! in module ledgers, the brewery simulator in an orchestrator crate.
//! Each concentration is its own car; this lint is the SENSOR with a
//! ratchet that stops the number rising while those cars land, and holds
//! it at zero afterwards.
//!
//! THE SHAPE. The word list is the tenant's own (`examples/<tenant>/
//! VOCABULARY`), the count is derived from the tree on every run, and the
//! baseline (`infra/lint/tenant-vocabulary.baseline`, one line per tier)
//! may only be REWRITTEN DOWN, in the same car that lowers the count
//! (CLAUDE.md §9a). So there are four verdicts on a tier, and this file
//! owns all four: above the baseline is refused naming the file; equal
//! is clean; below is refused too, telling the author to lower the
//! baseline — which is also what stops anyone raising it.
//!
//! Fixtures live under `boss_testing::scratch`, which carries the uid
//! and the pid. The fixture vocabulary is invented (`wibble`, `flurble`)
//! so nothing here spells a real tenant's word: a test that carried one
//! would be one more leak for the lint it proves to count, were test
//! files not excluded — and the exclusion is one of the properties
//! pinned below, so the fixture must not depend on it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/no-tenant-vocabulary-above-its-tier.sh";
const BASELINE: &str = "infra/lint/tenant-vocabulary.baseline";

/// The eight tier roots the lint scans, spelled here so the fixture
/// can create them all: the lint refuses a tree missing one, because a
/// wrong path answers 0 instead of erroring (CLAUDE.md §Doors).
const TIERS: [&str; 8] = [
    "crates/core",
    "crates/modules",
    "crates/orchestrators",
    "apps/web/src",
    "apps/simulator",
    "infra/postgres/schema",
    "infra/platform",
    "infra/cluster",
];

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: the lint itself
/// at the path its own `cd "$(dirname $0)/../.."` resolves from, every
/// tier root, one tenant's vocabulary, and a baseline the test writes.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("tenant-vocabulary-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        for tier in TIERS {
            scratch::create_dir(&root.join(tier));
        }
        let body = std::fs::read_to_string(lint()).expect("read the lint under test");
        scratch::write_exec(&root.join(LINT), &body);
        Tree(root)
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
        self
    }

    /// A tenant's word list: `wibble*` is a word-start prefix, `flurble`
    /// a whole word — both spellings the real files use. The tenant is
    /// NAMED for its first term (`examples/wibble`), the way the brewery
    /// is, so the name-as-path and name-as-id exemptions can be pinned.
    fn vocabulary(&self) -> &Tree {
        self.file(
            "examples/wibble/VOCABULARY",
            "# why: the fixture tenant's words\nwibble*\nflurble\n",
        )
    }

    /// A baseline with every tier at `n` except `crates/core` at `core`.
    fn baseline(&self, core: usize, n: usize) -> &Tree {
        let mut body = String::from("# fixture baseline\n");
        for tier in TIERS {
            let count = if tier == "crates/core" { core } else { n };
            body.push_str(&format!("{tier} {count}\n"));
        }
        self.file(BASELINE, &body)
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// ABOVE the baseline — the leak grew — is refused, and the refusal
/// names the tier, the file, and the number, so nobody re-derives them
/// (CLAUDE.md §Diagnosis).
#[test]
fn a_tier_above_its_baseline_is_refused_naming_the_file() {
    let tree = Tree::new("above");
    tree.vocabulary().baseline(1, 0).file(
        "crates/core/boss-thing/src/lib.rs",
        "// a Wibbler and a wibble: two hits\nfn flurble() {}\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "three hits over a baseline of one must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "crates/core",
        "crates/core/boss-thing/src/lib.rs",
        "3",
        "baseline",
    ] {
        assert!(
            msg.contains(expect),
            "the refusal must name {expect:?}:\n{msg}"
        );
    }
}

/// EQUAL to the baseline is clean — and the count is case-insensitive,
/// occurrence-based (two on one line are two), and whole-word for a bare
/// term: `flurbles` is not `flurble`, while `Wibbling` IS `wibble*`.
#[test]
fn a_tier_at_its_baseline_is_clean() {
    let tree = Tree::new("equal");
    tree.vocabulary().baseline(3, 0).file(
        "crates/core/boss-thing/src/lib.rs",
        "// Wibbling wibble — two; flurbles is not flurble, so one more\nfn flurble() {}\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a count equal to its baseline must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("clean"),
        "a pass must say so:\n{}",
        text(&out)
    );
}

/// BELOW the baseline is refused too, and the refusal is an
/// instruction: lower the baseline in this car. This is the whole
/// ratchet — a baseline that may sit above the count is a baseline
/// anyone can raise, and the number would stop meaning anything.
#[test]
fn a_tier_below_its_baseline_is_told_to_lower_it() {
    let tree = Tree::new("below");
    tree.vocabulary()
        .baseline(5, 0)
        .file("crates/core/boss-thing/src/lib.rs", "fn wibble() {}\n");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a baseline above the count must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("lower")
            && msg.contains("crates/core")
            && msg.contains("5")
            && msg.contains("1"),
        "the refusal must tell the author to LOWER the tier's line and \
         name both numbers:\n{msg}"
    );
}

/// A tenant's NAME used as a path or as an id is not vocabulary
/// leaking: the product must be able to say which tenant an instance
/// runs. The two forms are `examples/<tenant>` and
/// `tenant_id = "<tenant>"` — the shapes infra/cluster/instances.toml
/// and the tenant-contract table landed with on 2026-09-16 — and only
/// those: the same name as a bare word on the same line still counts.
#[test]
fn a_tenant_name_as_a_path_or_an_id_is_not_vocabulary() {
    let tree = Tree::new("name-as-path-or-id");
    tree.vocabulary().baseline(1, 0).file(
        "crates/core/boss-thing/src/lib.rs",
        "\
// tenant = \"examples/wibble/seeds/tenant.toml\" names a path, not a word
// tenant_id = \"wibble\" names an id; TENANT_ID = \"Wibble\" too
// /opt/boss/examples/wibble/data is the same path form, deeper
// but a plain wibble here is the one hit that counts
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "three name-as-path/id forms and one word must count exactly one; \
         got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );

    // And the exemption does not reach past its two forms: the name as a
    // bare word, or as a different key's value, still counts.
    let tree = Tree::new("name-as-word");
    tree.vocabulary().baseline(0, 0).file(
        "crates/core/boss-thing/src/lib.rs",
        "// kind = \"wibble\" is not the tenant_id key, and Wibble alone is a word\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "the name outside the two forms must still count:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("crates/core/boss-thing/src/lib.rs"),
        "and be named:\n{}",
        text(&out)
    );
}

/// Test files are not counted. `#[cfg(test)]` blocks inside a source
/// file ARE — the lint says so in its header — so the exclusion is by
/// path only, and this pins the shapes it recognises.
#[test]
fn test_files_are_not_counted() {
    let tree = Tree::new("tests");
    tree.vocabulary()
        .baseline(0, 0)
        .file("crates/core/boss-thing/tests/a.rs", "fn wibble() {}\n")
        .file("crates/core/boss-thing/src/a_test.rs", "fn wibble() {}\n")
        .file("apps/web/src/a/a.test.ts", "const flurble = 1;\n")
        .file("apps/web/src/a/a.spec.ts", "const flurble = 1;\n");
    let out = tree.run();
    assert!(
        out.status.success(),
        "a term in a test file must not count; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// A tree with no tenant vocabulary is a wrong path, not a clean tree:
/// zero words would count zero hits everywhere and certify nothing.
#[test]
fn a_tree_with_no_vocabulary_is_refused() {
    let tree = Tree::new("no-vocabulary");
    tree.baseline(0, 0);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "no VOCABULARY file must be a refusal, never clean:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("VOCABULARY"),
        "the refusal must name what is missing:\n{}",
        text(&out)
    );
}

/// The real tree passes: every tier's count equals its baseline line.
/// This is the ratchet on the repository itself — a car that adds a
/// brewery word to core, or removes one without lowering the baseline,
/// reds here before it reds a train.
#[test]
fn the_repository_itself_is_at_its_baseline() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint against the repository");
    assert!(
        out.status.success(),
        "every tier must sit exactly at its baseline; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}
