//! Every tenant bundle the image ships under `examples/` passes
//! `boss tenant check` before it can ride a train — the gate's half of
//! the tenant pre-check (backlog fd8ee021, part 1; the converge's half
//! is 1af5119d, pinned in
//! `the_converge_checks_the_tenant_before_it_applies.rs`).
//!
//! WHY. `infra/oss-quickstart/Dockerfile` COPYs `examples/` into the
//! image, and an instance declaring `tenant_dir = "examples/<name>"`
//! boots the gateway from that directory. The converge runs `boss
//! tenant check` on a `tenant_repo` checkout before it applies the
//! ConfigMap — but an image-sourced bundle never passes through that
//! door: it is baked in by CI from whatever the train carried, and the
//! first thing to judge a bad seed line is the gateway's own boot
//! refusal, which takes the system of record down (the 2026-09-07
//! class, a boot guard that refuses to start). So the bundle is judged
//! where the tree is judged: a lint in `infra/lint/`, run by the gate.
//!
//! WHAT IS PINNED, by RUNNING the lint against a synthetic tree with a
//! stub `boss` on PATH that records its argv and answers per bundle:
//!
//!   * the roster is the DIRECTORY: every `examples/*/` is checked, in
//!     C-locale order, no hand list; the `scanned` line counts them;
//!   * one refusing bundle reds the run (exit 1) and the output names
//!     the bundle AND the refused row (`<file>:<line>`), so nobody
//!     re-runs the verb to learn which file;
//!   * no `boss` anywhere (not on PATH, no built target) is exit 3
//!     with lib/git-answer.sh's CANNOT ANSWER marker and NO `scanned`
//!     line — a fact about the machine, never a pass; a `boss` that
//!     dies without a verdict is the same refusal;
//!   * an empty `examples/` is exit 1 (lib/scanned.sh: a lint that
//!     scanned nothing certifies nothing);
//!   * the lint declares `# consist: skip` (the pre-flight runs before
//!     the build, and the boss-ci image carries no `boss` — a roster
//!     lint here would refuse EVERY gate) and `infra/gate.sh` runs it
//!     as a `check` after its build phase, the way it runs
//!     `no-snapshot-arrays`. "A check nobody runs is not running."

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "an-image-sourced-tenant-passes-its-check";

/// The stub's own render, in the CLI's shape (tenant.rs
/// `Report::render`: status, path, detail — the converge's parser reads
/// the same rows). The bad bundle's INVALID row names a line, as the
/// real verb does for a TOML parse error.
const REFUSAL: &str = "boss tenant check examples/bad\n\
  OK       seeds/tenant.toml       tenant_id=bad\n\
  INVALID  seeds/workflows.toml    TOML parse error at line 2, column 10\n\
1 ok, 0 missing, 1 invalid, 0 unknown — FAIL\n";

