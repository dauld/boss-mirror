//! The repository root a test binary was COMPILED from — and a refusal
//! when that is not the tree it is running in.
//!
//! A compiled test binary bakes in its own `CARGO_MANIFEST_DIR`. On a
//! box where builders share one `CARGO_TARGET_DIR` (the dev pod does,
//! because a cold target is ~74 GB and the pod OOMs on parallel cold
//! builds), `cargo test` in worktree B can REUSE a binary that worktree
//! A built minutes earlier — and every `repo_root()` inside it then
//! points at A. Measured 2026-09-11 (3d8bb6e6): `gate_sh` built from one
//! agent worktree ran from another and reported a confident failure
//! belonging to someone else's tree; the second worktree's "13/13 green"
//! was evidence about a tree its builder was not working in. Forty
//! minutes, and the wrong-target-answers-instead-of-erroring shape from
//! CLAUDE.md §Doors at its most expensive: well-formed, confident, and
//! about a different tree.
//!
//! Twenty-seven test files carried their own six-line `repo_root()`
//! before this module (CLAUDE.md §9a — a fact that lives twice); none
//! could see the mismatch. This one can, because cargo runs every test
//! binary with the package directory as its working directory, so the
//! running cwd must lie INSIDE the compiled-in root. When it does not,
//! [`repo_root`] refuses with both paths and the remedy, instead of
//! answering with the other tree.

use std::path::{Path, PathBuf};

/// Does the tree this process is running in match the tree the binary
/// was compiled from? Pure so the refusal's shape is testable: `Ok` when
/// `cwd` lies under `compiled_root`; otherwise the message names both
/// paths, the mechanism, and what to do.
pub fn tree_match(compiled_root: &Path, cwd: &Path) -> Result<(), String> {
    if cwd.starts_with(compiled_root) {
        return Ok(());
    }
    Err(format!(
        "this test binary was compiled from {} but is running under {} — a shared \
         CARGO_TARGET_DIR served a binary another worktree built, so every path in it \
         belongs to THAT tree (3d8bb6e6). Rebuild for this tree (touch a source file, or \
         set a per-worktree CARGO_TARGET_DIR) rather than reading its answer.",
        compiled_root.display(),
        cwd.display()
    ))
}

/// The repository root this binary was compiled from, refusing when the
/// process is not running inside it. Replaces the per-file copies in
/// `tests/` (pinned by `no_test_file_defines_its_own_repo_root`).
pub fn repo_root() -> PathBuf {
    let compiled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves");
    let cwd = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .expect("cwd resolves");
    if let Err(why) = tree_match(&compiled, &cwd) {
        panic!("{why}");
    }
    compiled
}

/// The authored dispatcher-rule registry, `infra/dispatcher/rules/` —
/// the directory the dispatcher seeds `dispatcher_rules` from at boot
/// (`boss_dispatcher::rules::seed`, backlog 41ba00cd) — under
/// [`repo_root`], so it inherits the wrong-tree refusal.
///
/// One definition of where the rules live. Until 2026-09-14 seven test
/// files across boss-dispatcher and boss-dispatcher-handlers each
/// spelled `concat!(env!("CARGO_MANIFEST_DIR"), "/../../../infra/
/// dispatcher/rules")` (backlog 94f150f9; CLAUDE.md §9a) — a path that
/// answers about the tree the binary was COMPILED from, without the
/// check above. It lives here rather than in either crate's test
/// support because both crates already depend on this one and neither
/// can reach the other's `tests/common`.
pub fn dispatcher_rules_dir() -> PathBuf {
    repo_root().join("infra/dispatcher/rules")
}

