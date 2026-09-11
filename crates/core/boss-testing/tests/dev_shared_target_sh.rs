//! `infra/dev-shared-target.sh` points every cargo build on a machine
//! at one target directory instead of one per worktree.
//!
//! Driven as a real script against a sandboxed `CARGO_HOME`, because
//! the thing under test is "does it edit a cargo config correctly and
//! does cargo then obey it" — and a test that inspected the script's
//! text instead would prove only that this file agrees with that one.
//! Every case here writes into a temp dir; the developer's own
//! `~/.cargo/config.toml` is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> (Scratch, PathBuf) {
    // Per-uid and per-process, and it REFUSES by name if a leftover
    // cannot be cleared — see `boss_testing::scratch`.
    let root = boss_testing::scratch_dir(&format!("boss-dst-{name}"));
    boss_testing::create_dir(&root.join("cargo"));
    (Scratch(root.clone()), root)
}

fn run(cargo_home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/dev-shared-target.sh"))
        .args(args)
        .env("CARGO_HOME", cargo_home)
        .env_remove("CARGO_TARGET_DIR")
        // EVERY marker the script's CI guard reads. Missing one is not
        // a cosmetic slip: on 2026-08-17 this helper cleared `CI` and
        // `GITHUB_ACTIONS` but not `FORGEJO_ACTIONS`, so three of these
        // tests passed on a laptop and reddened train e19759ce the
        // moment they ran on the forge runner — where the script
        // correctly refused, against its own test suite. The list is
        // pinned below so it cannot drift from the script again.
        .env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("FORGEJO_ACTIONS")
        .current_dir(repo_root())
        .output()
        .expect("run dev-shared-target.sh");
    let merged = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), merged)
}

/// On, off, and — the part that matters — an unrelated section of the
/// developer's config surviving both.
#[test]
fn on_and_off_round_trip_without_eating_the_rest_of_the_config() {
    let (_g, root) = scratch("roundtrip");
    let home = root.join("cargo");
    let config = home.join("config.toml");
    std::fs::write(&config, "[net]\nretry = 3\n").expect("seed config");
    let shared = root.join("shared");

    let (ok, _) = run(&home, &["--on", shared.to_str().expect("utf8")]);
    assert!(ok, "--on should succeed");
    let after_on = std::fs::read_to_string(&config).expect("read");
    assert!(
        after_on.contains("[net]") && after_on.contains("retry = 3"),
        "an unrelated section must survive: {after_on}"
    );
    assert!(
        after_on.contains(shared.to_str().expect("utf8")),
        "the shared dir must be written: {after_on}"
    );

    let (ok, _) = run(&home, &["--off"]);
    assert!(ok, "--off should succeed");
    let after_off = std::fs::read_to_string(&config).expect("read");
    assert!(
        after_off.contains("[net]") && after_off.contains("retry = 3"),
        "--off must remove only what --on added: {after_off}"
    );
    assert!(
        !after_off.contains("target-dir"),
        "--off must remove the setting: {after_off}"
    );
}

/// A relative path silently shares NOTHING — it resolves inside each
/// worktree — so it has to be refused rather than accepted and
/// quietly not working.
#[test]
fn a_relative_directory_is_refused() {
    let (_g, root) = scratch("relative");
    let (ok, out) = run(&root.join("cargo"), &["--on", "./nope"]);
    assert!(!ok, "a relative dir must fail");
    assert!(out.contains("must be absolute"), "must say why: {out}");
}

/// Never edit a setting this script did not write.
#[test]
fn a_foreign_target_dir_setting_is_left_alone() {
    let (_g, root) = scratch("foreign");
    let home = root.join("cargo");
    let config = home.join("config.toml");
    std::fs::write(&config, "[build]\ntarget-dir = \"/somewhere/else\"\n").expect("seed");

    let (ok, out) = run(
        &home,
        &["--on", root.join("shared").to_str().expect("utf8")],
    );
    assert!(!ok, "must refuse");
    assert!(out.contains("Refusing to edit"), "must say why: {out}");
    assert!(
        std::fs::read_to_string(&config)
            .expect("read")
            .contains("/somewhere/else"),
        "the foreign setting must be untouched"
    );
}

/// CI gives every job its own volume, so sharing buys nothing there
/// and a surprise absolute path is a way to lose a build.
#[test]
fn it_refuses_to_run_in_ci() {
    let (_g, root) = scratch("ci");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/dev-shared-target.sh"))
        .args(["--on", root.join("shared").to_str().expect("utf8")])
        .env("CARGO_HOME", root.join("cargo"))
        .env("CI", "1")
        .current_dir(repo_root())
        .output()
        .expect("run");
    assert!(!out.status.success(), "must refuse under CI=1");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("refusing to run in CI"),
        "must say why"
    );
}

/// The helper above must clear exactly the variables the script's CI
/// guard reads — a fact that lives in two files and cannot be
/// collapsed, so CLAUDE.md 9a says pin it. Reads the guard line out of
/// the script and checks every `${VAR:-}` it tests is one this file
/// removes. Named the offender when it fails.
#[test]
fn the_helper_clears_every_ci_marker_the_script_checks() {
    let script = std::fs::read_to_string(repo_root().join("infra/dev-shared-target.sh"))
        .expect("read dev-shared-target.sh");
    let guard = script
        .lines()
        .find(|l| l.trim_start().starts_with("if [ -n \"${CI:-}\""))
        .expect("the CI guard line is still recognisable");

    let mut checked: Vec<String> = Vec::new();
    let mut rest = guard;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        if let Some(j) = rest.find(":-}") {
            checked.push(rest[..j].to_string());
        }
    }
    assert!(!checked.is_empty(), "parsed no variables out of: {guard}");

    const CLEARED: [&str; 3] = ["CI", "GITHUB_ACTIONS", "FORGEJO_ACTIONS"];
    let missed: Vec<&String> = checked
        .iter()
        .filter(|v| !CLEARED.contains(&v.as_str()))
        .collect();
    assert!(
        missed.is_empty(),
        "the script's CI guard reads {missed:?}, which `run()` does not clear — \
         add them to CLEARED and to the .env_remove calls, or these tests will \
         pass locally and fail on a runner that sets them"
    );
}
