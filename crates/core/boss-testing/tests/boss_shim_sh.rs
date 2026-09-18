//! `infra/dev/boss` — the shim every `boss` verb on the dev pod runs
//! through. It decides WHICH boss-cli binary answers, and defaults
//! `BOSS_JOBS_URL` to the system of record the way the conductor's
//! unit does (backlog f199c188).
//!
//! Until 2026-09-14 this was pod-local text under /work/tools/bin:
//! unversioned, untested, absent from any fresh pod — and a stale
//! binary had already answered `boss --version` wrong through it once.
//!
//! Since 2026-09-18 (backlog c35eda6c, retro 27fad542) the shim knows
//! the AGE of what it runs. The retro counted the pod's CLI rebuilt by
//! hand three times in one window because a landed car had changed
//! what the CLI validates locally, and a stale build refused a valid
//! four-argument verb (`takes 3 arg(s), got 4`); the shim exec'd it and
//! said nothing. Now two candidates are weighed against the checkout's
//! `origin/main`:
//!
//!   * the IMAGE CLI — `<store>/<sha>/boss` + `current`, installed by
//!     the reclaim sidecar's hourly pass out of the cluster image with
//!     infra/estate/install-cli-from-image.sh (the tree's CLI by
//!     construction), run through the estate's boss-cli-wrapper.sh so
//!     it names its commit;
//!   * the BUILD — the newer of target/release/boss and
//!     target/debug/boss by mtime, whose `--version` names the commit it
//!     was compiled from.
//!
//! The image CLI at origin/main wins. A build at or after origin/main
//! runs silently. A candidate BEHIND origin/main (or one that cannot
//! say what it was built from) runs a read verb under a one-line
//! WARNING naming both shas, and is REFUSED for a write verb — the
//! packet's fix (a): a refusal names the install and the rebuild, a
//! silent stale binary names nothing. `--built` (or BOSS_SHIM_BUILT)
//! asks for the build outright — a developer testing a branch — and the
//! same age rule still applies to it.
//!
//! Every test plants its own image store (`BOSS_SHIM_IMAGE_STORE`) and
//! build tree (`BOSS_SHIM_TARGET_ROOT`) under scratch, so nothing here
//! touches /work/tools or /scratch. The shas are the real checkout's:
//! `origin/main` and its parent, read from git, never typed.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/boss";
const WRAPPER: &str = "infra/estate/boss-cli-wrapper.sh";

