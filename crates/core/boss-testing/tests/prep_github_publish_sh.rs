//! `infra/prep-github-publish.sh` is RUN, not read — against a scratch
//! repository, so each verdict below is one the script actually made.
//!
//! THE DEFECT (design packet 7b59af2c, measured 2026-09-08). With a
//! SOURCE_REF that did not resolve, the script's `git rev-list` calls
//! failed silently under `|| echo 0`, so `commits ahead` read 0 and the
//! script exited 0 with `has_drift:false` — "the mirror is current" —
//! while the mirror was 211 commits behind. A wrong target answering
//! instead of erroring (CLAUDE.md §Doors), and on the protocol's
//! `measure` step it is the measurement that closes the packet
//! `nothing-to-publish`. So: a ref that does not resolve is a REFUSAL,
//! non-zero, naming the ref, and the drift path is never reached.
//!
//! The mirror is read through a remote named `github` by default —
//! read-only, anonymous (the repo is public), so measuring needs no
//! credential anywhere.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn script() -> PathBuf {
    repo_root().join("infra/prep-github-publish.sh")
}

/// A scratch directory per case, so cases cannot see each other's
/// fixtures.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prep-github-publish-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A working repo with one commit on `main`, and — when `with_mirror`
/// — a bare repo holding the same commit, wired as the `github` remote.
fn fixture(case: &str, with_mirror: bool) -> PathBuf {
    let root = scratch(case);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("README"), "hello\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-q", "-m", "first"]);
    if with_mirror {
        let mirror = root.join("mirror.git");
        git(&root, &["init", "-q", "--bare", "-b", "main", "mirror.git"]);
        git(
            &work,
            &["remote", "add", "github", mirror.to_str().unwrap()],
        );
        git(&work, &["push", "-q", "github", "main:main"]);
    }
    work
}

fn run(work: &Path, source_ref: &str, json: bool) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .current_dir(work)
        .env("SOURCE_REF", source_ref)
        .env("GITHUB_REMOTE", "github")
        .env("GITHUB_BRANCH", "main");
    if json {
        cmd.arg("--json");
    }
    cmd.output().expect("the script runs")
}

/// THE ONE THAT WAS WRONG BEFORE.
#[test]
fn a_source_ref_that_does_not_resolve_is_refused_by_name() {
    let work = fixture("bogus-source", true);
    let out = run(&work, "nowhere/does-not-exist", true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "a source ref that does not resolve must be a non-zero refusal, not a measurement; \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("nowhere/does-not-exist"),
        "the refusal must name the ref that did not resolve: {stderr}"
    );
    assert!(
        !stdout.contains("\"has_drift\":false"),
        "a refusal must never read as `has_drift:false` — that is the measurement that \
         closes the packet nothing-to-publish: {stdout}"
    );
}

/// The other half of the pair: the mirror side. No `github` remote
/// means the mirror ref cannot resolve either, and the refusal names
/// what is missing.
#[test]
fn a_missing_mirror_remote_is_refused_by_name() {
    let work = fixture("no-mirror", false);
    let out = run(&work, "main", true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "an unresolvable mirror ref must refuse; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("github/main") && stderr.contains("github"),
        "the refusal must name the mirror ref and its remote: {stderr}"
    );
    assert!(!stdout.contains("\"has_drift\":false"), "{stdout}");
}

/// When both refs resolve and agree, the measurement still says so —
/// the refusal did not swallow the honest "nothing to publish".
#[test]
fn a_current_mirror_still_measures_as_nothing_to_publish() {
    let work = fixture("current", true);
    let out = run(&work, "main", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "both refs resolve and match: exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("\"has_drift\":false"),
        "a current mirror measures as no drift: {stdout}"
    );
}
