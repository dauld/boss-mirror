//! infra/lint/lib/tiers.sh — the shell reader of infra/platform/tiers.toml
//! — is held equal to the Rust reader `boss_core::tiers` over one fixture
//! of paths (design 01c3cc3f, 2026-09-19; CLAUDE.md §9a: a pin, kept
//! because a shell lint cannot call Rust). Every case the Rust unit tests
//! pin — a crate directory, the most specific prefix (data over infra),
//! the `**/seeds/` glob at any depth and NOT as a file name, a path no
//! row claims — is asked of both readers here, and a disagreement names
//! the path.
//!
//! The second half pins the first consumer: `tier-import-audit.sh` reads
//! its roots from the map through the lib, and its verdict on the tree
//! and on a synthetic violation is what it was when the prefixes were
//! its own text.

use boss_testing::{feed_stdin, repo_root, scratch_dir};
use std::path::Path;
use std::process::Command;

/// The paths both readers are asked about. Kept as one list so the pin
/// cannot quietly ask the shell fewer questions than the Rust. None of
/// them names a REAL file on purpose: gate.sh derives a crate's inputs
/// from the path literals in its tests/, so a real path here would make
/// every edit to that file gate boss-testing (its scope self-test
/// refused exactly that when this list first named two registry rows).
const FIXTURE: &[&str] = &[
    "crates/core/boss-a/src/lib.rs",
    "crates/modules/boss-b/Cargo.toml",
    "crates/orchestrators/boss-c/src/x.rs",
    "crates/tenants/boss-acme-engine/src/main.rs",
    "examples/acme/DOMAIN.md",
    "examples/acme/seeds/tenant.toml",
    "seeds/x.sql",
    "crates/core/boss-a/src/seeds.rs",
    "apps/web/src/a-page/page.ts",
    "libs/web-kit/src/index.ts",
    "infra/lint/gate.sh",
    "infra/platform/a-registry.toml",
    "infra/platform/workflows/a-protocol.toml",
    "infra/dispatcher/rules/a-rule.toml",
    "infra/dispatcher/README.md",
    "docs/design/x.md",
    "README.md",
    ".forgejo/workflows/a-workflow.yml",
    "Cargo.toml",
    "./apps/web/x.ts",
];

/// `tier_of_path` through the lib, as a lint would call it: the tier
/// name, or `None` when the lib exits non-zero.
fn shell_tier_of(path: &str) -> Option<String> {
    let lib = repo_root().join("infra/lint/lib/tiers.sh");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}' || exit 3\ntier_of_path \"$1\"",
            lib.display()
        ))
        .arg("tiers_sh")
        .arg(path)
        .output()
        .expect("bash runs");
    assert_ne!(
        out.status.code(),
        Some(3),
        "the lib could not be read: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[test]
fn the_shell_reader_answers_every_fixture_path_as_the_rust_reader_does() {
    let mut disagreements = Vec::new();
    for path in FIXTURE {
        let rust = boss_core::tiers::tier_of(path).map(|t| t.name.clone());
        let shell = shell_tier_of(path);
        if rust != shell {
            disagreements.push(format!("{path}: rust={rust:?} shell={shell:?}"));
        }
    }
    assert!(
        disagreements.is_empty(),
        "infra/lint/lib/tiers.sh disagrees with boss_core::tiers:\n  {}",
        disagreements.join("\n  ")
    );
    // The fixture must exercise both answers, or an all-None pair
    // would pass as agreement.
    assert!(
        FIXTURE
            .iter()
            .any(|p| boss_core::tiers::tier_of(p).is_some())
    );
    assert!(
        FIXTURE
            .iter()
            .any(|p| boss_core::tiers::tier_of(p).is_none())
    );
}

#[test]
fn the_shell_reader_lists_a_tiers_prefixes_and_rank_from_the_same_file() {
    let map = boss_core::tiers::tier_map().expect("the embedded map parses");
    let lib = repo_root().join("infra/lint/lib/tiers.sh");
    for tier in &map.tiers {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". '{}' || exit 3\ntier_paths \"$1\"; echo \"rank=$(tier_rank \"$1\")\"",
                lib.display()
            ))
            .arg("tiers_sh")
            .arg(&tier.name)
            .output()
            .expect("bash runs");
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines: Vec<&str> = text.lines().collect();
        let rank = lines.pop().unwrap_or_default();
        assert_eq!(lines, tier.paths, "prefixes of `{}`", tier.name);
        assert_eq!(
            rank,
            format!("rank={}", tier.rank),
            "rank of `{}`",
            tier.name
        );
    }
}

