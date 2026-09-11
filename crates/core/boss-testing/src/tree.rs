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

    #[test]
    fn the_live_binary_is_running_in_its_own_tree() {
        // The real function, in the real process: cargo set our cwd to
        // this package's directory, which is inside the compiled root.
        let root = repo_root();
        assert!(root.join("Cargo.toml").is_file(), "{}", root.display());
    }
}