/// The one spelling, read from infra/dev/sor-url — the file boss-api
/// reads too. Read here rather than typed so the test cannot drift
/// from it.
fn sor() -> String {
    std::fs::read_to_string(repo_root().join("infra/dev/sor-url"))
        .expect("infra/dev/sor-url")
        .trim()
        .to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@test",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `origin/main` as this checkout knows it — what the shim compares
/// against — and its parent, a sha that is truthfully BEHIND it.
fn main_sha() -> String {
    git(
        &repo_root(),
        &["rev-parse", "--verify", "refs/remotes/origin/main"],
    )
}
fn behind_sha() -> String {
    git(
        &repo_root(),
        &["rev-parse", "--verify", "refs/remotes/origin/main~1"],
    )
}

/// A stub `boss` that prints what a real one would on `--version`
/// (`boss 0.1.0 built from <sha>`, built_from.rs) and, for every call,
/// which candidate answered, the jobs URL it was handed and its argv
/// one per line — so a test can read every decision the shim made.
/// `built_from` empty means "reads the commit from BOSS_BUILD_COMMIT",
/// the image binary's behaviour.
fn stub(path: &Path, who: &str, built_from: &str) {
    let sha = if built_from.is_empty() {
        "${BOSS_BUILD_COMMIT:-unknown}".to_string()
    } else {
        built_from.to_string()
    };
    write_exec(
        path,
        &format!(
            "#!/usr/bin/env bash\n\
             echo \"boss 0.1.0 built from {sha}\"\n\
             echo \"ran={who}\"\n\
             echo \"jobs_url=${{BOSS_JOBS_URL:-unset}}\"\n\
             for a in \"$@\"; do echo \"arg=$a\"; done\n"
        ),
    );
}

struct Fixture {
    /// Stands in for /scratch: the shim looks under `<root>/target`.
    root: PathBuf,
    /// Stands in for /work/tools/image-cli.
    store: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("boss-shim-{name}"));
        let store = root.join("image-cli");
        Self { root, store }
    }

    /// A build under `target/<profile>/boss` that says it was built
    /// from `sha`.
    fn build(&self, profile: &str, sha: &str) {
        let dir = self.root.join("target").join(profile);
        boss_testing::create_dir(&dir);
        stub(&dir.join("boss"), &format!("build-{profile}"), sha);
    }

    /// An image generation at `sha`, laid out the way
    /// install-cli-from-image.sh leaves it: `<store>/<sha>/boss`,
    /// `current -> <sha>`, and the REAL wrapper at `<store>/boss` — the
    /// one definition of how the image's binary learns its commit.
    fn image(&self, sha: &str) {
        let generation = self.store.join(sha);
        boss_testing::create_dir(&generation);
        stub(&generation.join("boss"), "image", "");
        let current = self.store.join("current");
        let _ = std::fs::remove_file(&current);
        std::os::unix::fs::symlink(sha, &current).expect("link current");
        write_exec(
            &self.store.join("boss"),
            &std::fs::read_to_string(repo_root().join(WRAPPER)).expect("read the wrapper"),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .env("BOSS_SHIM_TARGET_ROOT", &self.root)
            .env("BOSS_SHIM_IMAGE_STORE", &self.store)
            .env_remove("BOSS_JOBS_URL")
            .env_remove("BOSS_SHIM_BUILT")
            .env_remove("BOSS_SHIM_VERBOSE");
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

fn short(sha: &str) -> &str {
    &sha[..8]
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
    for must in ["BOSS_SHIM_TARGET_ROOT", "BOSS_SHIM_IMAGE_STORE"] {
        assert!(
            text.contains(must),
            "{must}: both roots must be read from the environment so a test never touches /scratch or /work/tools"
        );
    }
    assert!(
        text.contains("cargo build -p boss-cli"),
        "the refusal must name the rebuild command, as the pod copy's did"
    );
    assert!(
        text.contains("built from"),
        "the shim reads what a candidate was built from — the packet's fix (a)"
    );
}

/// THE IMAGE CLI AT ORIGIN/MAIN WINS, even over a build that is also
/// current: it is the tree's CLI by construction. It runs through the
/// wrapper, so it names its commit; with BOSS_SHIM_VERBOSE set the shim
/// says which candidate it ran and at what sha.
#[test]
fn the_image_cli_at_origin_main_is_execd_and_named() {
    let f = Fixture::new("image-current");
    let main = main_sha();
    f.image(&main);
    f.build("debug", &main);
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("ran=image") && out.contains(&format!("built from {main}")),
        "the image CLI answers and names its commit through the wrapper: {out}"
    );
    assert!(!out.contains("WARNING"), "current is silent: {out}");

    let (rc, out) = f.run(&["orient"], &[("BOSS_SHIM_VERBOSE", "1")]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("boss: running the image CLI")
            && out.contains(short(&main))
            && out.contains("= origin/main"),
        "verbose names the candidate and its sha: {out}"
    );
}

/// A STALE BUILD, and no image CLI: a read verb runs under ONE warning
/// line that names the build's sha, origin/main's, and the fix; a write
/// verb is refused (exit 78, publish-workflow's floor code) with the
/// same facts, and nothing is exec'd. Every argument still passes
/// through on the read.
#[test]
fn a_build_behind_origin_main_warns_on_a_read_and_refuses_a_write() {
    let f = Fixture::new("stale-build");
    let main = main_sha();
    let behind = behind_sha();
    f.build("debug", &behind);

    let (rc, out) = f.run(&["orient", "--since", "two words"], &[]);
    assert_eq!(rc, 0, "a read runs: {out}");
    let warning = out
        .lines()
        .find(|l| l.contains("WARNING"))
        .unwrap_or_else(|| panic!("one WARNING line on a read: {out}"));
    for must in [
        short(&behind),
        short(&main),
        "behind",
        "dev-scratch-reclaim.sh --cli",
        "cargo build -p boss-cli",
    ] {
        assert!(
            warning.contains(must),
            "the warning names {must}: {warning}"
        );
    }
    assert!(out.contains("ran=build-debug"), "the read still ran: {out}");
    let args: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("arg=")).collect();
    assert_eq!(
        args,
        ["orient", "--since", "two words"],
        "argv intact: {out}"
    );

    for write in [
        vec!["gate", "fix/x", "--wait"],
        vec!["ops", "sweep", "a", "b", "c", "d"],
        vec!["triage", "abc"],
    ] {
        let (rc, out) = f.run(&write, &[]);
        assert_eq!(rc, 78, "a write behind main is refused ({write:?}): {out}");
        assert!(
            out.contains("REFUSED") && out.contains(short(&behind)) && out.contains(short(&main)),
            "the refusal names both shas ({write:?}): {out}"
        );
        assert!(
            out.contains("dev-scratch-reclaim.sh --cli") && out.contains("cargo build -p boss-cli"),
            "the refusal names the install and the rebuild ({write:?}): {out}"
        );
        assert!(
            !out.contains("ran="),
            "nothing is exec'd ({write:?}): {out}"
        );
    }

    // `--help` on a write verb is a read.
    let (rc, out) = f.run(&["gate", "--help"], &[]);
    assert_eq!(rc, 0, "help is a read: {out}");
    assert!(out.contains("ran=build-debug"), "{out}");
}