// ---- the third reader: the edit level's predicate, in both languages ----

/// `edit_level_first_above <level>` through the lib, paths on stdin,
/// as the gate's lint calls it: `Ok(Some(path))` for the first path
/// above the level, `Ok(None)` when every path is admitted, `Err` when
/// the lib refused the level (exit 3, the CANNOT ANSWER vocabulary).
fn shell_first_above(level: &str, paths: &[&str]) -> Result<Option<String>, String> {
    use std::process::Stdio;
    let lib = repo_root().join("infra/lint/lib/tiers.sh");
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}' || exit 3\nedit_level_first_above \"$1\"",
            lib.display()
        ))
        .arg("tiers_sh")
        .arg(level)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash runs");
    // A refused level exits 3 before it reads a path, so the write races
    // the exit; a closed pipe is the child's verdict, read below from its
    // status (boss_testing::feed_stdin; train 11:23, backlog fec29a02).
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    feed_stdin(&mut child, input.as_bytes());
    let out = child.wait_with_output().expect("bash finishes");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match out.status.code() {
        Some(0) => Ok(None),
        Some(1) => Ok(Some(
            stdout.split('\t').next().unwrap_or_default().to_string(),
        )),
        other => Err(format!(
            "exit {other:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// Every level in the map, against the whole fixture in one pass (the
/// first path above wins) and against each path alone — so the pin
/// asks the shell the same per-path question the Rust unit tests
/// answer, at every rank, for a path in every tier and for the tree's
/// own root. A disagreement names the level and the path.
#[test]
fn the_shell_predicate_names_the_same_first_path_above_every_level_as_the_rust_one() {
    let map = boss_core::tiers::tier_map().expect("the embedded map parses");
    let mut disagreements = Vec::new();
    let mut refusals = 0usize;
    for tier in &map.tiers {
        let level = tier.name.as_str();
        let mut cases: Vec<Vec<&str>> = FIXTURE.iter().map(|p| vec![*p]).collect();
        cases.push(FIXTURE.to_vec());
        cases.push(FIXTURE.iter().rev().copied().collect());
        for paths in cases {
            let rust = map
                .first_above(level, paths.iter().copied())
                .expect("a tier name is a level")
                .map(|a| a.path.to_string());
            let shell = shell_first_above(level, &paths)
                .unwrap_or_else(|e| panic!("level {level}: the lib refused: {e}"));
            if rust != shell {
                disagreements.push(format!(
                    "{level} over {paths:?}: rust={rust:?} shell={shell:?}"
                ));
            }
            refusals += usize::from(rust.is_some());
        }
    }
    assert!(
        disagreements.is_empty(),
        "infra/lint/lib/tiers.sh edit_level_first_above disagrees with boss_core::tiers:\n  {}",
        disagreements.join("\n  ")
    );
    // The fixture must produce both answers at more than one level, or
    // an always-admit pair would pass as agreement.
    assert!(refusals > 10, "only {refusals} refusals across the levels");
}

#[test]
fn the_shell_predicate_refuses_a_level_that_is_not_a_tier_with_exit_3() {
    let err = shell_first_above("full", &["docs/a.md"]).expect_err("full is not a tier");
    assert!(err.starts_with("exit Some(3)"), "{err}");
    assert!(err.contains("full") && err.contains("core"), "{err}");
}

/// The same refusal, made deterministic: the lib exits 3 without reading
/// stdin, so a write the pipe buffer cannot hold MUST meet a closed pipe.
/// With one short path the write usually lands first — train 11:23 went
/// red the one time it did not (gate-run 10cdfa86, backlog fec29a02).
/// The closed pipe is the refusal's own shape, and the exit code and the
/// message stay the verdict.
#[test]
fn a_refusal_that_exits_before_reading_its_paths_is_still_exit_3() {
    let many: Vec<String> = (0..20_000)
        .map(|i| format!("docs/a-path-long-enough-to-fill-the-pipe-{i}.md"))
        .collect();
    let paths: Vec<&str> = many.iter().map(String::as_str).collect();
    let err = shell_first_above("full", &paths).expect_err("full is not a tier");
    assert!(err.starts_with("exit Some(3)"), "{err}");
    assert!(err.contains("full") && err.contains("core"), "{err}");
}

#[test]
fn the_shell_reader_answers_the_innermost_rank_from_the_same_file() {
    let map = boss_core::tiers::tier_map().expect("the embedded map parses");
    let lib = repo_root().join("infra/lint/lib/tiers.sh");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}' || exit 3\ntier_innermost_rank",
            lib.display()
        ))
        .output()
        .expect("bash runs");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        map.innermost_rank().to_string()
    );
}

