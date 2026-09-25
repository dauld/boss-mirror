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

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    repo_root().join("infra/prep-github-publish.sh")
}

/// A scratch directory per case, so cases cannot see each other's
/// fixtures.
fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("prep-github-publish-{case}"))
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

/// The address file the script reads (infra/lib/sor.sh): on a host the
/// install renders /etc/boss/sor.env from infra/estate/estate.toml;
/// here the same render into the scratch root, so the missing-remote
/// refusal names the mirror the one source declares rather than a
/// literal it used to carry (backlog f8af6040).
fn sor_env(work: &Path) -> PathBuf {
    let dest = work.join("sor.env");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .arg("--to")
        .arg(&dest)
        .output()
        .expect("render-sor-env.sh runs");
    assert!(
        out.status.success(),
        "could not render the address file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dest
}

/// The mirror's URL as the ONE source declares it — read the way the
/// renderer reads it, never spelled in this test.
fn declared_mirror_url() -> String {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .args(["--value", "BOSS_MIRROR_URL"])
        .output()
        .expect("render-sor-env.sh runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run(work: &Path, source_ref: &str, json: bool) -> Output {
    run_with(work, source_ref, json, &[])
}

fn run_with(work: &Path, source_ref: &str, json: bool, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .current_dir(work)
        .env("BOSS_SOR_ENV", sor_env(work))
        .env("SOURCE_REF", source_ref)
        .env("GITHUB_REMOTE", "github")
        .env("GITHUB_BRANCH", "main");
    for (k, v) in envs {
        cmd.env(k, v);
    }
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
    // And the remote it tells the operator to add is the one the estate
    // DECLARES. Until 2026-09-20 this sentence carried a literal copy of
    // the URL, so a move would have left it naming a remote that no
    // longer exists (backlog f8af6040, CLAUDE.md 9a).
    let url = declared_mirror_url();
    assert!(
        stderr.contains(&format!("{url}.git")),
        "the refusal must name the declared mirror ({url}.git): {stderr}"
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

// ---------------------------------------------------------------------
// THE SECRETS SCAN READS THE PUBLISHED TREE (backlog 63c82d2f).
//
// Measured 2026-09-25 00:17Z by run 701ccb98: the dev pod checkout was
// 2 commits (18 files) behind origin/main, and the script measured the
// drift AT SOURCE_REF while it ran the secrets lint over the WORKING
// TREE. Its `clean` vouched for a tree nobody was publishing, and the
// publish is not reversible. The cases below put the secret on one side
// only, so a scan of the wrong tree answers wrongly in each direction.
// ---------------------------------------------------------------------

/// The lint files the scan needs, copied from this tree into the
/// fixture's commit — so the fixture's ref carries its own lint, the way
/// the real SOURCE_REF does.
const LINT_FILES: [&str; 3] = [
    "infra/lint/no-secrets.sh",
    "infra/lint/lib/git-answer.sh",
    "infra/lint/lib/scanned.sh",
];

/// A credential-shaped line (the lint's `token-assignment` shape), built
/// at run time so this source file holds no line the tree scan would
/// flag — it scans this file too.
fn planted_secret() -> String {
    format!(
        "{}{}: \"{}\"\n",
        "api",
        "_key",
        "0123456789abcdef".repeat(4)
    )
}

fn write(work: &Path, rel: &str, body: &str) {
    let path = work.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn head(work: &Path, rev: &str) -> String {
    String::from_utf8_lossy(&git(work, &["rev-parse", rev]).stdout)
        .trim()
        .to_string()
}

/// `fixture`, plus the lint committed on `main` and pushed to the
/// mirror, and a `publish-src` branch one commit ahead carrying `files`.
/// The working tree is left on `main`, so the two trees differ.
fn fixture_with_source(case: &str, files: &[(&str, String)]) -> PathBuf {
    let work = fixture(case, true);
    for rel in LINT_FILES {
        let body = std::fs::read_to_string(repo_root().join(rel)).unwrap();
        write(&work, rel, &body);
    }
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "the lint"]);
    git(&work, &["push", "-q", "github", "main:main"]);
    git(&work, &["checkout", "-q", "-b", "publish-src"]);
    for (rel, body) in files {
        write(&work, rel, body);
    }
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "the change to publish"]);
    git(&work, &["checkout", "-q", "main"]);
    work
}

/// THE DEFECT. A secret present only at SOURCE_REF — the working tree is
/// clean — must be caught, and the JSON must name the commit whose tree
/// was scanned.
#[test]
fn a_secret_only_in_the_source_ref_is_caught() {
    let work = fixture_with_source("secret-in-ref", &[("leak.env", planted_secret())]);
    assert!(
        !work.join("leak.env").exists(),
        "fixture: the working tree must not hold the secret"
    );
    let source = head(&work, "publish-src");
    let out = run(&work, "publish-src", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a secret on the published tree is a blocking finding; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("\"secrets_scan\":\"FAILED\""),
        "the scan must read SOURCE_REF's tree, not the clean working tree: {stdout}"
    );
    assert!(
        stdout.contains(&format!("\"scanned_sha\":\"{source}\"")),
        "the JSON must record the commit whose tree was scanned ({source}): {stdout}"
    );
}

/// The other direction: a secret only in the working tree (tracked, so
/// the lint would read it there) is not the published tree's, and must
/// not block a publish of a clean ref.
#[test]
fn a_secret_only_in_the_working_tree_is_not_judged_as_the_refs() {
    let work = fixture_with_source("secret-in-worktree", &[("NOTES", "clean\n".to_string())]);
    write(&work, "leak.env", &planted_secret());
    git(&work, &["add", "leak.env"]);
    let source = head(&work, "publish-src");
    let out = run(&work, "publish-src", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "SOURCE_REF's tree is clean: exit 0; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("\"secrets_scan\":\"clean\""),
        "the working tree's secret is not on the published tree: {stdout}"
    );
    assert!(
        stdout.contains(&format!("\"scanned_sha\":\"{source}\"")),
        "the JSON must record the commit whose tree was scanned ({source}): {stdout}"
    );
}