/// A synthetic tree holding this tree's lint and the two helpers it
/// sources, the bundles a case names under `examples/`, and a `bin/`
/// whose `boss` records every argv to `argv.log` and answers by the
/// bundle's directory name: `bad` prints [`REFUSAL`] and exits 1,
/// `dies` exits 101 with no report, anything else exits 0.
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str, bundles: &[&str]) -> Tree {
        let dir = scratch_dir(&format!("tenant-check-lint-{tag}"));
        let tree = dir.join("tree");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        for rel in [
            &format!("infra/lint/{LINT}.sh"),
            "infra/lint/lib/scanned.sh",
            "infra/lint/lib/git-answer.sh",
        ] {
            std::fs::copy(repo_root().join(rel), tree.join(rel))
                .unwrap_or_else(|e| panic!("carry {rel} into the synthetic tree: {e}"));
        }
        boss_testing::create_dir(&tree.join("examples"));
        for b in bundles {
            boss_testing::create_dir(&tree.join("examples").join(b).join("seeds"));
            write_file(
                &tree.join("examples").join(b).join("seeds/tenant.toml"),
                &format!("[meta]\ntenant_id = \"{b}\"\n"),
            );
        }
        // A file beside the bundles is not a bundle: the roster is
        // directories only.
        write_file(&tree.join("examples/README.md"), "not a tenant\n");
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        let log = dir.join("argv.log");
        write_exec(
            &bin.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 case \"$3\" in\n\
                   *bad) printf '%s' '{}'; exit 1 ;;\n\
                   *dies) echo 'thread main panicked' >&2; exit 101 ;;\n\
                 esac\n\
                 exit 0\n",
                log.display(),
                REFUSAL.replace('\'', "'\\''"),
            ),
        );
        Tree { dir, tree }
    }

    /// The lint, with `bin/` on PATH (or not), no CARGO_TARGET_DIR and
    /// no `target/` in the tree — so the only `boss` it can find is
    /// the stub, or nothing.
    fn run(&self, with_stub: bool) -> Output {
        let path = if with_stub {
            format!(
                "{}:/usr/local/bin:/usr/bin:/bin",
                self.dir.join("bin").display()
            )
        } else {
            "/usr/local/bin:/usr/bin:/bin".to_string()
        };
        Command::new("bash")
            .arg(self.tree.join(format!("infra/lint/{LINT}.sh")))
            .current_dir(&self.tree)
            .env_clear()
            .env("PATH", path)
            .env("LC_ALL", "C")
            .output()
            .unwrap_or_else(|e| panic!("run {LINT} in {}: {e}", self.tree.display()))
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv.log")).unwrap_or_default()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// THE ROSTER IS THE DIRECTORY. Two passing bundles: exit 0, the stub
/// was asked `tenant check examples/<name>` once per bundle in
/// C-locale order and never for the README, and the `scanned` line
/// counts two.
#[test]
fn every_bundle_under_examples_is_checked_and_counted() {
    let tree = Tree::new("passes", &["zeta", "alpha"]);
    let out = tree.run(true);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "two passing bundles are a clean run:\n{t}"
    );
    assert_eq!(
        tree.argv(),
        "tenant check examples/alpha\ntenant check examples/zeta\n",
        "the verb is run once per directory under examples/, in C-locale order, \
         and a file beside them is not a bundle:\n{t}"
    );
    assert!(
        t.contains(&format!("{LINT}: scanned 2 tenant bundle(s)")),
        "the scanned line counts the bundles the lint judged (lib/scanned.sh):\n{t}"
    );
}

/// ONE REFUSING BUNDLE REDS THE RUN, and the red names the bundle and
/// the row — `examples/bad` and `seeds/workflows.toml:2` — while the
/// passing bundle beside it is still checked (the whole roster is
/// judged, not the first failure).
#[test]
fn a_refused_bundle_is_a_red_that_names_the_bundle_and_the_row() {
    let tree = Tree::new("refuses", &["alpha", "bad"]);
    let out = tree.run(true);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a bundle the verb refuses is a fact about the branch — exit 1:\n{t}"
    );
    assert!(
        t.contains("examples/bad") && t.contains("seeds/workflows.toml:2"),
        "the red names the bundle and the refused row as <file>:<line>:\n{t}"
    );
    assert!(
        t.contains("INVALID  seeds/workflows.toml"),
        "the verb's own report is replayed, not summarised away (CLAUDE.md §Diagnosis: \
         a reduction before the record is the only copy thrown away):\n{t}"
    );
    assert_eq!(
        tree.argv(),
        "tenant check examples/alpha\ntenant check examples/bad\n",
        "every bundle is judged even when an earlier one refused:\n{t}"
    );
}

/// NO CLI IS A REFUSAL, NOT A PASS. With nothing named `boss` on PATH
/// and no built target, the lint exits 3 with the CANNOT ANSWER marker,
/// names where it looked, and prints no `scanned` line — the shape
/// `a_lint_that_cannot_answer_refuses_the_gate.rs` pins the gate to
/// read as a refusal.
#[test]
fn no_cli_is_cannot_answer_never_clean() {
    let tree = Tree::new("no-cli", &["alpha"]);
    let out = tree.run(false);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "no boss binary is a fact about the MACHINE: exit 3 (LINT_CANNOT_ANSWER), \
         never 0 and never 1:\n{t}"
    );
    assert!(
        t.contains("CANNOT ANSWER") && t.contains("boss"),
        "the refusal carries lib/git-answer.sh's marker and names the binary:\n{t}"
    );
    assert!(
        !t.contains("scanned") && !t.contains(": ok"),
        "a refusal certifies nothing — no scanned line, no ok:\n{t}"
    );
    assert_eq!(tree.argv(), "", "nothing ran:\n{t}");
}

