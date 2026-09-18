//! A pre-flight lint that COULD NOT ANSWER refuses the gate; it does not
//! red it. `infra/gate.sh --quick` is RUN against a synthetic tree, so
//! the property is one the gate has, not one its comments claim.
//!
//! THE CLASS (2026-09-18, backlog a26f92c4, gate 35f4ff0c). The lint
//! `the-live-protocols-are-the-authored-protocols` reads the live
//! registry. When the system of record was rolling (a converge) it
//! skipped — exit 0, no `scanned` line — and the lint-of-lints pin
//! (`a_lint_that_scanned_nothing_is_red`) redded the gate. An
//! infrastructure refusal recorded as a consist failure, the exact
//! shape CLAUDE.md §Diagnosis names.
//!
//! TWO HALVES, ONE VOCABULARY. `infra/lint/lib/git-answer.sh` already
//! reserves exit 3 (`LINT_CANNOT_ANSWER`) for "the machine could not
//! answer — never clean, never a verdict on the branch", and named the
//! gate's mapping as the half still open. The lint half is pinned in
//! `a_lint_that_scanned_nothing_is_red.rs` (a dead registry is exit 3
//! with the marker and NO scanned line). This file pins the gate half:
//! a roster lint exiting 3 becomes a REFUSAL receipt in the disk
//! floor's shape — `verdict: refused`, `refused_because` naming the
//! lint and its own words, exit 2 — which `train_gate::standing` reads
//! as `Standing::Refused` and relaunches, and which strikes no car.
//! A lint exiting 1 stays a red: a fact about the branch is still the
//! author's to fix.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The lint every case plants, and the line it prints when it refuses.
const LINT: &str = "a-stub-that-reads-a-dark-registry";
const WHY: &str = "http://[::1]:9/api/workflows answered HTTP 000";

/// A synthetic repository holding this tree's `infra/gate.sh` and the
/// lib it sources, the one lint the roster refuses to run without, a
/// `cargo` that passes `fmt` without a workspace, and a `df` that always
/// reports plenty — so `--quick` runs its roster and the only thing that
/// can decide the verdict is the lint a case plants.
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("lint-cannot-answer-{tag}"));
        let tree = dir.join("tree");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        for rel in ["infra/gate.sh", "infra/lint/lib/git-answer.sh"] {
            std::fs::copy(repo_root().join(rel), tree.join(rel))
                .unwrap_or_else(|e| panic!("carry {rel} into the synthetic tree: {e}"));
        }
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(
            &tree,
            &[
                "-c",
                "user.email=lint-cannot-answer@test",
                "-c",
                "user.name=lint-cannot-answer",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        boss_testing::write_exec(&bin.join("cargo"), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        Tree { dir, tree }
    }

    /// The planted lint: `exit_with` is its whole verdict, and the
    /// refusal line is the shape `git-answer.sh` prints.
    fn lint(&self, exit_with: &str) -> &Tree {
        let body = format!(
            "#!/usr/bin/env bash\n\
             . infra/lint/lib/git-answer.sh\n\
             echo '{LINT}: '\"$LINT_CANNOT_ANSWER_MARKER\"' — {WHY}' >&2\n\
             exit {exit_with}\n"
        );
        boss_testing::write_exec(&self.tree.join(format!("infra/lint/{LINT}.sh")), &body);
        git(&self.tree, &["add", "."]);
        self
    }

    fn receipt_path(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    fn quick(&self) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg("--quick")
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.receipt_path())
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

/// THE DEFECT. A roster lint exiting `LINT_CANNOT_ANSWER` is a refusal
/// in the disk floor's shape: exit 2, a receipt whose verdict is
/// `refused`, `refused_because` naming the lint and carrying its own
/// words, and the lint's check recorded as `refused` — never `fail`,
/// because nothing about the branch was judged.
#[test]
fn a_lint_that_cannot_answer_is_a_refusal_receipt_not_a_red() {
    let tree = Tree::new("refuses");
    tree.lint("\"$LINT_CANNOT_ANSWER\"");
    let out = tree.quick();
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a lint that could not answer is a REFUSAL (exit 2, the disk floor's shape), \
         not a red (exit 1) and not a certification:\n{t}"
    );
    assert!(
        !t.contains("check(s) failed"),
        "the pre-flight must not report a refusal as failed checks:\n{t}"
    );
    let raw = std::fs::read_to_string(tree.receipt_path()).unwrap_or_else(|e| {
        panic!("a refusal writes a receipt, like the disk floor does: {e}\n{t}")
    });
    let receipt: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("the receipt is JSON: {e}\n{raw}"));
    assert_eq!(
        receipt["verdict"], "refused",
        "the receipt's verdict is the word train_gate::standing reads as Refused:\n{raw}"
    );
    let why = receipt["refused_because"]
        .as_str()
        .unwrap_or_else(|| panic!("refused_because is a string:\n{raw}"));
    assert!(
        why.contains(LINT) && why.contains(WHY),
        "refused_because names the lint AND carries what it said, so nobody re-derives \
         which lint or why:\n{why}"
    );
    let checks = receipt["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("the receipt carries a checks array:\n{raw}"));
    let entry = checks
        .iter()
        .find(|c| c["name"] == LINT)
        .unwrap_or_else(|| panic!("the refusing lint is in the checks list:\n{raw}"));
    assert_eq!(
        entry["result"], "refused",
        "the lint's own entry says refused, not fail — a reader of the checks list \
         must not count it as a verdict:\n{raw}"
    );
}

/// THE CONTROL. A lint exiting 1 found a fact about the branch, and
/// that stays the author's red: exit 1, named as a failed check, and no
/// refusal receipt is written for it.
#[test]
fn a_lint_that_fails_is_still_a_red() {
    let tree = Tree::new("fails");
    tree.lint("1");
    let out = tree.quick();
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a lint that read the tree and found a violation is a red, not a refusal:\n{t}"
    );
    assert!(
        t.contains("check(s) failed") && t.contains(LINT),
        "the red names the failed lint:\n{t}"
    );
    assert!(
        !tree.receipt_path().exists(),
        "a red pre-flight writes no refusal receipt — one here would be read as \
         infrastructure's fault, not the branch's:\n{t}"
    );
}
