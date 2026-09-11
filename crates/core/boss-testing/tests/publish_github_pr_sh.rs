//! `infra/forge/publish-github-pr.sh --check` is RUN, not read — against
//! fixture paths, a fixture compose file and (where this box is root) a
//! second uid, so every verdict below is one the script actually reached.
//!
//! THE DEFECT (backlog ed84b5d9, measured 2026-09-11). Two faults in one
//! refusal:
//!
//! 1. The verb's `BOSS_FORGE_REPO_PATH` default —
//!    `/opt/forgejo/data/git/repositories/david/boss.git` — appeared
//!    EXACTLY ONCE in the tree, as its own default, with the only other
//!    references being test overrides that substitute a tmpdir. It was
//!    authored from a plausible Forgejo layout and had never been run
//!    against the real host: ops-request 04975694 ran `--check` on the
//!    forge and the repo path was the one and only failure.
//! 2. `check_inputs` tested the path with one `git rev-parse` and
//!    reported `forge repository not found at <path>` — the SAME sentence
//!    whether the path was absent, was not a directory, was unreadable by
//!    the caller, or was readable but refused by git. CLAUDE.md §Doors
//!    ("a wrong target answers instead of erroring") in its most literal
//!    form, and CLAUDE.md §Diagnosis: a verdict must name what failed.
//!
//! 3. And the fix for (2) left the door still broken, because the check
//!    it added read the repository IN ITS OWN PROCESS (`git -c
//!    safe.directory=<repo> -C <repo> rev-parse`) while the publish
//!    FETCHES from it. `-c` cannot exempt a fetch SOURCE: a local fetch
//!    runs `git upload-pack` inside the source repository and git clears
//!    the command-line config crossing into it — its own trace reads
//!    `unset GIT_CONFIG_PARAMETERS … git-upload-pack '<src>'`. So on
//!    2026-09-11 `--check` returned ok at 13:41 and the publish FAILED at
//!    13:43 on the same host with "detected dubious ownership"
//!    (ops-request c258d3b7), and the mirror sat 271 commits behind under
//!    a green check. A `--check` that passes where the operation fails is
//!    worse than no `--check`, so the exemption now travels through a
//!    protected channel (a config file named by `GIT_CONFIG_GLOBAL`,
//!    which the child inherits) and `--check` PERFORMS the fetch.
//!
//! So this file pins FIVE distinct refusals and the derivation that
//! replaces the authored default: the path now comes from the compose
//! file that DECLARES the host directory mounted at the container's
//! `/data` (§9a — one definition, not a second copy in a shell default).
//!
//! Nothing here touches the network, the forge, or a token: the token
//! fixture holds a fixed non-secret string and is never printed.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn script() -> PathBuf {
    repo_root().join("infra/forge/publish-github-pr.sh")
}

fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a leftover
    // cannot be cleared — see `boss_testing::scratch`.
    let dir = boss_testing::scratch_dir(&format!("publish-github-pr-{case}"));
    // Traversable by the second uid the ownership cases drop to.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 0755 {}: {e}", dir.display()));
    dir
}

