//! `infra/dev/wt-target-dir.sh` — the one name for a worktree's own
//! cargo target dir, and the one decision about when a build must
//! leave a shared one.
//!
//! WHY (backlog 955c99b6, 2026-09-22). `infra/gate.sh` exports
//! `CARGO_INCREMENTAL=0` and never set `CARGO_TARGET_DIR`, so its
//! cargo inherited the pod-wide `/scratch/target`
//! (infra/cluster/manifests/boss-dev.yaml) — while every builder's
//! real build goes through `infra/dev/wt-cargo`, which gives each
//! worktree its own. Six builders ran concurrently on 2026-09-22, so
//! six source trees checked into ONE target directory, and
//! `infra/gate.sh --lint` — the door CLAUDE.md names before pushing —
//! false-redded a clean tree three times for one builder.
//!
//! The mechanism, reproduced deterministically in a two-package
//! fixture before this car was written: cargo's artifact identity does
//! NOT include the workspace path (two packages of the same name and
//! version at different paths were given the SAME `-C metadata` hash
//! and one rmeta), and its freshness check resolves the dep-info's
//! cwd-RELATIVE source paths against the current directory. So a
//! neighbour worktree's rlib, built minutes ago, reads as fresh here,
//! and this tree compiles against the neighbour's API: the reported
//! symptom was `no variant named List found for enum boss_expr::Expr`
//! against source that compiles cleanly under wt-cargo, cleared by
//! `touch`-ing the source file.
//!
//! What the helper must and must not do, each pinned below. It must
//! isolate a LINKED worktree — the only checkout that shares a target
//! dir with a checkout it does not own — and it must leave a MAIN
//! worktree alone, because that is the cluster gate runner (which
//! `git clone`s into a per-run emptyDir and exports its own
//! `/gate-target/target`) and forge CI. Redirecting there would trade
//! a false red for a cold ~74 GB build on every gate.

use boss_testing::{repo_root, scratch_dir};
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER: &str = "infra/dev/wt-target-dir.sh";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    root: PathBuf,
    /// The main checkout — what the gate runner's clone and CI look like.
    main: PathBuf,
    /// The dir the pod's manifest would hand every build.
    shared: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-target-dir-{name}"));
        let main = root.join("boss");
        boss_testing::create_dir(&main);
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@example.invalid"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join("f"), "x").expect("seed file");
        git(&main, &["add", "f"]);
        git(&main, &["commit", "-qm", "seed"]);
        let shared = root.join("scratch").join("target");
        boss_testing::create_dir(&shared);
        Self { root, main, shared }
    }

    /// A linked worktree, exactly as a builder gets one.
    fn worktree(&self, name: &str) -> PathBuf {
        let wt = self.root.join("trees").join(name);
        boss_testing::create_dir(&self.root.join("trees"));
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                wt.to_str().expect("utf8"),
            ],
        );
        wt
    }

    /// Source the helper in `cwd`, call it, and report what
    /// `CARGO_TARGET_DIR` became. `target` is the ambient value, or
    /// `None` for unset — the case where there is nothing to leave.
    fn isolate(&self, cwd: &Path, target: Option<&Path>) -> (i32, String, String) {
        let script = format!(
            "set -u\n. '{}' || exit 2\nwt_isolate_target_dir\necho \"target=${{CARGO_TARGET_DIR:-unset}}\"\n",
            repo_root().join(HELPER).display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(script).current_dir(cwd);
        match target {
            Some(t) => cmd.env("CARGO_TARGET_DIR", t),
            None => cmd.env_remove("CARGO_TARGET_DIR"),
        };
        let out = cmd.output().expect("run the helper");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// THE DEFECT. A builder's worktree must not check into the dir the
/// main checkout owns — it must check into the SAME per-worktree dir
/// `wt-cargo` already seeded for it, so `--lint` predicts the build
/// the builder ran rather than a neighbour's.
#[test]
fn a_linked_worktree_leaves_the_shared_target_dir() {
    let f = Fixture::new("linked");
    let wt = f.worktree("agent-0123abcd");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&f.shared));
    assert_eq!(rc, 0, "{stdout}{stderr}");

    let mine = f.root.join("scratch").join("target-agent-0123abcd");
    assert!(
        stdout.contains(&format!("target={}", mine.display())),
        "a linked worktree must build into its own dir: {stdout}{stderr}"
    );
    // The line is the whole point: a builder who sees `--lint` use a
    // different dir than the run before must be able to read why.
    assert!(
        stderr.contains(mine.to_str().expect("utf8"))
            && stderr.contains(f.shared.to_str().expect("utf8")),
        "the redirect must name both dirs: {stderr:?}"
    );
    assert!(
        stderr.contains("955c99b6"),
        "the message must carry the packet that measured this: {stderr:?}"
    );
}