/// An image CLI that is itself behind (the hourly pass has not caught
/// up) is weighed the same way as a build: read with a warning, write
/// refused. With both candidates behind, the newer by ancestry answers.
#[test]
fn a_stale_image_cli_is_weighed_like_a_stale_build() {
    let f = Fixture::new("stale-image");
    let main = main_sha();
    let behind = behind_sha();
    f.image(&behind);
    let (rc, out) = f.run(&["orient"], &[]);
    assert_eq!(rc, 0, "a read runs: {out}");
    assert!(
        out.contains("WARNING") && out.contains("ran=image") && out.contains(short(&behind)),
        "{out}"
    );
    let (rc, out) = f.run(&["gate", "x"], &[]);
    assert_eq!(rc, 78, "a write is refused: {out}");

    // The build is at main: it is current, and answers over the stale image.
    f.build("debug", &main);
    let (rc, out) = f.run(&["gate", "x"], &[]);
    assert_eq!(rc, 0, "a current build answers a write: {out}");
    assert!(
        out.contains("ran=build-debug") && !out.contains("WARNING"),
        "{out}"
    );
}

/// A build that cannot say what it was built from — `built from
/// unknown`, or a `--version` naming no commit — is not assumed
/// current: the same warning/refusal, worded for it.
#[test]
fn a_build_that_cannot_say_what_it_was_built_from_is_not_assumed_current() {
    let f = Fixture::new("unknown-build");
    f.build("debug", "unknown");
    let (rc, out) = f.run(&["brief", "abc"], &[]);
    assert_eq!(rc, 0, "a read runs: {out}");
    assert!(
        out.contains("WARNING") && out.contains("cannot say what it was built from"),
        "{out}"
    );
    let (rc, out) = f.run(&["gate", "x"], &[]);
    assert_eq!(rc, 78, "a write is refused: {out}");
    assert!(out.contains("cannot say what it was built from"), "{out}");
}

/// A BUILD AHEAD OF ORIGIN/MAIN — a developer's branch build — runs
/// silently, writes included, and `--built` asks for it over an image
/// CLI that is current. Ahead needs a commit that is not in the real
/// checkout, so the shim is copied (with its sor-url) into a scratch
/// repository whose origin/main is one commit behind HEAD; the shim
/// reads origin/main from its own checkout, so the copy reads this one.
#[test]
fn a_build_ahead_of_origin_main_runs_silently_and_built_asks_for_it() {
    let f = Fixture::new("ahead");
    let repo = f.root.join("repo");
    boss_testing::create_dir(&repo.join("infra/dev"));
    git(&repo, &["init", "-q"]);
    boss_testing::write_file(&repo.join("README"), "one\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let main = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", &main]);
    boss_testing::write_file(&repo.join("README"), "two\n");
    git(&repo, &["commit", "-q", "-am", "two"]);
    let ahead = git(&repo, &["rev-parse", "HEAD"]);
    let copy = repo.join("infra/dev/boss");
    write_exec(
        &copy,
        &std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap(),
    );
    boss_testing::write_file(&repo.join("infra/dev/sor-url"), &format!("{}\n", sor()));
    f.image(&main);
    f.build("debug", &ahead);

    let run = |args: &[&str], env: &[(&str, &str)]| {
        let mut cmd = Command::new(&copy);
        cmd.args(args)
            .env("BOSS_SHIM_TARGET_ROOT", &f.root)
            .env("BOSS_SHIM_IMAGE_STORE", &f.store)
            .env_remove("BOSS_JOBS_URL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run the copied shim");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };

    // Both current: the image CLI wins by default …
    let (rc, out) = run(&["gate", "x"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("ran=image") && !out.contains("WARNING"),
        "{out}"
    );
    // … and the developer asks for the build with --built, which is
    // consumed, or with BOSS_SHIM_BUILT.
    let (rc, out) = run(&["--built", "gate", "x"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("ran=build-debug") && !out.contains("WARNING"),
        "an ahead build runs a write silently: {out}"
    );
    let args: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("arg=")).collect();
    assert_eq!(
        args,
        ["gate", "x"],
        "--built is the shim's, not the CLI's: {out}"
    );
    let (rc, out) = run(
        &["gate", "x"],
        &[("BOSS_SHIM_BUILT", "1"), ("BOSS_SHIM_VERBOSE", "1")],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("ran=build-debug")
            && out.contains("boss: running the build")
            && out.contains(short(&ahead)),
        "{out}"
    );
}