/// A bare repository WITH a `refs/heads/main`, because that is the ref
/// the verb fetches. A fixture without one is not a stand-in for the
/// forge — and an empty bare repository is exactly the fixture that let
/// the `-c`-only check look right: `rev-parse --git-dir` answered and no
/// fetch was ever attempted.
fn git_init_bare(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let st = Command::new("git")
        .args(["init", "-q", "--bare", path.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success(), "git init --bare {}", path.display());
    let tree = git_in(path, &["hash-object", "-t", "tree", "-w", "--stdin"]);
    let commit = git_in(path, &["commit-tree", &tree, "-m", "seed"]);
    git_in(path, &["update-ref", "refs/heads/main", &commit]);
}

/// One git command in `dir`, with an identity so `commit-tree` works, and
/// its trimmed stdout. Panics with git's own words — never a bare status.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// THE NEGATIVE CONTROL, measured in the test rather than assumed: one
/// raw fetch from `src` as `uid`, exempting the source the way the script
/// USED to (`-c safe.directory=<src>`) or not at all. A test that asserts
/// the verb now succeeds proves nothing unless the same fetch, from the
/// same fixture, as the same uid, still FAILS without the protected
/// channel — so this is what tells those two apart.
fn control_fetch(root: &Path, src: &Path, uid: u32, exempt_via_c: bool) -> (bool, String) {
    let pen = root.join("control");
    std::fs::create_dir_all(&pen).unwrap();
    std::fs::set_permissions(&pen, std::fs::Permissions::from_mode(0o777)).unwrap();
    let dst = pen.join(if exempt_via_c {
        "via-c.git"
    } else {
        "bare.git"
    });
    let _ = std::fs::remove_dir_all(&dst);
    let exempt = if exempt_via_c {
        "-c \"safe.directory=$SRC\""
    } else {
        ""
    };
    let body = format!(
        "git init -q --bare \"$DST\" || exit 9\n\
         exec git -C \"$DST\" {exempt} fetch \"$SRC\" \
         \"+refs/heads/main:refs/control/main\"\n"
    );
    let out = Command::new("setpriv")
        .args([
            "--reuid".to_string(),
            uid.to_string(),
            "--regid".to_string(),
            uid.to_string(),
            "--clear-groups".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            body,
        ])
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        // Nothing but the exemption under test may exempt anything.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("SRC", src)
        .env("DST", &dst)
        .output()
        .expect("setpriv runs");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// `--check` only asks that `gh`/`jq`/`curl` EXIST. This box may lack
/// them (the gate image has them), so stand in a stub for whatever is
/// missing — the same idiom `infra/lint/the-controls-are-bounded-verbs.sh`
/// uses. `git` is never stubbed: every verdict here is a real git's.
fn stub_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    for tool in ["gh", "jq", "curl"] {
        if Command::new("sh")
            .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
            .status()
            .is_ok_and(|s| s.success())
        {
            continue;
        }
        let stub = bin.join(tool);
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// A state dir any uid can write and a 0600 token file, so the only thing
/// a case varies is the forge repository.
fn base_env(root: &Path) -> Vec<(String, String)> {
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).unwrap();
    let token = root.join("github.token");
    std::fs::write(&token, "not-a-real-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    vec![
        ("BOSS_PUBLISH_STATE_DIR".into(), state.display().to_string()),
        ("BOSS_GITHUB_TOKEN_FILE".into(), token.display().to_string()),
    ]
}

/// `--check`, with a cleared environment (the ops-runner has no HOME).
/// `as_uid` runs the script under a second account, which is the only way
/// to exercise "unreadable by this user" and git's ownership refusal on a
/// box where every fixture belongs to the caller.
fn check(root: &Path, extra: &[(&str, String)], as_uid: Option<u32>) -> (bool, String) {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let mut cmd = match as_uid {
        None => {
            let mut c = Command::new("bash");
            c.arg(script());
            c
        }
        Some(uid) => {
            let mut c = Command::new("setpriv");
            c.args([
                "--reuid".to_string(),
                uid.to_string(),
                "--regid".to_string(),
                uid.to_string(),
                "--clear-groups".to_string(),
                "bash".to_string(),
                script().display().to_string(),
            ]);
            c
        }
    };
    let path = format!("{}:{path}", stub_bin(root).display());
    cmd.arg("--check").env_clear().env("PATH", path);
    for (k, v) in base_env(root) {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the verb runs");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn forge_path(p: &Path) -> (&'static str, String) {
    ("BOSS_FORGE_REPO_PATH", p.display().to_string())
}

/// The ownership cases need a second uid, which needs root and setpriv.
/// They SKIP loudly rather than pass vacuously (the gate image is root).
fn second_uid() -> Option<u32> {
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "0")
        .unwrap_or(false);
    let have_setpriv = Command::new("sh")
        .args(["-c", "command -v setpriv >/dev/null 2>&1"])
        .status()
        .is_ok_and(|s| s.success());
    if is_root && have_setpriv {
        Some(65534)
    } else {
        None
    }
}

// ---------------------------------------------------------------------
// Defect 2 — four refusals, four sentences.
// ---------------------------------------------------------------------

/// ABSENT. Nothing exists at the path, and the refusal names the deepest
/// path that DOES exist — which is what turns "the layout is wrong" into
/// "the layout is right up to here".
#[test]
fn an_absent_forge_repository_names_the_deepest_path_that_does_exist() {
    let root = scratch("absent");
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let missing = here.join("data/git/repositories/david/boss.git");
    let (ok, out) = check(&root, &[forge_path(&missing)], None);
    assert!(!ok, "--check passed with no forge repository: {out}");
    assert!(
        out.contains("nothing exists there"),
        "the absent case does not say nothing exists there: {out}"
    );
    assert!(
        out.contains(&here.display().to_string()),
        "the absent case does not name the deepest existing path {}: {out}",
        here.display()
    );
    assert!(
        !out.contains("not readable by this user"),
        "the absent case reads as a permission finding: {out}"
    );
}

/// NOT A DIRECTORY. A file where a bare repository was expected is its
/// own sentence — the commonest shape of a half-right path.
#[test]
fn a_forge_repository_path_that_is_a_regular_file_says_it_is_not_a_directory() {
    let root = scratch("regular-file");
    let file = root.join("boss.git");
    std::fs::write(&file, "not a repository\n").unwrap();
    let (ok, out) = check(&root, &[forge_path(&file)], None);
    assert!(!ok, "--check passed on a regular file: {out}");
    assert!(
        out.contains("is not a directory"),
        "the regular-file case does not say it is not a directory: {out}"
    );
    assert!(
        !out.contains("nothing exists there"),
        "the regular-file case reads as absent: {out}"
    );
}

/// UNREADABLE, AND THE USER IS NAMED. The verb runs as root through the
/// ops-runner, so "unreadable by root" and "unreadable by david" are
/// different findings and the message must say which.
#[test]
fn an_unreadable_forge_repository_names_the_user_that_cannot_read_it() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("unreadable");
    let locked = root.join("boss.git");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (ok, out) = check(&root, &[forge_path(&locked)], Some(uid));
    assert!(!ok, "--check passed on an unreadable directory: {out}");
    assert!(
        out.contains("not readable by this user"),
        "the unreadable case does not say it is a permission finding: {out}"
    );
    assert!(
        out.contains("nobody") || out.contains(&uid.to_string()),
        "the unreadable case does not NAME the user that cannot read it: {out}"
    );
    assert!(
        !out.contains("nothing exists there"),
        "the unreadable case reads as absent — the exact confusion ed84b5d9 filed: {out}"
    );
}

/// REFUSED BY GIT. A readable directory that is not a repository reports
/// GIT'S OWN WORDS, so no reader has to re-derive what git said.
#[test]
fn a_readable_directory_that_is_no_repository_reports_gits_own_words() {
    let root = scratch("not-a-repo");
    let dir = root.join("boss.git");
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out) = check(&root, &[forge_path(&dir)], None);
    assert!(
        !ok,
        "--check passed on a directory that is no repository: {out}"
    );
    assert!(
        out.contains("git refused it as a repository"),
        "the non-repository case does not say git refused it: {out}"
    );
    assert!(
        out.to_lowercase().contains("not a git repository"),
        "the non-repository case does not carry git's own words: {out}"
    );
}

