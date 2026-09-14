//! `infra/dev/boss` — the shim every `boss` verb on the dev pod runs
//! through. It decides WHICH built boss-cli binary answers, and
//! defaults `BOSS_JOBS_URL` to the system of record the way the
//! conductor's unit does (backlog f199c188).
//!
//! Until 2026-09-14 this was pod-local text under /work/tools/bin:
//! unversioned, untested, absent from any fresh pod — and a stale
//! binary had already answered `boss --version` wrong through it once.
//! The pod copy is the spec and its behaviour is preserved, not
//! redesigned; these tests pin it against a stub binary tree under
//! scratch (`BOSS_SHIM_TARGET_ROOT`), so nothing touches /scratch:
//!
//!   * with both profiles built, `target/release/boss` answers; with
//!     only a debug build, `target/debug/boss` does — the pod copy's
//!     fixed order, which is what "which binary wins" means today;
//!   * argv passes through untouched, spaces included;
//!   * `BOSS_JOBS_URL` is defaulted only when unset — a caller's
//!     value (the conductor's unit, a second-stack read) is kept;
//!   * with no binary at all the shim refuses with exit 127 and names
//!     the rebuild command, instead of a bare "command not found".

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/dev/boss";

/// The system of record; the shim's default when the caller sets none.
/// The same literal the conductor's unit and the maintenance units
/// carry — the packet excludes changing it.
const SOR: &str = "http://10.20.0.34:7900";

struct Fixture {
    /// Stands in for /scratch: the shim looks under `<root>/target`.
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self {
            root: scratch_dir(&format!("boss-shim-{name}")),
        }
    }

    /// A stub `boss` under `target/<profile>/`. It prints which profile
    /// answered, the jobs URL it was handed, and its argv one per line,
    /// so a test can read every decision the shim made.
    fn build(&self, profile: &str) {
        let dir = self.root.join("target").join(profile);
        boss_testing::create_dir(&dir);
        write_exec(
            &dir.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 echo \"profile={profile}\"\n\
                 echo \"jobs_url=${{BOSS_JOBS_URL:-unset}}\"\n\
                 for a in \"$@\"; do echo \"arg=$a\"; done\n"
            ),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .env("BOSS_SHIM_TARGET_ROOT", &self.root)
            .env_remove("BOSS_JOBS_URL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run the boss shim");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }
}

#[test]
fn the_shim_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/boss is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read the boss shim");
    assert!(
        text.contains("BOSS_SHIM_TARGET_ROOT"),
        "the pod path must be read from the environment so a test never touches /scratch"
    );
    assert!(
        text.contains("cargo build -p boss-cli"),
        "the refusal must name the rebuild command, as the pod copy's did"
    );
}

/// The pod copy's order: release first, then debug. Both built, release
/// answers; only debug built, debug answers.
#[test]
fn release_answers_over_debug_and_debug_answers_alone() {
    let f = Fixture::new("order");
    f.build("debug");
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("profile=debug"),
        "with only a debug build, debug answers: {out}"
    );

    f.build("release");
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("profile=release"),
        "with both built, release answers: {out}"
    );
}

/// Every argument reaches the binary as one argument — a reason with
/// spaces in it is one argv entry, not three.
#[test]
fn argv_passes_through_untouched() {
    let f = Fixture::new("argv");
    f.build("debug");
    let (rc, out) = f.run(&["train", "cancel", "--reason", "two words here"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    let args: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("arg=")).collect();
    assert_eq!(
        args,
        ["train", "cancel", "--reason", "two words here"],
        "argv must pass through one-for-one: {out}"
    );
}

/// The jobs URL is the system of record unless the caller already
/// chose one; the shim never overrides a caller's target.
#[test]
fn the_jobs_url_defaults_to_the_sor_only_when_unset() {
    let f = Fixture::new("jobs-url");
    f.build("debug");

    let (rc, out) = f.run(&["orient"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains(&format!("jobs_url={SOR}")),
        "unset, the jobs URL defaults to the system of record: {out}"
    );

    let second_stack = "http://127.0.0.1:7900";
    let (rc, out) = f.run(&["orient"], &[("BOSS_JOBS_URL", second_stack)]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains(&format!("jobs_url={second_stack}")),
        "a caller's jobs URL is kept, not overridden: {out}"
    );
}

/// No built binary under the root: exit 127 (the shell's own "command
/// not found" code) with a line that names where it looked and the
/// command that rebuilds it — the failure the pod copy already named.
#[test]
fn with_no_binary_it_refuses_and_names_the_rebuild() {
    let f = Fixture::new("no-binary");
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 127, "must refuse with exit 127: {out}");
    assert!(
        out.contains("no built binary")
            && out.contains(f.root.join("target").to_str().expect("utf8")),
        "the refusal must say where it looked: {out}"
    );
    assert!(
        out.contains("cargo build -p boss-cli"),
        "the refusal must name the rebuild command: {out}"
    );
    assert!(!out.contains("profile="), "nothing must be exec'd: {out}");
}