// ---- the first consumer: tier-import-audit reads its roots from the map ----

const AUDIT: &str = "infra/lint/tier-import-audit.sh";

#[test]
fn tier_import_audit_sources_the_lib_and_carries_no_prefix_of_its_own() {
    let text = std::fs::read_to_string(repo_root().join(AUDIT)).expect("the lint reads");
    assert!(
        text.contains("lib/tiers.sh"),
        "tier-import-audit must read its roots through infra/lint/lib/tiers.sh"
    );
    let code: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    for literal in [
        "crates/core",
        "modules|tenants",
        "\"../../modules",
        "\"../../tenants",
    ] {
        assert!(
            !code.iter().any(|l| l.contains(literal)),
            "tier-import-audit still carries `{literal}` as its own text — the map is the definition"
        );
    }
}

#[test]
fn tier_import_audit_is_clean_on_the_tree_and_scans_every_core_crate() {
    let out = Command::new("bash")
        .arg(repo_root().join(AUDIT))
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "tier-import-audit on the tree: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let core_crates = walk_cargo_tomls(&repo_root().join("crates/core"));
    assert!(core_crates > 0);
    assert!(
        stdout.contains(&format!(
            "tier-import-audit: scanned {core_crates} core crate(s)"
        )),
        "the scanned line must count every crates/core Cargo.toml ({core_crates}): {stdout}"
    );
    assert!(stdout.contains("tier-import-audit: clean"), "{stdout}");
}

/// A synthetic tree: one core crate, one module crate, the core crate
/// depending on the module by path — the exact edge the audit exists
/// to refuse. Run from a copy of the lint (it cds to its own ../..),
/// with the lib and the REAL map beside it.
#[test]
fn tier_import_audit_still_refuses_a_core_crate_that_depends_on_a_module() {
    let tree = scratch_dir("tier-import-audit-fixture");
    let _ = std::fs::remove_dir_all(&tree);
    boss_testing::copy_lint_libs(&tree);
    std::fs::create_dir_all(tree.join("infra/platform")).unwrap();
    std::fs::copy(
        repo_root().join("infra/platform/tiers.toml"),
        tree.join("infra/platform/tiers.toml"),
    )
    .unwrap();
    std::fs::copy(repo_root().join(AUDIT), tree.join(AUDIT)).unwrap();
    write(
        &tree.join("crates/core/boss-x/Cargo.toml"),
        "[package]\nname = \"boss-x\"\n[dependencies]\nboss-people = { path = \"../../modules/boss-people\" }\n",
    );
    write(
        &tree.join("crates/core/boss-y/Cargo.toml"),
        "[package]\nname = \"boss-y\"\n[dependencies]\nboss-core = { path = \"../boss-core\" }\n",
    );
    write(
        &tree.join("crates/modules/boss-people/Cargo.toml"),
        "[package]\nname = \"boss-people\"\n",
    );
    let out = Command::new("bash")
        .arg(tree.join(AUDIT))
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a core->module edge is a violation: {stdout}"
    );
    assert!(
        stdout.contains("VIOLATION: core crate \"boss-x\" depends on a non-core crate"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"boss-y\""),
        "a core->core edge is not named: {stdout}"
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn walk_cargo_tomls(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        walk_cargo_tomls(&p)
                    } else {
                        usize::from(p.file_name().is_some_and(|n| n == "Cargo.toml"))
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}
