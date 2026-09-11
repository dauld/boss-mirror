//! `infra/gate.sh` is the documented pre-push door (CLAUDE.md §Doors),
//! and since #310 (2026-09-11) the gate itself runs as uid 65534 / gid
//! 1500 — which is also the uid every builder brief now tells an agent
//! to verify work under. The dev pod's checkout is root-owned
//! (`drwxrwsr-x root 1500`), so since git 2.35.2 every git command run
//! by any other uid refuses it:
//!
//!     fatal: detected dubious ownership in repository at '/work/boss'
//!
//! MEASURED as 65534 before the fix (packet 8674c440): `--quick` exits 1
//! naming `migrations-append-only`, `no-secrets` and
//! `steptype-bundle-ratchet`. That list is the loud third of the defect.
//! Five lints are affected and they fail three different ways:
//!
//! | lint | git call | what it does |
//! |---|---|---|
//! | `no-secrets` | `git ls-files` | FAILS, printing the refusal |
//! | `migrations-append-only` | trunk `rev-parse` | refusal swallowed by `2>/dev/null`; fails claiming "no trunk ref found … Fetch the trunk" |
//! | `steptype-bundle-ratchet` | trunk `rev-parse` | same wrong diagnosis |
//! | `no-session-paths` | `git grep` via `lib/pattern-scan.sh` | `|| true` — reports `clean` on a tree it never read |
//! | `one-palette` | `git grep` | `|| true` — reports `clean` likewise |
//!
//! The last pair is the reason the fix belongs in `gate.sh` rather than
//! in the lints: a SILENT FALSE PASS has no red to notice, so a per-lint
//! fix has to find five call sites and would have to keep finding them
//! (CLAUDE.md §9a).
//!
//! ## Why this file exists beside `gate_sh.rs`
//!
//! `gate_sh.rs` pins the gate's DEFINITION (one roster, no second
//! inline copy in CI). This pins the gate's ENVIRONMENT: that a child
//! of the gate can read git at all. Different question, and on
//! 2026-09-11 a separate car was already in flight on `gate_sh.rs`, so
//! a new file is also the non-colliding place to put it.
//!
//! ## Why no `chown`, and why this holds as root
//!
//! A test cannot create a foreign-owned directory without root, and the
//! gate has no root. `GIT_TEST_ASSUME_DIFFERENT_OWNER` is git's own
//! "pretend another user owns this" knob, so these tests reproduce the
//! uid-65534 refusal while running as ANY uid — root included. That is
//! the property that makes them trustworthy on the runner: they do not
//! pass merely because the account happened to own the tree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

/// Run a command, returning its output with stdout and stderr joined —
/// the refusal these tests look for goes to stderr, and a verdict that
/// reads only one stream names half of what happened.
fn run(cmd: &mut Command) -> (bool, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("command spawns");
    let mut text = String::from_utf8_lossy(&stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&stderr));
    (status.success(), text)
}

/// `git` in `dir`, with the simulated foreign owner and no config
/// channel that could carry a `safe.directory` of its own.
///
/// `GIT_CONFIG_COUNT=0` drops the env channel, and the two `/dev/null`
/// config paths drop the global and system files — `safe.directory` is
/// read from exactly those three places, so a repository blessed for
/// this box by a previous operator cannot make the probe lie.
fn git_as_a_stranger(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd
}

const REFUSAL: &str = "dubious ownership";

/// The simulation is real, proven OUTSIDE the gate so that the test
/// below cannot be vacuously green.
///
/// If this box's git ignored `GIT_TEST_ASSUME_DIFFERENT_OWNER`, the gate
/// test would pass without ever having been challenged. This builds a
/// repository of its own and shows git refusing it, so "the gate opened
/// under a foreign owner" means something when we read it.
///
/// The fixture is `boss_testing::scratch`, which carries the pid AND the
/// uid: root and 65534 both run this suite on the long-lived dev pod,
/// and a fixed `/tmp` path shared between them is a defect class this
/// repo has hit eleven times.
#[test]
fn git_refuses_a_repository_it_is_told_another_user_owns() {
    let dir = boss_testing::scratch::scratch_dir("gate-foreign-owner");
    let repo = dir.join("repo");
    boss_testing::scratch::create_dir(&repo);
    for args in [
        &["init", "-q", "."][..],
        &["config", "user.email", "t@t"][..],
        &["config", "user.name", "t"][..],
    ] {
        let (ok, out) = run(Command::new("git").args(args).current_dir(&repo));
        assert!(ok, "fixture `git {}` failed in {repo:?}: {out}", args[0]);
    }

    // An empty index answers `ls-files` with exit 0 and no output, so a
    // failure below is the ownership refusal and not an empty tree.
    let (ok, out) = run(&mut git_as_a_stranger(&repo, &["ls-files"]));
    assert!(
        !ok && out.contains(REFUSAL),
        "this git did not refuse {repo:?} when told another user owns it, so \
         GIT_TEST_ASSUME_DIFFERENT_OWNER has no effect here and \
         `the_gate_opens_for_a_uid_that_does_not_own_the_checkout` below is \
         proving nothing. Output was: {out}"
    );
}

/// THE PACKET, end to end: a child of `infra/gate.sh` can read git in a
/// checkout whose owner git refuses.
///
/// `--self-test` rather than `--quick`: it runs the gate's own git calls
/// and its three self-tests in about a second, builds no throwaway
/// repository of its own (a repo-wide foreign-owner simulation would
/// refuse those fixtures and red the run for a reason that is not this
/// defect), and `gate_git_reads_self_test` inside it is what probes a
/// child's git read. `--quick` is the human-facing success criterion and
/// is verified by hand under `setpriv`; running its whole lint roster
/// from inside `cargo test` would nest the roster in the gate that runs
/// this test.
#[test]
fn the_gate_opens_for_a_uid_that_does_not_own_the_checkout() {
    let root = repo_root();
    let mut cmd = Command::new("bash");
    cmd.arg(root.join("infra/gate.sh"))
        .arg("--self-test")
        .current_dir(&root)
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_CONFIG_COUNT");
    let (ok, out) = run(&mut cmd);
    assert!(
        ok,
        "infra/gate.sh --self-test exited nonzero in a checkout git was told \
         another user owns. On the dev pod's root-owned tree the gate's own uid \
         (65534 since #310) hits this for real, and the cost is not one red \
         check: no-secrets fails, migrations-append-only and \
         steptype-bundle-ratchet fail claiming a missing trunk ref, and \
         no-session-paths and one-palette report `clean` on a tree they never \
         read. Output was:\n{out}"
    );
    assert!(
        !out.contains(REFUSAL),
        "infra/gate.sh --self-test left an ownership refusal in its output. It \
         exited 0, which is how this defect reads as a lint finding rather than \
         an environment problem — a swallowed `2>/dev/null` or a `|| true` turns \
         the refusal into a wrong diagnosis or a false `clean`. Output was:\n{out}"
    );
}
