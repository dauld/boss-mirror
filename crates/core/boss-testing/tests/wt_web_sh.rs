//! `infra/dev/wt-web` — the web half of `wt-cargo`: from inside a
//! worktree it symlinks the operator checkout's `node_modules` at the
//! three places bun resolves them, then runs the given command there
//! (backlog ef4394e1).
//!
//! A worktree has no `node_modules`, so every web check a builder ran
//! there (svelte-check, the mocked specs, anything importing zod) failed
//! on the environment before it could fail on the change — and on
//! 2026-09-14 three builders each re-derived a different workaround.
//! Pinned here against a scratch git repository, a fake source tree,
//! and a stub command on PATH, so nothing touches /work/boss:
//!
//!   * links land at `./node_modules`, `apps/web/node_modules`,
//!     `libs/web-kit/node_modules`, pointing into `WT_WEB_SOURCE`;
//!   * a second run changes nothing and says the links already exist;
//!   * a source dir that does not exist is skipped, by name, and the
//!     rest still link;
//!   * a real directory at a destination (a `bun install` someone ran)
//!     is left alone, by name;
//!   * outside a git worktree it refuses (exit 2);
//!   * argv passes through untouched; with no argv it only prepares;
//!   * the links are gitignored by the repo's own root `.gitignore` —
//!     `node_modules/` with a trailing slash matches directories only,
//!     and a symlink is not a directory to git, so the pattern that
//!     ignores a `bun install` did NOT ignore these links until this
//!     car dropped the slash.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/wt-web";
const LINKS: [&str; 3] = [
    "node_modules",
    "apps/web/node_modules",
    "libs/web-kit/node_modules",
];

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-web-{name}"));
        let bin = root.join("bin");
        boss_testing::create_dir(&bin);
        // The command under test hands off to: print what it was given
        // and where it ran. The real bun / svelte-check is never reached.
        write_exec(
            &bin.join("webcheck"),
            "#!/usr/bin/env bash\n\
             echo \"argv=$*\"\n\
             echo \"cwd=$PWD\"\n",
        );
        // A fake operator checkout: the three node_modules dirs, each
        // with a marker so a test can prove a link resolves INTO it.
        let source = root.join("source");
        for rel in LINKS {
            let dir = source.join(rel);
            boss_testing::create_dir(&dir);
            write_file(&dir.join("marker.txt"), rel);
        }
        Self { root, bin, source }
    }

    /// A real git repository carrying the repo's own root `.gitignore`,
    /// so the test reads the pattern that will judge the links in a
    /// live worktree rather than one it typed itself.
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
        let ignore = std::fs::read_to_string(repo_root().join(".gitignore")).expect(".gitignore");
        write_file(&wt.join(".gitignore"), &ignore);
        wt
    }

    fn run(&self, worktree: &Path, args: &[&str]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .current_dir(worktree)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("WT_WEB_SOURCE", &self.source);
        let out = cmd.output().expect("run wt-web");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    /// Every link that is in place and points into the source tree.
    fn assert_linked(&self, worktree: &Path, rels: &[&str]) {
        for rel in rels {
            let dst = worktree.join(rel);
            let meta = std::fs::symlink_metadata(&dst)
                .unwrap_or_else(|e| panic!("{}: {e}", dst.display()));
            assert!(
                meta.file_type().is_symlink(),
                "{rel} must be a symlink, not {:?}",
                meta.file_type()
            );
            let target = std::fs::read_link(&dst).expect("readlink");
            assert_eq!(
                target,
                self.source.join(rel),
                "{rel} must point at the source checkout's copy"
            );
            assert_eq!(
                std::fs::read_to_string(dst.join("marker.txt")).expect("marker through the link"),
                *rel,
                "{rel} must resolve into the source tree"
            );
        }
    }

    /// `git check-ignore` for EVERY path, one at a time — with several
    /// paths it answers "one or more", which is not the question.
    fn ignored(&self, worktree: &Path, rels: &[&str]) -> bool {
        rels.iter().all(|rel| {
            Command::new("git")
                .args(["check-ignore", "-q", rel])
                .current_dir(worktree)
                .output()
                .expect("git check-ignore")
                .status
                .success()
        })
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/wt-web is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read wt-web");
    assert!(
        text.contains("WT_WEB_SOURCE"),
        "the source root must be overridable, so this test never reads /work/boss"
    );
}

/// The happy path: the three links land, resolve into the source tree,
/// are gitignored by the repo's own pattern, and the command runs in
/// the worktree with its argv intact.
#[test]
fn links_the_three_paths_and_execs_its_argv() {
    let f = Fixture::new("links");
    let wt = f.worktree("agent-web");

    let (rc, out) = f.run(&wt, &["webcheck", "test", "src/it/yard/"]);
    assert_eq!(rc, 0, "wt-web failed: {out}");

    f.assert_linked(&wt, &LINKS);
    for rel in LINKS {
        assert!(
            out.contains(&format!("linked {rel}")),
            "each link must be reported by name: {out}"
        );
    }
    assert!(
        out.contains("argv=test src/it/yard/"),
        "the command's argv must pass through: {out}"
    );
    assert!(
        out.contains(&format!("cwd={}", wt.display())),
        "the command must run where wt-web was run: {out}"
    );
    assert!(
        f.ignored(&wt, &LINKS),
        "the links must be ignored by the root .gitignore, or every builder worktree \
         shows three untracked paths after its first web check"
    );
}

/// A second run is a no-op that says so: the links are still the same
/// links, nothing is reported as newly linked.
#[test]
fn a_second_run_is_idempotent() {
    let f = Fixture::new("idempotent");
    let wt = f.worktree("agent-twice");

    let (rc, first) = f.run(&wt, &[]);
    assert_eq!(rc, 0, "first run failed: {first}");
    let (rc, second) = f.run(&wt, &[]);
    assert_eq!(rc, 0, "second run failed: {second}");

    f.assert_linked(&wt, &LINKS);
    assert!(
        !second.contains("linked "),
        "a second run must not report new links: {second}"
    );
    for rel in LINKS {
        assert!(
            second.contains(&format!("{rel} already")),
            "a second run must say each link already exists: {second}"
        );
    }
}

/// A source dir that does not exist (an operator checkout that never
/// installed web-kit's packages, say) is skipped BY NAME; the others
/// still link and the command still runs.
#[test]
fn a_missing_source_dir_is_skipped_with_a_message() {
    let f = Fixture::new("missing");
    let wt = f.worktree("agent-partial");
    std::fs::remove_dir_all(f.source.join("libs/web-kit/node_modules")).expect("drop one source");

    let (rc, out) = f.run(&wt, &["webcheck", "--version"]);
    assert_eq!(rc, 0, "a missing source dir is not a failure: {out}");

    f.assert_linked(&wt, &["node_modules", "apps/web/node_modules"]);
    assert!(
        std::fs::symlink_metadata(wt.join("libs/web-kit/node_modules")).is_err(),
        "no link may be made to a source that does not exist"
    );
    assert!(
        out.contains("skipping libs/web-kit/node_modules")
            && out.contains(
                f.source
                    .join("libs/web-kit/node_modules")
                    .to_str()
                    .expect("utf8")
            ),
        "the skip must name the path and the missing source: {out}"
    );
    assert!(
        out.contains("argv=--version"),
        "the command must still run: {out}"
    );
}

/// A real directory at a destination — someone ran `bun install` in
/// the worktree — is not replaced; the script says it left it alone.
#[test]
fn a_real_directory_at_a_destination_is_left_alone() {
    let f = Fixture::new("real-dir");
    let wt = f.worktree("agent-installed");
    let installed = wt.join("apps/web/node_modules");
    boss_testing::create_dir(&installed);
    write_file(&installed.join("installed.txt"), "");

    let (rc, out) = f.run(&wt, &[]);
    assert_eq!(rc, 0, "wt-web failed: {out}");

    f.assert_linked(&wt, &["node_modules", "libs/web-kit/node_modules"]);
    assert!(
        installed.join("installed.txt").exists()
            && !std::fs::symlink_metadata(&installed)
                .expect("meta")
                .file_type()
                .is_symlink(),
        "an installed directory must not be replaced by a link"
    );
    assert!(
        out.contains("apps/web/node_modules") && out.contains("not a symlink"),
        "leaving a real directory must be said, by name: {out}"
    );
}

/// With no argv there is nothing to exec: it prepares, prints what it
/// linked, and exits 0.
#[test]
fn with_no_arguments_it_only_prepares() {
    let f = Fixture::new("noargs");
    let wt = f.worktree("agent-prepare");

    let (rc, out) = f.run(&wt, &[]);
    assert_eq!(rc, 0, "wt-web failed: {out}");
    f.assert_linked(&wt, &LINKS);
    assert!(!out.contains("argv="), "nothing must be exec'd: {out}");
    for rel in LINKS {
        assert!(
            out.contains(&format!("linked {rel}")),
            "what was linked must be printed: {out}"
        );
    }
}

/// Outside a git worktree there is no root to link into; the script
/// refuses rather than dropping symlinks into whatever cwd is.
#[test]
fn outside_a_worktree_it_refuses() {
    let f = Fixture::new("nogit");
    let nowhere = f.root.join("plain-dir");
    boss_testing::create_dir(&nowhere);

    let (rc, out) = f.run(&nowhere, &["webcheck"]);
    assert_eq!(rc, 2, "must refuse with exit 2: {out}");
    assert!(out.contains("not in a git worktree"), "must say why: {out}");
    assert!(!out.contains("argv="), "the command must not run: {out}");
    assert!(
        std::fs::symlink_metadata(nowhere.join("node_modules")).is_err(),
        "nothing may be linked outside a worktree"
    );
}