/// THE CASE THAT MUST NOT MOVE. The cluster gate runner clones into a
/// per-run emptyDir and exports `/gate-target/target`; forge CI clones
/// too. Both are MAIN worktrees, both own their target dir, and both
/// would pay a cold build for a redirect they do not need.
#[test]
fn a_main_checkout_keeps_the_target_dir_it_was_given() {
    let f = Fixture::new("main");
    let given = f.root.join("gate-target").join("target");

    let (rc, stdout, stderr) = f.isolate(&f.main, Some(&given));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", given.display())),
        "a main checkout owns its target dir: {stdout}{stderr}"
    );
    assert_eq!(
        stderr, "",
        "nothing was changed, so nothing is said: {stderr:?}"
    );
}

/// Unset is not shared: cargo then uses `<workspace>/target`, which is
/// per-worktree by construction. Nothing to leave, nothing to say —
/// and the helper must not invent a `/scratch` that this machine may
/// not have.
#[test]
fn an_unset_target_dir_is_left_unset() {
    let f = Fixture::new("unset");
    let wt = f.worktree("agent-unset");

    let (rc, stdout, stderr) = f.isolate(&wt, None);
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(stdout.contains("target=unset"), "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing to say: {stderr:?}");
}

/// A target dir INSIDE the worktree is already this tree's own — the
/// operator who exported one by hand keeps it.
#[test]
fn a_target_dir_inside_the_worktree_is_already_its_own() {
    let f = Fixture::new("inside");
    let wt = f.worktree("agent-inside");
    let inside = wt.join("target");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&inside));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", inside.display())),
        "{stdout}{stderr}"
    );
    assert_eq!(stderr, "", "nothing to say: {stderr:?}");
}

/// Idempotence, the correctness protocol's own property: a second call
/// on the result of the first must be a no-op, so a gate re-entered
/// through a wrapper cannot walk `target-agent-x` into
/// `target-target-agent-x`.
#[test]
fn isolating_an_already_isolated_dir_changes_nothing() {
    let f = Fixture::new("idempotent");
    let wt = f.worktree("agent-twice");
    let mine = f.root.join("scratch").join("target-agent-twice");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&mine));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", mine.display())),
        "{stdout}{stderr}"
    );
    assert_eq!(stderr, "", "a no-op says nothing: {stderr:?}");
}

/// §9a — the name lives ONCE. `wt-cargo` seeds the dir and `gate.sh`
/// checks into it; if either spelled `target-<basename>` itself the
/// two would drift, and the drift's symptom is the silent one this
/// car exists to close: a `--lint` that reads a dir no build wrote.
#[test]
fn wt_cargo_and_the_gate_both_read_the_name_from_here() {
    let helper = std::fs::read_to_string(repo_root().join(HELPER)).expect(HELPER);
    assert!(
        helper.contains("wt_target_dir()") && helper.contains("wt_isolate_target_dir()"),
        "{HELPER} defines both the name and the decision"
    );

    for (path, call) in [
        ("infra/dev/wt-cargo", "wt_target_dir"),
        ("infra/gate.sh", "wt_isolate_target_dir"),
    ] {
        let text = std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| {
            panic!("{path}: {e}");
        });
        assert!(
            text.contains("wt-target-dir.sh"),
            "{path} must source {HELPER}, not carry its own copy of the rule"
        );
        assert!(
            text.contains(call),
            "{path} must call {call}() rather than spell the dir itself"
        );
        assert!(
            !text.contains("/target-$"),
            "{path} must not retype the per-worktree name — that is the drift §9a names"
        );
    }
}