/// THE NEWER BUILD ANSWERS. The pod copy took release over debug in a
/// fixed order, so a stale release binary would beat a fresh debug one
/// forever — the mechanism behind the wrong `boss --version` the
/// built_from car chased (2026-09-14). Only debug built: debug answers.
/// Both built, release older: debug answers. Release rebuilt newer:
/// release answers.
#[test]
fn the_newer_build_answers_whatever_its_profile() {
    let f = Fixture::new("newest");
    let main = main_sha();
    f.build("release", &main);
    let old = f.root.join("target/release/boss");
    // Age the release build by an hour so mtime order is unambiguous.
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    f.build("debug", &main);
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("ran=build-debug"),
        "a fresh debug build beats a stale release one: {out}"
    );

    // Rebuild release now: it is the newer of the two and answers.
    f.build("release", &main);
    let now_plus = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(now_plus)
        .unwrap();
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains("ran=build-release"),
        "rebuilt newer, release answers: {out}"
    );
}

/// A shim copied away from its `sor-url` has no default to invent.
#[test]
fn without_sor_url_beside_it_the_shim_refuses_rather_than_guess() {
    let f = Fixture::new("no-sor-url");
    f.build("debug", &main_sha());
    let copy_dir = scratch_dir("boss-shim-copy");
    let copy = copy_dir.join("boss");
    write_exec(
        &copy,
        &std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap(),
    );
    let out = Command::new(&copy)
        .arg("--version")
        .env("BOSS_SHIM_TARGET_ROOT", &f.root)
        .env("BOSS_SHIM_IMAGE_STORE", &f.store)
        .env_remove("BOSS_JOBS_URL")
        .output()
        .expect("run the copied shim");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("sor-url") && err.contains("BOSS_JOBS_URL"),
        "the refusal names the file and the override: {err}"
    );
}

/// Every argument reaches the binary as one argument — a reason with
/// spaces in it is one argv entry, not three.
#[test]
fn argv_passes_through_untouched() {
    let f = Fixture::new("argv");
    f.build("debug", &main_sha());
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
    f.build("debug", &main_sha());

    let (rc, out) = f.run(&["orient"], &[]);
    assert_eq!(rc, 0, "the shim failed: {out}");
    assert!(
        out.contains(&format!("jobs_url={}", sor())),
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

/// No candidate at all: exit 127 (the shell's own "command not found"
/// code) with a line that names where it looked — both roots — the
/// install that the hourly pass runs, and the command that rebuilds.
#[test]
fn with_no_binary_it_refuses_and_names_the_install_and_the_rebuild() {
    let f = Fixture::new("no-binary");
    let (rc, out) = f.run(&["--version"], &[]);
    assert_eq!(rc, 127, "must refuse with exit 127: {out}");
    assert!(
        out.contains("no built binary")
            && out.contains(f.root.join("target").to_str().expect("utf8"))
            && out.contains(f.store.to_str().expect("utf8")),
        "the refusal must say where it looked: {out}"
    );
    assert!(
        out.contains("cargo build -p boss-cli") && out.contains("dev-scratch-reclaim.sh --cli"),
        "the refusal must name the rebuild and the install: {out}"
    );
    assert!(!out.contains("ran="), "nothing must be exec'd: {out}");
}