/// A repository owned by ANOTHER account is read anyway. The ops-runner
/// executes verbs as root and this repository belongs to the Forgejo
/// container's account, so since git 2.35.2 every command refuses it as
/// "dubious ownership" — measured on this host class on ops-request
/// c9877f75 (2026-09-10). `--check` and the run read through one helper,
/// so this is the same read the fetch performs.
#[test]
fn a_repository_owned_by_another_account_is_read_anyway() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("foreign-owner");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let (_ok, out) = check(&root, &[forge_path(&repo)], Some(uid));
    // `forge repo :` is the banner; `forge repositor…` only ever appears in
    // a refusal about the repository itself. (The token fixture stays
    // root-owned 0600, so the token line may still complain — a different
    // finding, and the point is that the REPOSITORY read no longer does.)
    assert!(
        !out.contains("forge repositor"),
        "a repository owned by another account was refused — the ownership check was not dropped: {out}"
    );
    assert!(
        !out.to_lowercase().contains("dubious ownership"),
        "git's ownership refusal reached the output: {out}"
    );
}

// ---------------------------------------------------------------------
// Defect 1 — the path is derived from the thing that declares it.
// ---------------------------------------------------------------------

/// The compose file declares which HOST directory is mounted at the
/// container's `/data`; the verb reads it instead of keeping a second
/// copy. A relative mount resolves against the compose file's directory.
#[test]
fn the_forge_repository_path_is_derived_from_the_compose_data_mount() {
    let root = scratch("derive-compose");
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    image: codeberg.org/forgejo/forgejo:16.0.2\n    volumes:\n      - ./data:/data\n      - /etc/timezone:/etc/timezone:ro\n",
    )
    .unwrap();
    let expected = here.join("data/git/repositories/david/boss.git");
    git_init_bare(&expected);
    let (ok, out) = check(
        &root,
        &[("BOSS_FORGE_COMPOSE", compose.display().to_string())],
        None,
    );
    assert!(ok, "--check failed on a derived path that exists: {out}");
    assert!(
        out.contains(&expected.display().to_string()),
        "--check does not report the derived path {}: {out}",
        expected.display()
    );
    assert!(
        out.contains(&compose.display().to_string()),
        "--check does not say the path was derived from {}: {out}",
        compose.display()
    );
}