/// The extraction must BE the ref's tree. `git archive` honours an
/// `export-ignore` attribute committed in the tree it archives, so a
/// file can be published without ever being scanned; the scanned tree
/// is hashed and compared with SOURCE_REF's, and a difference is a
/// refusal, never a clean.
#[test]
fn a_scanned_tree_that_is_not_the_refs_is_refused() {
    let work = fixture_with_source(
        "export-ignore",
        &[
            (".gitattributes", "leak.env export-ignore\n".to_string()),
            ("leak.env", planted_secret()),
        ],
    );
    let out = run(&work, "publish-src", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a scan of a tree other than SOURCE_REF's must refuse; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("\"refused\"") && !stdout.contains("\"secrets_scan\":\"clean\""),
        "the refusal must never read as a clean scan: {stdout}"
    );
    assert!(
        stderr.contains("publish-src"),
        "the refusal must name the ref: {stderr}"
    );
}

/// SOURCE_REF is resolved ONCE; if it names a different commit by the
/// time the scan finishes (a concurrent `git fetch` moves origin/main
/// under every worktree of the clone), the verdict vouches for a commit
/// the ref no longer names — refused. The ref's own lint moves the ref,
/// which is the one hook a test has inside the scan's window.
#[test]
fn a_source_ref_that_moves_during_the_scan_is_refused() {
    let stub = "#!/usr/bin/env bash\n\
                git -C \"$MOVE_REF_IN\" update-ref refs/heads/publish-src \"$MOVE_REF_TO\"\n\
                exit 0\n";
    let work = fixture_with_source(
        "moved-ref",
        &[("infra/lint/no-secrets.sh", stub.to_string())],
    );
    let main_sha = head(&work, "main");
    let out = run_with(
        &work,
        "publish-src",
        true,
        &[
            ("MOVE_REF_IN", work.to_str().unwrap()),
            ("MOVE_REF_TO", &main_sha),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        head(&work, "publish-src"),
        main_sha,
        "fixture: the stub lint must have moved the ref; stderr={stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a ref that moved under the scan must refuse; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("publish-src") && stderr.contains(&main_sha),
        "the refusal must name the ref and where it moved to: {stderr}"
    );
}