/// Copy the shared lint library — every file under `infra/lint/lib/` —
/// into a fixture tree at `<fixture>/infra/lint/lib/`, so a lint copied
/// into that fixture finds what it sources the way it does in the repo.
///
/// One definition of "what a copied lint needs beside it". Until
/// 2026-09-18 each fixture test copied the one or two libs its lint
/// happened to source, and adding a sourced helper to a lint
/// (`lib/scanned.sh`, backlog cdf2d959) broke four fixtures at once:
/// the copied lint ran, failed to source, and the failure read as a
/// verdict about the fixture. The directory is the definition
/// (CLAUDE.md §9a), so a lib added tomorrow is carried without this
/// function changing.
pub fn copy_lint_libs(fixture: &Path) {
    let src = repo_root().join("infra/lint/lib");
    let dst = fixture.join("infra/lint/lib");
    std::fs::create_dir_all(&dst).unwrap_or_else(|e| panic!("create {}: {e}", dst.display()));
    let entries = std::fs::read_dir(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));
    let mut copied = 0;
    for entry in entries {
        let path = entry.expect("a lib entry").path();
        if path.extension().is_some_and(|e| e == "sh") {
            let to = dst.join(path.file_name().expect("a file name"));
            std::fs::copy(&path, &to)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", path.display(), to.display()));
            copied += 1;
        }
    }
    assert!(copied > 0, "{} holds no .sh to copy", src.display());
}