/// And inside `/data`, the repository root is FORGEJO'S OWN —
/// `[repository] ROOT` from app.ini — translated from the container path
/// to the host one. `git/repositories` is only the image's default.
#[test]
fn the_derivation_takes_forgejos_own_repository_root_from_app_ini() {
    let root = scratch("derive-app-ini");
    let here = root.join("forgejo");
    let data = here.join("data");
    std::fs::create_dir_all(data.join("gitea/conf")).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    volumes:\n      - ./data:/data\n",
    )
    .unwrap();
    std::fs::write(
        data.join("gitea/conf/app.ini"),
        "[server]\nROOT_URL = http://10.20.0.15:3000/\n\n[repository]\nROOT = /data/elsewhere/repos\n",
    )
    .unwrap();
    let expected = data.join("elsewhere/repos/david/boss.git");
    git_init_bare(&expected);
    let (ok, out) = check(
        &root,
        &[("BOSS_FORGE_COMPOSE", compose.display().to_string())],
        None,
    );
    assert!(ok, "--check failed on the app.ini repository root: {out}");
    assert!(
        out.contains(&expected.display().to_string()),
        "--check does not report the app.ini repository root {}: {out}",
        expected.display()
    );
}

/// With no readable compose file the verb still answers — but it SAYS the
/// path is a fallback guess rather than presenting it as fact. The
/// unexercised default is what ed84b5d9 is about, so it may never again
/// be printed without that label.
#[test]
fn an_unreadable_compose_file_makes_the_check_label_the_path_a_fallback() {
    let root = scratch("no-compose");
    let (ok, out) = check(
        &root,
        &[(
            "BOSS_FORGE_COMPOSE",
            root.join("absent-compose.yml").display().to_string(),
        )],
        None,
    );
    assert!(!ok, "--check passed with no forge repository at all: {out}");
    assert!(
        out.contains("fallback"),
        "--check does not label the underived path a fallback: {out}"
    );
    assert!(
        out.contains("/opt/forgejo/data/git/repositories/david/boss.git"),
        "--check does not report the fallback path it actually used: {out}"
    );
}

/// The override still wins over every derivation — the bounded-verbs lint
/// and every case above depend on it.
#[test]
fn the_env_override_wins_over_the_derivation_and_says_so() {
    let root = scratch("override");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    volumes:\n      - ./data:/data\n",
    )
    .unwrap();
    let (ok, out) = check(
        &root,
        &[
            forge_path(&repo),
            ("BOSS_FORGE_COMPOSE", compose.display().to_string()),
        ],
        None,
    );
    assert!(
        ok,
        "--check failed on an overridden, existing repository: {out}"
    );
    assert!(
        out.contains("BOSS_FORGE_REPO_PATH"),
        "--check does not say the path came from the override: {out}"
    );
    assert!(
        out.contains("--check ok"),
        "--check did not report ok: {out}"
    );
    assert!(
        !out.contains("not-a-real-token"),
        "--check printed the token fixture: {out}"
    );
}

// ---------------------------------------------------------------------
// Defect 3 — `--check` exercises the operation, not a cheaper cousin.
// ---------------------------------------------------------------------

/// `--check` FETCHES, and says what it read. The sha it reports is the
/// fixture's own `main`, so the line is evidence the fetch happened
/// rather than a claim that it would.
#[test]
fn the_check_performs_the_forge_fetch_and_names_what_it_read() {
    let root = scratch("fetch-performed");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let head = git_in(&repo, &["rev-parse", "refs/heads/main"]);
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(ok, "--check failed on a fetchable repository: {out}");
    assert!(
        out.contains("forge fetch:"),
        "--check does not report the fetch it performed: {out}"
    );
    assert!(
        out.contains(&head),
        "--check does not name the sha it fetched ({head}): {out}"
    );
}