/// A CLI THAT DIES WITHOUT A VERDICT is the same refusal as no CLI:
/// nothing about the bundle was judged. The converge's `tenant_check`
/// treats any exit but 0 and 1 the same way, so the two readers of
/// this verb agree.
#[test]
fn a_cli_that_dies_without_a_verdict_is_cannot_answer() {
    let tree = Tree::new("dies", &["dies"]);
    let out = tree.run(true);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "exit 101 from the verb is no verdict on the bundle — exit 3:\n{t}"
    );
    assert!(
        t.contains("CANNOT ANSWER") && t.contains("101") && t.contains("panicked"),
        "the refusal carries the verb's exit status and its own words:\n{t}"
    );
    assert!(!t.contains("scanned"), "no scanned line on a refusal:\n{t}");
}

/// AN EMPTY examples/ IS RED, not clean: lib/scanned.sh refuses a zero
/// scan, because a lint that looked at nothing has nothing to certify.
#[test]
fn an_empty_examples_directory_is_red() {
    let tree = Tree::new("empty", &[]);
    let out = tree.run(true);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "zero bundles scanned is exit 1 (lib/scanned.sh):\n{t}"
    );
    assert!(
        t.contains("scanned 0 tenant bundle(s)"),
        "the refusal says what was counted:\n{t}"
    );
}

/// THE GATE RUNS IT. The lint is out of the pre-flight roster by its
/// own `# consist: skip` header line (the boss-ci image carries no
/// `boss`; only the gate's build phase makes one), and `infra/gate.sh`
/// runs it as a named `check` — otherwise the exclusion would be a
/// lint nobody runs, the twelve-day gap `no-snapshot-arrays` sat in.
#[test]
fn the_lint_is_excluded_from_the_preflight_and_run_by_the_gate_after_its_build() {
    let root = repo_root();
    let lint =
        std::fs::read_to_string(root.join(format!("infra/lint/{LINT}.sh"))).expect("read the lint");
    let header_skip = lint
        .lines()
        .take_while(|l| l.starts_with('#') || l.trim().is_empty())
        .any(|l| l.starts_with("# consist: skip — ") && l.contains("build"));
    assert!(
        header_skip,
        "the lint's HEADER must declare `# consist: skip — <why>` naming the build it \
         waits for; the pre-flight runs before any binary exists"
    );
    let gate = std::fs::read_to_string(root.join("infra/gate.sh")).expect("read gate.sh");
    let check = format!("check \"{LINT}\" infra/lint/{LINT}.sh");
    let sites: Vec<usize> = gate.match_indices(&check).map(|(i, _)| i).collect();
    assert_eq!(
        sites.len(),
        2,
        "gate.sh runs the lint as a check in BOTH cargo modes — the full gate and the \
         scoped (--auto) gate — so a tenant-bundle car and a train each pay for it: \
         found {} site(s)",
        sites.len()
    );
    let build = gate
        .find("check \"build (default features)\" cargo build --workspace")
        .expect("the full build phase");
    assert!(
        sites[0] > build,
        "the check runs AFTER the build that produces the binary it needs"
    );
    let scoped_build = gate
        .find("check \"build boss-cli\" cargo build -p boss-cli")
        .expect("the scoped gate builds boss-cli before it can run the verb");
    assert!(
        sites[1] > scoped_build,
        "in the scoped gate the CLI is built first: a bundle car's scope is its engine, \
         not boss-cli, so the binary must be asked for by name"
    );
    let exclusions = Command::new("bash")
        .arg(root.join("infra/gate.sh"))
        .arg("--exclusions")
        .current_dir(&root)
        .output()
        .expect("run gate.sh --exclusions");
    let listed = String::from_utf8_lossy(&exclusions.stdout);
    assert!(
        listed
            .lines()
            .any(|l| l.starts_with(&format!("infra/lint/{LINT}.sh\t"))),
        "gate.sh --exclusions derives the skip from the header:\n{listed}"
    );
}