/// The tunnel's ingress map as one line — `infra/cluster/
/// render-tunnel-config.sh --summary`, run against THIS tree, with
/// `skipped` handed to it as the converge hands its own
/// `instances_skipped` field.
///
/// One definition of what the converge records as `tunnel_ingress`.
/// Until 2026-09-22 the rendered line was spelled as a literal in four
/// assertions across two test files — `the_tunnel_connector_runs_in_
/// the_cluster.rs` and `every_converge_tick_observes_the_connector.rs`
/// — so adding one row to `infra/cluster/instances.toml` churned all of
/// them (backlog e9423392; CLAUDE.md §9a). The renderer already
/// answered the question, and these four are CONSUMERS: they assert the
/// runner carried the renderer's line through into the packet, which is
/// exactly what deriving it here states. The two assertions that pin
/// what the renderer itself PRINTS stay literal in that file — a
/// definition derived from its own subject would prove nothing — and
/// `the_tunnel_ingress_summary_is_spelled_only_where_it_is_defined`
/// below holds the split.
pub fn tunnel_ingress_summary(skipped: &str) -> String {
    let root = repo_root();
    let script = root.join("infra/cluster/render-tunnel-config.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("--summary")
        .current_dir(&root)
        .env("BOSS_CLUSTER_TREE", &root)
        .env("BOSS_INSTANCES_SKIPPED", skipped)
        .output()
        .unwrap_or_else(|e| panic!("run {} --summary: {e}", script.display()));
    assert!(
        out.status.success(),
        "{} --summary refused ({:?}):\n{}",
        script.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        !summary.is_empty(),
        "{} --summary printed nothing — an empty expectation matches every file",
        script.display()
    );
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cwd_inside_the_compiled_tree_matches() {
        let root = Path::new("/work/boss");
        assert_eq!(tree_match(root, Path::new("/work/boss")), Ok(()));
        assert_eq!(
            tree_match(root, Path::new("/work/boss/crates/core/boss-testing")),
            Ok(())
        );
    }

    #[test]
    fn another_worktree_is_refused_naming_both_trees_and_the_remedy() {
        let err = tree_match(
            Path::new("/work/boss/.claude/worktrees/agent-a"),
            Path::new("/work/boss/.claude/worktrees/agent-b/crates/core/boss-testing"),
        )
        .expect_err("a different tree must refuse");
        for must in [
            "/work/boss/.claude/worktrees/agent-a",
            "/work/boss/.claude/worktrees/agent-b",
            "CARGO_TARGET_DIR",
            "3d8bb6e6",
        ] {
            assert!(err.contains(must), "refusal must name {must}: {err}");
        }
    }

    #[test]
    fn a_sibling_with_a_common_prefix_is_not_inside() {
        // starts_with is by path COMPONENT, so /work/boss-2 is not under /work/boss.
        assert!(tree_match(Path::new("/work/boss"), Path::new("/work/boss-2")).is_err());
    }

    /// The collapse holds: no integration test grows its own copy back
    /// (CLAUDE.md §9a — the twenty-seven this replaced could not see the
    /// mismatch, and a twenty-eighth would not either).
    #[test]
    fn no_test_file_defines_its_own_repo_root() {
        let tests = repo_root().join("crates/core/boss-testing/tests");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&tests).expect("tests dir") {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "rs") {
                let src = std::fs::read_to_string(&path).expect("read");
                if src.lines().any(|l| l.starts_with("fn repo_root(")) {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these test files define their own repo_root() instead of using \
             boss_testing::repo_root, which refuses a binary from another tree: {offenders:?}"
        );
    }

    /// The rules directory is the authored registry — one file per rule
    /// plus its README — resolved under the SAME root, so it carries the
    /// same wrong-tree refusal rather than a second, unguarded path.
    #[test]
    fn the_dispatcher_rules_dir_is_the_authored_registry_under_this_root() {
        let dir = dispatcher_rules_dir();
        assert_eq!(dir, repo_root().join("infra/dispatcher/rules"));
        assert!(dir.join("README.md").is_file(), "{}", dir.display());
        let rule_files = std::fs::read_dir(&dir)
            .expect("read the rules dir")
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "toml"))
            })
            .count();
        assert!(rule_files > 0, "no rule files under {}", dir.display());
    }

    /// The tunnel ingress summary is spelled once — in the file that
    /// asserts what the renderer PRINTS — and derived everywhere else.
    /// The collapse holds only while nothing grows a copy back, and a
    /// copy is what made one added instance row churn four assertions
    /// (backlog e9423392).
    #[test]
    fn the_tunnel_ingress_summary_is_spelled_only_where_it_is_defined() {
        // Where `render-tunnel-config.sh --summary` itself is under
        // test: that file states the renderer's output, so it spells it.
        let defining = "the_tunnel_connector_runs_in_the_cluster.rs";
        let summaries = [
            tunnel_ingress_summary(""),
            tunnel_ingress_summary("boss-playground (secrets absent: boss-secrets)"),
        ];
        assert_ne!(
            summaries[0], summaries[1],
            "a skipped instance must change the line, or this pin reads one fact as two"
        );
        let tests = repo_root().join("crates/core/boss-testing/tests");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&tests).expect("tests dir") {
            let path = entry.expect("entry").path();
            if path.file_name().is_some_and(|n| n == defining) {
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read");
            if summaries.iter().any(|s| src.contains(s.as_str())) {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these files spell the rendered ingress summary instead of calling \
             boss_testing::tunnel_ingress_summary, so one added instance row churns \
             every one of them: {offenders:?}"
        );

        // And in the defining file, ONCE per variant: that one is the
        // assertion about what the renderer prints. A second spelling
        // there is a consumer, and a consumer derives.
        let src = std::fs::read_to_string(tests.join(defining)).expect("read the defining file");
        for summary in &summaries {
            assert_eq!(
                src.matches(summary.as_str()).count(),
                1,
                "{defining} states the renderer's own output once per variant; \
                 every other assertion about it derives: {summary}"
            );
        }
    }

    /// The two shapes the converge records, from the renderer itself:
    /// an applied instance routes to its own namespace, a skipped one to
    /// its source's gateway with the reason in parentheses.
    #[test]
    fn the_summary_names_the_instance_that_serves_each_hostname() {
        let applied = tunnel_ingress_summary("");
        let skipped = tunnel_ingress_summary("boss-playground (secrets absent: boss-secrets)");
        assert!(applied.contains(" \u{2192} "), "{applied}");
        assert!(
            skipped.contains("(boss-playground skipped: secrets absent)"),
            "a skipped instance must carry its reason: {skipped}"
        );
        assert!(
            !applied.contains("skipped"),
            "with nothing skipped the line says so: {applied}"
        );
    }

    #[test]
    fn the_live_binary_is_running_in_its_own_tree() {
        // The real function, in the real process: cargo set our cwd to
        // this package's directory, which is inside the compiled root.
        let root = repo_root();
        assert!(root.join("Cargo.toml").is_file(), "{}", root.display());
    }
}