/// THE REGRESSION THAT LET THIS SHIP. A repository git reads perfectly
/// well and the fetch cannot: an empty bare repository, where
/// `rev-parse --git-dir` answers and `refs/heads/main` does not exist.
/// The old `--check` said ok; the publish would have failed. So the pin
/// is not "the exemption works" — it is "`--check` fails wherever the
/// fetch fails", whatever the reason.
#[test]
fn a_forge_repository_whose_fetch_cannot_succeed_fails_the_check() {
    let root = scratch("fetch-unfetchable");
    let repo = root.join("boss.git");
    // Deliberately NOT git_init_bare: no main, nothing to fetch.
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    let st = Command::new("git")
        .args(["init", "-q", "--bare", repo.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success());
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(
        !ok,
        "--check passed on a repository the publish cannot fetch from — \
         the 2026-09-11 defect exactly: {out}"
    );
    assert!(
        out.contains("the FETCH the publish performs fails"),
        "the refusal does not say the FETCH is what failed: {out}"
    );
    assert!(
        out.to_lowercase().contains("couldn't find remote ref")
            || out.to_lowercase().contains("could not find remote ref"),
        "the refusal does not carry git's own words about the fetch: {out}"
    );
    // And it is its OWN sentence — not one of the four path findings.
    assert!(
        !out.contains("nothing exists there") && !out.contains("is not a directory"),
        "the fetch finding reads as a path finding: {out}"
    );
}

/// A repository owned by ANOTHER account is FETCHED, not merely read —
/// with the negative control measured alongside it. The control performs
/// the same fetch, from the same fixture, as the same uid, exempting the
/// source the way the script used to; it must FAIL with git's ownership
/// refusal, or this test is vacuous. Then `--check` must succeed.
#[test]
fn a_foreign_owned_forge_repository_is_fetched_not_only_read() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("fetch-foreign-owner");
    let repo = root.join("boss.git");
    git_init_bare(&repo); // root-owned; the check below runs as `uid`

    // CONTROL A — no exemption at all. Establishes that this fixture
    // really is foreign-owned from `uid`'s point of view.
    let (ok, out) = control_fetch(&root, &repo, uid, false);
    assert!(
        !ok,
        "an unexempted fetch from a foreign-owned source SUCCEEDED — the fixture is not foreign-owned, so nothing below is a test: {out}"
    );
    assert!(
        out.to_lowercase().contains("dubious ownership"),
        "the unexempted control failed for some other reason than ownership: {out}"
    );

    // CONTROL B — the script's former shape. `-c safe.directory=<src>`
    // does not reach upload-pack, so this fails identically. This is the
    // bug, reproduced, in the test that guards against it.
    let (ok, out) = control_fetch(&root, &repo, uid, true);
    assert!(
        !ok,
        "`-c safe.directory=<source>` exempted a fetch SOURCE on this git \
         ({}) — the premise of the fix does not hold here and the fix must \
         be re-argued: {out}",
        git_version()
    );
    assert!(
        out.to_lowercase().contains("dubious ownership"),
        "the `-c`-only control failed for some other reason than ownership: {out}"
    );

    // AND THE VERB, through the protected channel, on the same fixture.
    let (ok, out) = check(&root, &[forge_path(&repo)], Some(uid));
    assert!(
        !out.to_lowercase().contains("dubious ownership"),
        "git's ownership refusal reached the verb's output: {out}"
    );
    assert!(
        !out.contains("the FETCH the publish performs fails"),
        "the verb could not fetch from a foreign-owned repository: {out}"
    );
    assert!(
        out.contains("forge fetch:"),
        "the verb did not report a fetch at all: {out}"
    );
    // The token fixture is root:root 0600, so `uid` cannot read it and
    // --check still refuses overall — a DIFFERENT finding, and the one
    // thing this case is not about.
    assert!(
        !ok && out.contains("github.token"),
        "expected the token to be the only remaining finding: {out}"
    );
}

/// `git --version`, for the one assertion whose premise is version-bound.
fn git_version() -> String {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "git version unknown".into())
}

// ---------------------------------------------------------------------
// Defect 4 (2026-09-11, the fourth failure of the day) — the fork check
// verified a NAME, not a fork.
//
// `gh repo view "$FORK_SLUG" --json name` succeeded for `dauld/boss`,
// David's unrelated PRIVATE repository, so the auto-fork beneath it was
// skipped, the snapshot was pushed into a repository outside
// algedonic-dev/boss's fork network, and `gh pr create` then returned
// four GraphQL errors at once — "Head sha can't be blank", "Base sha
// can't be blank", "No commits between algedonic-dev:main and
// dauld:publish/2026-09-11", "Head ref must be a branch" — none of which
// names the cause. Measured the same day against GitHub's REST API, with
// a control on the same connection: repos/dauld/boss 404,
// repos/algedonic-dev/boss 200 (fork=false), repos/dauld/boss-mirror 200
// (fork=true, parent=algedonic-dev/boss).
//
// So the cases below run the VERB's run path, not `--check`: the fork
// question needs the token and the network, which `--check` deliberately
// has neither of. Every outside party is a fixture — the jobs API a
// `curl` stub that prints one packet, GitHub's REST API a `gh` stub that
// prints a repository object, the mirror and the fork local bare
// repositories — and the slugs are fixture names, never dauld's. The
// accepted and the refused case differ in ONE thing: the `fork`/`parent`
// fields of that object. Without that pairing neither proves anything.
// ---------------------------------------------------------------------

