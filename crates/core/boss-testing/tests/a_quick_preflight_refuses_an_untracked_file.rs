//! `infra/gate.sh --quick` is RUN, not read — against synthetic trees,
//! so every property below is one the pre-flight actually has.
//!
//! THE CLASS (2026-09-18, backlog a5efc919, gate a3b26113). A builder
//! ran `gate.sh --quick`, read `clean`, pushed, and the gate went red
//! on `the-estate-address-lives-once`: the branch carried a NEW manifest
//! spelling the registry address. The pre-flight never saw the file.
//! Every git-based lint — `pattern_scan`, `git_answer … ls-files`,
//! `git grep` — reads the INDEX, and an untracked file is not in it, so
//! a dirty tree is clean at pre-flight and red at the gate, which checks
//! out the pushed commit where the file IS tracked.
//!
//! THE ONE DEFINITION. Rather than teaching every reader to add
//! `--others`, `--quick` (and `--lint`, which is `--quick` plus clippy)
//! REFUSES to certify a tree that holds untracked, not-ignored files,
//! names them, and says the fix. A builder has to `git add` the file
//! before the commit anyway; the refusal only moves that step in front
//! of the read that depends on it. Two lints (`no-secrets`,
//! `the-estate-address-lives-once`) read every tracked path, so the
//! roots the lints scan are the whole tree and the question is exactly
//! `git ls-files --others --exclude-standard`.
//!
//! WHY THIS FILE AND NOT A TEXT PIN ON gate.sh: a grep for
//! `exclude-standard` would prove the words are there, not that the
//! script refuses on them, and the refusal has three edges that only a
//! run can hold — a tracked-but-modified file is fine, an ignored file
//! is fine, only the untracked-not-ignored file refuses.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A synthetic repository with this tree's `infra/gate.sh` at the path
/// its own `cd "$(dirname "$0")/.."` resolves from, the one lint the
/// roster refuses to run without, and a `df` that reports plenty at
/// startup and trips at the poll before `fmt` — so `--quick` decides
/// about the tree and never compiles anything (the same trick
/// gate_sh.rs uses to get a cheap receipt). Reaching that trip IS the
/// evidence that the untracked-file question answered "certify".
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("quick-preflight-untracked-{tag}"));
        let tree = dir.join("tree");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh"))
            .unwrap_or_else(|e| panic!("read infra/gate.sh: {e}"));
        boss_testing::write_exec(&tree.join("infra/gate.sh"), &gate);
        // gate.sh sources the lint vocabulary from the lib and refuses to
        // run without it (a lint's exit 3 is a refusal, backlog a26f92c4).
        std::fs::copy(
            repo_root().join("infra/lint/lib/git-answer.sh"),
            tree.join("infra/lint/lib/git-answer.sh"),
        )
        .unwrap_or_else(|e| panic!("carry infra/lint/lib/git-answer.sh into the tree: {e}"));
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        boss_testing::write_file(&tree.join(".gitignore"), "scratch/\n");
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(
            &tree,
            &[
                "-c",
                "user.email=quick-preflight@test",
                "-c",
                "user.name=quick-preflight",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        let counter = dir.join("calls");
        boss_testing::write_exec(
            &dir.join("df"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=$(cat {c} 2>/dev/null || echo 0)\n\
                 echo $((n+1)) > {c}\n\
                 echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
                 if [ \"$n\" -lt 1 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
                 else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
                c = counter.display()
            ),
        );
        Tree { dir, tree }
    }

    /// A file written into the working tree and NOT added: the shape
    /// the H12 builder's manifest had.
    fn untracked(&self, rel: &str) -> &Tree {
        let path = self.tree.join(rel);
        boss_testing::create_dir(path.parent().expect("a relative path has a parent"));
        boss_testing::write_file(&path, "registry: 10.20.0.34:7900\n");
        self
    }

    fn quick(&self) -> Output {
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg("--quick")
            .current_dir(&self.tree)
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh --quick in {}: {e}", self.tree.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: &Path, args: &[&str]) {
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

/// The words the refusal must carry, so an operator reading it knows
/// which class this is without opening the script.
const REFUSAL: &str = "untracked";
/// The poll before the first check: reaching it means the tree was
/// certified as far as the untracked-file question goes.
const REACHED_FMT: &str = "Refusing to continue before 'fmt'";

/// The tree was certified (as far as this question goes) and the run
/// went on to the first check.
fn assert_certified(out: &Output, what: &str) {
    let t = text(out);
    assert!(
        !t.contains(REFUSAL),
        "{what} must not refuse the pre-flight, but the output names an untracked file:\n{t}"
    );
    assert!(
        t.contains(REACHED_FMT),
        "{what}: the run never reached the first check — the fake df trips there, \
         and that trip is how this test knows the tree was accepted:\n{t}"
    );
}

/// THE DEFECT. An untracked, not-ignored file under a scanned root is
/// refused BEFORE any lint runs, named by path, with the fix.
#[test]
fn an_untracked_file_refuses_the_preflight_by_name() {
    let tree = Tree::new("refuses");
    tree.untracked("infra/estate/instances/new-manifest.yaml");
    let out = tree.quick();
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an untracked file is a REFUSAL (exit 2, like the disk floor), not a verdict on \
         the branch and not a certification:\n{t}"
    );
    assert!(
        t.contains(REFUSAL) && t.contains("infra/estate/instances/new-manifest.yaml"),
        "the refusal must name the file the lints cannot see:\n{t}"
    );
    assert!(
        t.contains("git add"),
        "the refusal must say the fix (add it, or ignore it):\n{t}"
    );
    assert!(
        !t.contains(REACHED_FMT),
        "the refusal must come BEFORE the first check — a lint that ran on this tree \
         would report clean on a file it never read:\n{t}"
    );
}

/// A tracked file with uncommitted edits is what every builder's tree
/// looks like mid-car; `git grep` reads the working copy of a tracked
/// path, so the lints DO see it and there is nothing to refuse.
#[test]
fn a_tracked_but_modified_file_is_certified() {
    let tree = Tree::new("modified");
    boss_testing::write_file(
        &tree
            .tree
            .join("infra/lint/workspace-declares-what-it-runs.sh"),
        "#!/usr/bin/env bash\n# edited, not committed\nexit 0\n",
    );
    assert_certified(&tree.quick(), "a tracked-but-modified file");
}

/// An ignored file is one the gate workspace will never carry either,
/// so the pre-flight and the gate agree about it already.
#[test]
fn an_ignored_file_is_certified() {
    let tree = Tree::new("ignored");
    tree.untracked("scratch/notes.yaml");
    assert_certified(&tree.quick(), "an ignored file");
}

/// The baseline: a clean tree is not refused, or every builder's
/// pre-flight would be.
#[test]
fn a_clean_tree_is_certified() {
    let tree = Tree::new("clean");
    assert_certified(&tree.quick(), "a clean tree");
}
