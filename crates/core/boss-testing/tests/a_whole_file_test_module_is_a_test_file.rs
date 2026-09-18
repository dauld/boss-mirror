//! `infra/lint/lib/test-file.sh` — the ONE answer to "is this path a
//! test file" that `no-employee-id-literal` and `no-wallclock` share —
//! is RUN against fixture trees, so every property below is one the
//! predicate actually has.
//!
//! WHY (backlog e9c77544, 2026-09-18). Each lint carried its own
//! test-region parser, and both recognised only two shapes: a path
//! under `tests/` and an inline `#[cfg(test)] mod tests { … }`. A test
//! module declared as a whole FILE — `#[cfg(test)] mod tests;` in the
//! parent, the tests in `tests.rs` beside it — was read as production by
//! both, so the H1 builder kept conductor.rs's tests inline to stay
//! green: the lint dictating file layout. The file-level shape is the
//! same declaration as the inline one with the body moved out, and the
//! predicate reads it where it sits — in the parent.
//!
//! This file pins the predicate itself and the no-wallclock lint's
//! `--self-test`, which drives it through `is_allowed`; the
//! no-employee-id-literal half is pinned on a synthetic tree in
//! `no_employee_id_literal_sh.rs`, next to that lint's other verdicts.
//! (no-wallclock cannot run on a synthetic tree: its allowlist demands
//! every entry exist and excuse a hit, which only the real tree does.)

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::Command;

const LIB: &str = "infra/lint/lib/test-file.sh";

/// A fixture tree with the helper beside it, and the verdict the
/// predicate gives on one relative path inside it.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("test-file-{tag}"));
        scratch::create_dir(&root.join("infra/lint/lib"));
        let body = std::fs::read_to_string(repo_root().join(LIB))
            .unwrap_or_else(|e| panic!("read {LIB}: {e}"));
        scratch::write_file(&root.join(LIB), &body);
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

    /// `is_test_file <rel>`, run from the tree root under the same
    /// `set -euo pipefail` no-wallclock sources it under.
    fn is_test_file(&self, rel: &str) -> bool {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -euo pipefail; . {LIB}; if is_test_file '{rel}'; then echo yes; else echo no; fi"
            ))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the predicate in {}: {e}", self.0.display()));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the predicate must answer, never die, on {rel}: {}{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
        match stdout.trim() {
            "yes" => true,
            "no" => false,
            other => panic!("the predicate printed {other:?} for {rel}"),
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The two shapes the packet names, and the one every lint already had:
/// `<dir>/<name>.rs` and `<dir>/<name>/mod.rs` declared by the parent
/// under `#[cfg(test)]` (the attribute on its own line or on the
/// declaration's, `pub(crate)` or bare), and a path under `tests/`.
#[test]
fn a_file_the_parent_declares_under_cfg_test_is_a_test_file() {
    let t = Tree::new("declared");
    t.file("src/lib.rs", "pub fn f() {}\n#[cfg(test)]\nmod tests;\n")
        .file("src/tests.rs", "fn t() {}\n")
        // The parent is `<dir>.rs`, the module a directory beside it.
        .file(
            "src/conductor.rs",
            "pub fn run() {}\n#[cfg(test)]\npub(crate) mod conductor_tests;\n",
        )
        .file("src/conductor/conductor_tests/mod.rs", "fn t() {}\n")
        // The parent is `mod.rs`, the attribute on the declaration's line.
        .file("src/train/mod.rs", "#[cfg(test)] mod fixtures;\n")
        // A crate root under src/bin declares its siblings.
        .file(
            "src/bin/sim.rs",
            "#[cfg(test)]\n#[allow(dead_code)]\nmod sim_tests;\n",
        )
        .file("src/bin/sim_tests.rs", "fn t() {}\n")
        // A submodule of a whole-file test module is a test file too.
        .file("src/train/fixtures.rs", "mod deep;\nfn t() {}\n")
        .file("src/train/fixtures/deep.rs", "fn t() {}\n")
        .file("tests/anything.rs", "fn t() {}\n");
    for rel in [
        "src/tests.rs",
        "src/conductor/conductor_tests/mod.rs",
        "src/train/fixtures.rs",
        "src/bin/sim_tests.rs",
        "src/train/fixtures/deep.rs",
        "tests/anything.rs",
    ] {
        assert!(t.is_test_file(rel), "{rel} must be a test file");
    }
}

/// The declaration is the fact, not the name: a `tests.rs` nobody
/// declares, one declared WITHOUT `#[cfg(test)]`, the parent that does
/// the declaring, and an inline `#[cfg(test)] mod tests { … }` (which is
/// the lints' own region parser's business, not this predicate's) are
/// each production.
#[test]
fn a_file_no_parent_declares_under_cfg_test_is_production() {
    let t = Tree::new("undeclared");
    t.file(
        "src/lib.rs",
        "mod tests;\n#[cfg(test)]\nmod inline { }\nmod prod;\n",
    )
    .file("src/tests.rs", "fn t() {}\n")
    .file("src/prod.rs", "fn p() {}\n")
    .file("src/orphan.rs", "fn p() {}\n")
    // cfg(test) guards the item BEFORE `mod prod;`, not `mod prod;`.
    .file("src/other.rs", "#[cfg(test)]\nuse x::y;\nmod prod;\n")
    .file("src/other/prod.rs", "fn p() {}\n");
    for rel in [
        "src/lib.rs",
        "src/tests.rs",
        "src/prod.rs",
        "src/orphan.rs",
        "src/other/prod.rs",
        "src/missing.rs",
    ] {
        assert!(!t.is_test_file(rel), "{rel} must be production");
    }
}

/// no-wallclock's `--self-test` drives the predicate through the lint's
/// own `is_allowed`, and SAYS so — a silent pass is indistinguishable
/// from a self-test that no longer runs.
#[test]
fn no_wallclock_proves_the_whole_file_shape_on_every_invocation() {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/lint/no-wallclock.sh"))
        .arg("--self-test")
        .current_dir(repo_root())
        .output()
        .expect("run no-wallclock's self-test");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the self-test must pass:\n{text}");
    assert!(
        text.contains("whole-file test module"),
        "the self-test must say it covered the whole-file shape:\n{text}"
    );
}