/// One git command in `dir` with `input` on stdin — `hash-object` and
/// `mktree` are the two fixtures below need, and both read stdin.
fn git_stdin(dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .spawn()
        .expect("git runs");
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(input.as_bytes())
        .expect("git reads stdin");
    let out = child.wait_with_output().expect("git finishes");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare repository whose `main` holds one file with `content`. The
/// forge and the mirror stand-ins must differ in their TREE, not only
/// their history: the verb refuses "nothing to publish" when the two
/// trees match, so two empty-tree fixtures would never reach the fork.
fn git_init_bare_holding(path: &Path, content: &str) {
    git_init_bare(path);
    let blob = git_stdin(
        path,
        &["hash-object", "-t", "blob", "-w", "--stdin"],
        content,
    );
    let tree = git_stdin(path, &["mktree"], &format!("100644 blob {blob}\tfile\n"));
    let commit = git_in(path, &["commit-tree", &tree, "-m", "fixture"]);
    git_in(path, &["update-ref", "refs/heads/main", &commit]);
}

/// Real `jq`, not the `exit 0` stand-in `stub_bin` installs for a missing
/// tool: the run path reads the packet and the repository object with it,
/// so a stub would make every case below pass vacuously.
fn have_real_jq() -> bool {
    Command::new("sh")
        .args(["-c", "command -v jq >/dev/null 2>&1"])
        .status()
        .is_ok_and(|s| s.success())
}

const FORK_SLUG: &str = "fixture-owner/mirror-fork";
const MIRROR_SLUG: &str = "fixture-upstream/mirror";
const PUBLISH_DATE: &str = "2026-01-02";

/// One offline run of the verb. Build it, describe what GitHub says about
/// the fork with `gh_repo` / `on_fork`, then `go()`.
struct Run {
    root: PathBuf,
    forge: PathBuf,
    mirror: PathBuf,
    fork: PathBuf,
    gh_api: PathBuf,
    stubs: PathBuf,
}

impl Run {
    fn new(case: &str) -> Run {
        let root = scratch(case);
        let forge = root.join("forge.git");
        let mirror = root.join("mirror.git");
        let fork = root.join("fork.git");
        // Distinct trees, or the verb stops at "nothing to publish".
        git_init_bare_holding(&forge, "forge main\n");
        git_init_bare_holding(&mirror, "an older mirror main\n");
        git_init_bare(&fork);
        let gh_api = root.join("gh-api");
        std::fs::create_dir_all(&gh_api).unwrap();

        // One open publish-to-github packet whose open-pr step is ready —
        // the shape the verb selects with jq.
        let jobs = root.join("jobs.json");
        boss_testing::write_file(
            &jobs,
            r#"{"data":[{"id":"00000000-0000-0000-0000-0000000000aa",
                         "title":"publish to github","status":"open",
                         "steps":[{"id":"00000000-0000-0000-0000-0000000000bb",
                                   "spec_slug":"open-pr","status":"ready",
                                   "metadata":{"ops_verb":"publish-github-pr"}}]}]}"#,
        );

        let stubs = root.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        let fork_file = format!("{}.json", FORK_SLUG.replace('/', "_"));
        // `gh`: the REST repository object from a fixture file, `repo
        // view --json name` answering for ANY name that has one (which is
        // exactly how today's wrong repository passed), and `repo fork`
        // materialising whatever `on_fork` left for it.
        boss_testing::write_exec(
            &stubs.join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
slug_file() {{ echo '{api}/'"$(echo "$1" | tr / _)".json; }}
if [ "$1" = "api" ]; then
    f=$(slug_file "${{2#repos/}}")
    if [ -f "$f" ]; then cat "$f"; exit 0; fi
    echo 'gh: Not Found (HTTP 404)' >&2
    exit 1
fi
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
    f=$(slug_file "$3")
    if [ -f "$f" ]; then echo '{{"name":"stub"}}'; exit 0; fi
    echo 'gh: Could not resolve to a Repository (HTTP 404)' >&2
    exit 1
fi
if [ "$1" = "repo" ] && [ "$2" = "fork" ]; then
    [ -f '{api}/_on_fork.json' ] && cp '{api}/_on_fork.json' '{api}/{fork_file}'
    exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
    echo 'https://github.invalid/{mirror_slug}/pull/1'
    exit 0
fi
exit 0
"#,
                log = root.join("gh.log").display(),
                api = gh_api.display(),
                fork_file = fork_file,
                mirror_slug = MIRROR_SLUG,
            ),
        );
        // `curl`: the packet on a GET, silence on the step PUT.
        boss_testing::write_exec(
            &stubs.join("curl"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
for a in "$@"; do
    if [ "$a" = "PUT" ]; then exit 0; fi
done
cat '{jobs}'
"#,
                log = root.join("curl.log").display(),
                jobs = jobs.display(),
            ),
        );

        Run {
            root,
            forge,
            mirror,
            fork,
            gh_api,
            stubs,
        }
    }

    /// What GitHub's REST API says about `slug`.
    fn gh_repo(&self, slug: &str, body: &str) {
        boss_testing::write_file(
            &self.gh_api.join(format!("{}.json", slug.replace('/', "_"))),
            body,
        );
    }

    /// What `gh repo fork` brings into being — absent, the fork stays 404
    /// after forking, which is its own finding.
    fn on_fork(&self, body: &str) {
        boss_testing::write_file(&self.gh_api.join("_on_fork.json"), body);
    }

    fn go(&self) -> (bool, String) {
        let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        // Ours first: `stub_bin` stands in for tools this box LACKS, and
        // gh/curl must be ours even where the box has them.
        let path = format!(
            "{}:{}:{outer}",
            self.stubs.display(),
            stub_bin(&self.root).display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(script()).env_clear().env("PATH", path);
        for (k, v) in base_env(&self.root) {
            cmd.env(k, v);
        }
        cmd.env("BOSS_JOBS_URL", "http://jobs.invalid")
            .env("BOSS_FORGE_REPO_PATH", self.forge.display().to_string())
            .env("BOSS_MIRROR_SLUG", MIRROR_SLUG)
            .env("BOSS_MIRROR_URL", self.mirror.display().to_string())
            .env("BOSS_FORK_SLUG", FORK_SLUG)
            .env("BOSS_FORK_URL", self.fork.display().to_string())
            .env("BOSS_PUBLISH_DATE", PUBLISH_DATE);
        let out = cmd.output().expect("the verb runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    /// Did the snapshot reach the repository standing in for the fork?
    /// This is the assertion that matters: a refusal that still pushed is
    /// not a refusal.
    fn pushed(&self) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(&self.fork)
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/publish/{PUBLISH_DATE}"),
            ])
            .output()
            .expect("git runs")
            .status
            .success()
    }

    fn gh_log(&self) -> String {
        std::fs::read_to_string(self.root.join("gh.log")).unwrap_or_default()
    }

    fn curl_log(&self) -> String {
        std::fs::read_to_string(self.root.join("curl.log")).unwrap_or_default()
    }
}

