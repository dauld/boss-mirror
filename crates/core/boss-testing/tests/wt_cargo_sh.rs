//! `infra/dev/wt-cargo` — the door every cargo build on the dev pod goes
//! through: a per-worktree `CARGO_TARGET_DIR` reflink-seeded from the
//! warm shared one, and a jobs bound that keeps a builder inside its
//! share of the pod's 16 GiB cgroup (backlog 34b29b52).
//!
//! Until 2026-09-14 this script was pod-local text under
//! /work/tools/bin, with its OWN copy of the cargo bound — and it
//! drifted (8, against pod-build.env's 6). Now it lives in the tree and
//! sources `infra/dev/pod-build.env`, so there is one number; these
//! tests read that file rather than retyping the number, for the same
//! reason. Pinned here, against a scratch git repository and stub
//! `cargo` / `nice` / `cp` binaries, so nothing touches /scratch:
//!
//!   * an operator worktree builds at pod-build.env's bound, un-niced;
//!   * a builder worktree (basename `agent-*`) builds 4-wide under
//!     `nice -n 10`, so the interactive shell wins the scheduler;
//!   * `WT_JOBS` wins over either;
//!   * the reflink seed is attempted only when the seed exists —
//!     otherwise the per-worktree dir starts cold, and says so;
//!   * the seed lands in a sibling `.seeding` dir and is renamed into
//!     place only once whole (backlog 08782b71): a copy that dies
//!     part-way leaves no half target for the next run to believe warm.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/wt-cargo";

