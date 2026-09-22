//! `infra/gate.sh --quick` and `--lint` are RUN, not read — against
//! synthetic trees, so every property below is one the pre-flight
//! actually has.
//!
//! THE CLASS (2026-09-21, backlog 87454e54, gate-run 9e02228b). The
//! pre-flight reads the WORKING TREE; the gate checks out the PUSHED
//! commit. A builder resolving a rerail ran `cherry-pick --continue`
//! (which commits), then `cargo fmt --all`, then `gate.sh --lint`
//! (`pre-flight: clean`), then `git push`. The fmt had moved one
//! mis-indented closing brace and was never committed, so the push
//! carried the PRE-fmt commit: the gate went red on `fmt`, and on
//! `no-employee-id-literal` — the brace was where that lint reads the
//! `cfg(test)` region to end, so a literal inside a test module read as
//! production code. One whitespace character, two failed checks,
//! neither of them about the change.
//!
//! THE OTHER HALF OF an_untracked_file_refuses_the_preflight_by_name.
//! `refuse_untracked_files` (backlog a5efc919) covers the file git
//! cannot see at all, and its own words — "an untracked file is clean
//! here and red at the gate, which checks out the pushed commit" —
//! apply word for word to a file git CAN see and reads differently from
//! HEAD. That half was unguarded.
//!
//! A WARNING, NOT A REFUSAL, unlike the untracked case: running the
//! pre-flight mid-edit to see whether the lints are happy is the normal
//! builder loop, and refusing it would break that. What must not happen
//! silently is pre-flight -> push with edits in between, so the closing
//! lines name the files and the verdict still stands.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The one lint the roster refuses to run without, and the tracked file
/// these tests edit — one file plays both parts, the way a builder's
/// own `cargo fmt` edits a file that is already tracked.
const LINT: &str = "infra/lint/workspace-declares-what-it-runs.sh";

/// A synthetic repository with this tree's `infra/gate.sh` at the path
/// its own `cd "$(dirname "$0")/.."` resolves from, the lint libs it
/// sources, one lint, a `cargo` that exits 0 (the pre-flight's `fmt`
/// check) and a `df` reporting plenty — so `--quick` runs all the way
/// to its closing lines, which are what this file is about.
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("preflight-differs-from-head-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(&tree.join(LINT), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::write_file(&tree.join(".gitignore"), "scratch/\n");
        let t = Tree { dir, tree };
        t.git(&["init", "-q", "-b", "main"]);
        t.git(&["add", "."]);
        t.git(&[
            "-c",
            "user.email=preflight-head@test",
            "-c",
            "user.name=preflight-head",
            "commit",
            "-q",
            "-m",
            "the gate under test",
        ]);
        let bin = t.dir.join("bin");
        boss_testing::create_dir(&bin);
        boss_testing::write_exec(&bin.join("cargo"), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::write_exec(
            &t.dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        t
    }

    fn git(&self, args: &[&str]) {
        run_git(&self.tree, args);
    }

    /// The shape the 9e02228b builder's tree had: a tracked file edited
    /// after the commit the push will carry.
    fn edit_a_tracked_file(&self) -> &Tree {
        boss_testing::write_file(
            &self.tree.join(LINT),
            "#!/usr/bin/env bash\n# the edit cargo fmt made and nobody committed\nexit 0\n",
        );
        self
    }

    fn run(&self, mode: &str) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg(mode)
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "main")
            .env("BOSS_TRUNK_REF", "main")
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {mode} in {}: {e}", self.tree.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The words the note must carry, so a builder reading the tail of a
/// pre-flight knows which tree was judged without opening the script.
const DIFFERS: &str = "differ from HEAD";

/// Every assertion the two modes share: the run still reaches a verdict
/// (a warning, not a refusal), and the note names the count, the file
/// and what the gate will read instead.
fn assert_named(out: &Output, mode: &str) {
    let t = text(out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{mode}: an uncommitted edit is a WARNING beside the verdict, not a refusal — \
         a builder runs the pre-flight mid-edit on purpose:\n{t}"
    );
    assert!(
        t.contains("pre-flight: clean"),
        "{mode}: the lints did read this tree and it is clean; the note is beside that \
         verdict, not instead of it:\n{t}"
    );
    assert!(
        t.contains(DIFFERS) && t.contains("1 tracked file(s)"),
        "{mode}: the note must say how many files the gate will read differently:\n{t}"
    );
    assert!(
        t.contains(LINT),
        "{mode}: the note must NAME the files — a count alone sends the builder to \
         re-derive it (CLAUDE.md, a verdict someone must go re-derive):\n{t}"
    );
    assert!(
        t.contains("PUSHED commit"),
        "{mode}: the note must say what the gate reads instead of this tree:\n{t}"
    );
}

/// THE DEFECT. A tracked file edited and not committed is named beside
/// the closing verdict, in both pre-flight modes.
#[test]
fn an_uncommitted_edit_is_named_beside_the_verdict_in_both_preflight_modes() {
    for mode in ["--quick", "--lint"] {
        let tree = Tree::new(mode.trim_start_matches('-'));
        tree.edit_a_tracked_file();
        assert_named(&tree.run(mode), mode);
    }
}

/// STAGED IS STILL UNCOMMITTED. `git add` moves an edit into the index,
/// where every git-based lint reads it — and leaves it out of the
/// commit the push carries just the same.
#[test]
fn a_staged_but_uncommitted_edit_is_named_too() {
    let tree = Tree::new("staged");
    tree.edit_a_tracked_file();
    tree.git(&["add", LINT]);
    assert_named(&tree.run("--quick"), "--quick (staged)");
}

/// THE CONTROL. A tree that matches HEAD says nothing about HEAD — a
/// note on every clean pre-flight is a note nobody reads.
#[test]
fn a_tree_that_matches_head_says_nothing_about_head() {
    let tree = Tree::new("clean");
    let out = tree.run("--quick");
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a clean tree is certified:\n{t}"
    );
    assert!(
        t.contains("pre-flight: clean"),
        "a clean tree reaches the verdict:\n{t}"
    );
    assert!(
        !t.contains(DIFFERS),
        "a tree equal to HEAD must not be told it differs from it:\n{t}"
    );
}