/// A fork of the mirror — `fork=true`, `parent`/`source` the mirror. The
/// POSITIVE half of the pair: the verb must push and open the PR here, or
/// the refusal below is just a verb that refuses everything.
#[test]
fn a_real_fork_of_the_mirror_is_published_to() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("fork-accepted");
    run.gh_repo(
        FORK_SLUG,
        &format!(
            r#"{{"full_name":"{FORK_SLUG}","fork":true,
                 "parent":{{"full_name":"{MIRROR_SLUG}"}},
                 "source":{{"full_name":"{MIRROR_SLUG}"}},
                 "default_branch":"main","private":false}}"#
        ),
    );
    let (ok, out) = run.go();
    assert!(ok, "the verb refused a real fork of the mirror: {out}");
    assert!(
        run.pushed(),
        "the verb accepted the fork but pushed nothing to it: {out}"
    );
    assert!(
        out.contains("https://github.invalid"),
        "no PR was opened: {out}"
    );
    assert!(
        run.curl_log().contains("PUT"),
        "open-pr was never completed on the packet: {}",
        run.curl_log()
    );
    assert!(
        !run.gh_log().contains("repo fork"),
        "the verb forked a fork that already existed: {}",
        run.gh_log()
    );
}

/// THE CASE THAT FAILED. A repository that EXISTS under the fork's name
/// and is not in the mirror's fork network — today's `dauld/boss`, and
/// below it a fork of some OTHER upstream, because "is a fork" is not the
/// question either. Both must be refused by name, and neither may push:
/// GitHub accepts the push (it is our own repository) and only the PR
/// fails, one step too late to undo.
#[test]
fn a_namesake_that_is_not_a_fork_of_the_mirror_is_refused_before_the_push() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    // (a) exists, not a fork at all — the live shape of dauld/boss.
    let run = Run::new("fork-namesake");
    run.gh_repo(
        FORK_SLUG,
        &format!(
            r#"{{"full_name":"{FORK_SLUG}","fork":false,"parent":null,"source":null,
                 "default_branch":"main","private":true}}"#
        ),
    );
    let (ok, out) = run.go();
    assert!(
        !ok,
        "a repository that is NOT a fork of {MIRROR_SLUG} was accepted: {out}"
    );
    assert!(
        !run.pushed(),
        "REFUSED and pushed anyway — the push is the step that cannot be taken back: {out}"
    );
    assert!(
        out.contains(FORK_SLUG) && out.contains(MIRROR_SLUG),
        "the refusal names neither the wrong repository nor the mirror: {out}"
    );
    assert!(
        out.contains("fork=false"),
        "the refusal does not quote what the repository says about itself: {out}"
    );
    assert!(
        !run.gh_log().contains("pr create"),
        "it reached gh pr create, which is where today's failure surfaced: {}",
        run.gh_log()
    );

    // (b) a fork — of something else. `isFork` alone would pass this.
    let run = Run::new("fork-of-another-upstream");
    run.gh_repo(
        FORK_SLUG,
        &format!(
            r#"{{"full_name":"{FORK_SLUG}","fork":true,
                 "parent":{{"full_name":"someone-else/boss"}},
                 "source":{{"full_name":"someone-else/boss"}},
                 "default_branch":"main","private":false}}"#
        ),
    );
    let (ok, out) = run.go();
    assert!(
        !ok,
        "a fork of someone-else/boss was accepted as a fork of {MIRROR_SLUG}: {out}"
    );
    assert!(!run.pushed(), "REFUSED and pushed anyway: {out}");
    assert!(
        out.contains("someone-else/boss"),
        "the refusal does not say which upstream it IS a fork of: {out}"
    );
}