/// The bound pod-build.env declares, read from the file — the test
/// must not become the second literal the script just stopped being.
fn env_file_bound() -> String {
    let live = std::fs::read_to_string(repo_root().join("infra/dev/pod-build.env"))
        .expect("infra/dev/pod-build.env");
    live.lines()
        .find_map(|l| l.strip_prefix("CARGO_BUILD_JOBS="))
        .expect("pod-build.env sets CARGO_BUILD_JOBS")
        .trim()
        .to_string()
}

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    /// Written by the `nice` stub, once per call, with its argv.
    niced: PathBuf,
    /// Written by the `cp` stub, once per call, with its argv.
    copied: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-cargo-{name}"));
        let bin = root.join("bin");
        boss_testing::create_dir(&bin);
        let niced = root.join("niced.txt");
        let copied = root.join("copied.txt");
        // cargo: print what the script decided. The real one is never
        // reached — this test is about the environment it is handed.
        write_exec(
            &bin.join("cargo"),
            "#!/usr/bin/env bash\n\
             echo \"target=${CARGO_TARGET_DIR:-unset}\"\n\
             echo \"jobs=${CARGO_BUILD_JOBS:-unset}\"\n\
             echo \"argv=$*\"\n",
        );
        // nice: record that it was called and with what, then run the
        // rest exactly as the real one would.
        write_exec(
            &bin.join("nice"),
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> \"$STUB_NICED\"\n\
             shift 2\n\
             exec \"$@\"\n",
        );
        // cp: a reflink copy needs XFS and the test runs on whatever
        // scratch is (tmpfs, overlay). Record the call and make the
        // destination exist, which is all the script reads back. With
        // STUB_CP_FAIL set it dies PART-WAY the way a real cp does —
        // destination created, one file inside, nonzero exit — so a
        // test can see what the script leaves behind.
        write_exec(
            &bin.join("cp"),
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> \"$STUB_COPIED\"\n\
             mkdir -p \"${!#}\"\n\
             if [ -n \"${STUB_CP_FAIL:-}\" ]; then\n\
                 touch \"${!#}/half-copied\"\n\
                 exit \"$STUB_CP_FAIL\"\n\
             fi\n",
        );
        Self {
            root,
            bin,
            niced,
            copied,
        }
    }

    /// A real git repository whose basename is `name` — the script
    /// reads both through `git rev-parse --show-toplevel`.
    fn worktree(&self, name: &str) -> PathBuf {
        let wt = self.root.join("trees").join(name);
        boss_testing::create_dir(&wt);
        let out = Command::new("git")
            .args(["init", "-q"])
            .arg(&wt)
            .output()
            .expect("git init");
        assert!(
            out.status.success(),
            "git init {}: {}",
            wt.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        wt
    }

    fn targets(&self) -> PathBuf {
        self.root.join("targets")
    }

    fn run(&self, worktree: &Path, seed: &Path, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(["test", "-p", "boss-cli"])
            .current_dir(worktree)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_NICED", &self.niced)
            .env("STUB_COPIED", &self.copied)
            .env("WT_SEED", seed)
            .env("WT_TARGET_ROOT", self.targets())
            .env_remove("WT_JOBS")
            .env_remove("STUB_CP_FAIL")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_JOBS");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run wt-cargo");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    fn nice_calls(&self) -> String {
        std::fs::read_to_string(&self.niced).unwrap_or_default()
    }

    fn cp_calls(&self) -> String {
        std::fs::read_to_string(&self.copied).unwrap_or_default()
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/wt-cargo is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read wt-cargo");
    assert!(
        text.contains("pod-build.env"),
        "the bound must be sourced from infra/dev/pod-build.env, not retyped"
    );
    assert!(
        !text.contains("WT_JOBS:-6") && !text.contains("WT_JOBS:-8"),
        "no second literal for the operator bound — that is the drift this car closes"
    );
}

/// The operator's worktree: pod-build.env's bound, no nice, and — with
/// no seed to copy — a cold target dir that says it is cold.
#[test]
fn an_operator_worktree_builds_at_the_pod_bound_unniced() {
    let f = Fixture::new("operator");
    let wt = f.worktree("boss");
    let missing_seed = f.root.join("no-such-seed");

    let (rc, out) = f.run(&wt, &missing_seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let expected_target = f.targets().join("target-boss");
    assert!(
        out.contains(&format!("target={}", expected_target.display())),
        "CARGO_TARGET_DIR must be the per-worktree dir: {out}"
    );
    assert!(
        expected_target.is_dir(),
        "the cold dir must be created: {out}"
    );
    assert!(
        out.contains(&format!("jobs={}", env_file_bound())),
        "the bound must be pod-build.env's: {out}"
    );
    assert!(
        out.contains("argv=test -p boss-cli"),
        "cargo's argv must pass through: {out}"
    );
    assert!(
        out.contains("starts cold"),
        "a missing seed must be said, not silent: {out}"
    );
    assert_eq!(f.nice_calls(), "", "the operator's build is not niced");
    assert_eq!(f.cp_calls(), "", "no seed, no copy attempted");
}

/// A builder's worktree: 4-wide under `nice -n 10`, and — with a seed
/// present — the per-worktree dir is reflink-seeded from it.
#[test]
fn a_builder_worktree_builds_four_wide_and_niced_from_the_seed() {
    let f = Fixture::new("builder");
    let wt = f.worktree("agent-0123abcd");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let expected_target = f.targets().join("target-agent-0123abcd");
    assert!(
        out.contains(&format!("target={}", expected_target.display())),
        "CARGO_TARGET_DIR must be the per-worktree dir: {out}"
    );
    assert!(out.contains("jobs=4"), "a builder builds 4-wide: {out}");
    assert!(
        f.nice_calls().starts_with("-n 10 cargo"),
        "a builder's cargo runs under nice -n 10: {:?}",
        f.nice_calls()
    );
    let cp = f.cp_calls();
    assert!(
        cp.contains("--reflink=always")
            && cp.contains(seed.to_str().expect("utf8"))
            && cp.contains(expected_target.to_str().expect("utf8")),
        "the seed must be reflink-copied to the per-worktree dir: {cp:?}"
    );
    assert!(
        out.contains("seeded") && out.contains("by reflink"),
        "the seed step must be reported: {out}"
    );

    // Second run: the dir exists, so no second copy.
    let (rc, _) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0);
    assert_eq!(
        f.cp_calls().lines().count(),
        1,
        "an existing per-worktree dir is never re-seeded"
    );
}

/// `WT_JOBS` is the explicit override, and it wins in both branches.
#[test]
fn wt_jobs_wins_in_both_branches() {
    let f = Fixture::new("override");
    let missing_seed = f.root.join("no-such-seed");

    let operator = f.worktree("boss");
    let (rc, out) = f.run(&operator, &missing_seed, &[("WT_JOBS", "2")]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    assert!(
        out.contains("jobs=2"),
        "WT_JOBS wins for the operator: {out}"
    );
    assert_eq!(f.nice_calls(), "", "WT_JOBS does not change the nice rule");

    let builder = f.worktree("agent-ffff");
    let (rc, out) = f.run(&builder, &missing_seed, &[("WT_JOBS", "2")]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    assert!(out.contains("jobs=2"), "WT_JOBS wins for a builder: {out}");
    assert!(
        f.nice_calls().starts_with("-n 10 cargo"),
        "a builder is still niced under WT_JOBS: {:?}",
        f.nice_calls()
    );
}

/// A seed copy that dies part-way (backlog 08782b71): the half copy
/// must not become `$dir`, or the next run sees the directory, skips
/// the seed, and builds against it believing it warm. The script must
/// clear the temp dir, start `$dir` cold, say WHY (cp's exit code),
/// and still run cargo.
#[test]
fn a_failed_seed_leaves_no_half_target_and_starts_cold_saying_why() {
    let f = Fixture::new("failed-seed");
    let wt = f.worktree("agent-halfcopy");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[("STUB_CP_FAIL", "1")]);
    assert_eq!(rc, 0, "a failed seed is not a failed build: {out}");

    let target = f.targets().join("target-agent-halfcopy");
    let seeding = f.targets().join("target-agent-halfcopy.seeding");
    assert!(
        !seeding.exists(),
        "the temp dir must be removed after a failed copy: {out}"
    );
    assert!(target.is_dir(), "the dir must still exist, cold: {out}");
    assert!(
        !target.join("half-copied").exists(),
        "the half copy must not be renamed into place: {out}"
    );
    assert!(
        out.contains("starts cold"),
        "the existing cold message must be kept: {out}"
    );
    assert!(
        out.contains("cp exit 1"),
        "the message must name why the seed failed: {out}"
    );
    assert!(
        out.contains(&format!("target={}", target.display()))
            && out.contains("argv=test -p boss-cli"),
        "cargo must still run against the cold dir: {out}"
    );
}

/// A seed copy that succeeds lands in `$dir.seeding` and is renamed to
/// `$dir`; a stale `.seeding` left by a killed run is cleared before
/// the attempt, so nothing from it rides into the new target.
#[test]
fn a_successful_seed_is_renamed_into_place_over_a_stale_temp_dir() {
    let f = Fixture::new("renamed-seed");
    let wt = f.worktree("agent-rename");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);
    let target = f.targets().join("target-agent-rename");
    let seeding = f.targets().join("target-agent-rename.seeding");
    boss_testing::create_dir(&seeding);
    std::fs::write(seeding.join("stale-from-killed-run"), "").expect("stale marker");

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let cp = f.cp_calls();
    assert!(
        cp.trim_end().ends_with(seeding.to_str().expect("utf8")),
        "cp's destination must be the sibling temp dir, not $dir: {cp:?}"
    );
    assert!(
        target.is_dir(),
        "the seeded dir must be renamed into place: {out}"
    );
    assert!(
        !seeding.exists(),
        "no temp dir remains after a successful seed: {out}"
    );
    assert!(
        !target.join("stale-from-killed-run").exists(),
        "a stale .seeding is cleared before the attempt, not renamed in: {out}"
    );
    assert!(
        out.contains("seeded") && out.contains("by reflink"),
        "the seed step must be reported: {out}"
    );
}

/// Outside a git worktree there is no name to key the target dir on;
/// the script refuses rather than building into `target-`.
#[test]
fn outside_a_worktree_it_refuses() {
    let f = Fixture::new("nogit");
    let nowhere = f.root.join("plain-dir");
    boss_testing::create_dir(&nowhere);
    let (rc, out) = f.run(&nowhere, &f.root.join("no-such-seed"), &[]);
    assert_eq!(rc, 2, "must refuse with exit 2: {out}");
    assert!(out.contains("not in a git worktree"), "must say why: {out}");
    assert!(!out.contains("argv="), "cargo must not run: {out}");
}