/// ABSENT (404) is the one case the auto-fork was written for, and it must
/// still reach it — and fork under the fork's OWN name, because
/// `gh repo fork` otherwise names the new fork after the upstream, which
/// on this account is the repository that caused today's failure.
#[test]
fn an_absent_fork_is_created_under_the_forks_own_name_and_re_read() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("fork-absent");
    // No repository object for the fork: gh answers 404.
    run.on_fork(&format!(
        r#"{{"full_name":"{FORK_SLUG}","fork":true,
             "parent":{{"full_name":"{MIRROR_SLUG}"}},
             "source":{{"full_name":"{MIRROR_SLUG}"}},
             "default_branch":"main","private":false}}"#
    ));
    let (ok, out) = run.go();
    assert!(ok, "the verb could not fork an absent fork: {out}");
    let log = run.gh_log();
    assert!(
        log.contains(&format!("repo fork {MIRROR_SLUG}")),
        "the auto-fork never ran: {log}"
    );
    assert!(
        log.contains("--fork-name mirror-fork"),
        "the fork was created under the upstream's name, not {FORK_SLUG}'s — the next push would go somewhere else again: {log}"
    );
    assert!(run.pushed(), "nothing was pushed after forking: {out}");
}

/// And if the fork does NOT appear after forking, that is a refusal too —
/// `gh repo fork` exiting 0 is not evidence that the repository we are
/// about to push to is the one we just made.
#[test]
fn a_fork_that_does_not_appear_after_forking_is_refused() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("fork-absent-after");
    // No repository object, and nothing for `repo fork` to bring into
    // being — it exits 0 and changes nothing, the way a silent failure
    // looks from here.
    let (ok, out) = run.go();
    assert!(
        !ok,
        "the verb pushed to a fork GitHub never reported: {out}"
    );
    assert!(!run.pushed(), "REFUSED and pushed anyway: {out}");
    assert!(
        out.contains(FORK_SLUG),
        "the refusal does not name the fork: {out}"
    );
}

/// The DEFAULT, read off a real run of `--check`: unset, `BOSS_FORK_SLUG`
/// must be dauld/boss-mirror — the fork that is actually in
/// algedonic-dev/boss's network — and must not be dauld/boss, which is
/// not. Nothing in the tree sets this variable, so the default IS what
/// runs, and `--check` prints it without touching the network.
#[test]
fn the_default_fork_is_the_mirrors_real_fork() {
    let root = scratch("default-fork");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(ok, "--check refused a complete input set: {out}");
    assert!(
        out.contains("https://github.com/dauld/boss-mirror.git"),
        "--check does not name dauld/boss-mirror as the fork: {out}"
    );
    assert!(
        !out.contains("https://github.com/dauld/boss.git"),
        "the fork default is still dauld/boss, David's unrelated private repository: {out}"
    );
}
