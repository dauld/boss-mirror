//! `infra/cluster/dev-scratch-reclaim.sh` — the dev pod's reclaim
//! sidecar — reclaims the per-builder target dirs that actually
//! accumulate, not only the one path it was told about.
//!
//! Measured 2026-09-11 01:40Z (backlog 0efbd69e): /scratch held 90 GB,
//! of which /scratch/target — the ONLY dir the reclaim knew — was 28 GB
//! and twenty sibling dirs (target-item-at-open, target-ae887a1b,
//! tgt-rerail-sweep, …) held 62 GB, nearly all for branches that had
//! landed the day before. Every coding agent is briefed to use its own
//! CARGO_TARGET_DIR under /scratch, so the caches that pile up are
//! SIBLINGS of the path the reclaim scanned, and the reclaim could not
//! see one byte of them. Its floor (50 GB free) would not have fired
//! anyway with 304 GB free — a dead cache is not a headroom problem, it
//! is an AGE problem, so the pass below runs on age regardless of free
//! space. Driven as a real script against a fixture scratch mount.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn touch_at(path: &Path, hours_ago: u64) {
    let when = format!("-{hours_ago} hours");
    let ok = Command::new("touch")
        .args(["-d", &when])
        .arg(path)
        .status()
        .expect("touch")
        .success();
    assert!(ok, "touch -d {when} {}", path.display());
}

/// A dir shaped like a cargo target: `CACHEDIR.TAG` plus `debug/`, with
/// every mtime set `hours_ago`.
fn target_dir(root: &Path, name: &str, hours_ago: u64) -> PathBuf {
    let d = root.join(name);
    boss_testing::create_dir(&d.join("debug").join("deps"));
    boss_testing::write_file(
        &d.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    boss_testing::write_file(&d.join("debug").join("deps").join("libfoo.rlib"), "x");
    for p in [
        d.join("debug").join("deps").join("libfoo.rlib"),
        d.join("debug").join("deps"),
        d.join("debug"),
        d.join("CACHEDIR.TAG"),
        d.clone(),
    ] {
        touch_at(&p, hours_ago);
    }
    d
}

fn run(scratch: &Path, extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .env("SCRATCH_MOUNT", scratch)
        .env("CARGO_TARGET_DIR", scratch.join("target"))
        .env("WORK_MOUNT", scratch.join("work"))
        .env("REPO_DIR", scratch.join("work").join("repo"))
        .env("WORKTREES_DIR", scratch.join("work").join("wt"))
        .env("BOSS_STALE_TARGET_H", "12");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("run dev-scratch-reclaim.sh")
}

fn say(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_stale_sibling_target_is_reclaimed_and_the_fresh_ones_and_the_primary_are_kept() {
    let root = boss_testing::scratch_dir("boss-dsr-siblings");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let primary = target_dir(&root, "target", 72);
    let stale = target_dir(&root, "target-landed-yesterday", 30);
    let fresh = target_dir(&root, "target-live-builder", 1);
    // Not a target at all — old, but not ours to touch.
    let notes = root.join("notes");
    boss_testing::create_dir(&notes);
    boss_testing::write_file(&notes.join("a.txt"), "keep");
    touch_at(&notes.join("a.txt"), 200);
    touch_at(&notes, 200);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !stale.exists(),
        "the 30h-old sibling target must be reclaimed\n{text}"
    );
    assert!(
        fresh.exists(),
        "a sibling touched 1h ago belongs to a live builder\n{text}"
    );
    assert!(
        primary.exists(),
        "the primary CARGO_TARGET_DIR is never reclaimed by age\n{text}"
    );
    assert!(
        notes.join("a.txt").exists(),
        "a dir that is not a cargo target is not touched\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target-landed-yesterday") && stdout.contains("stale target"),
        "the reclaim must NAME the dir it removed and why\n{text}"
    );
}

#[test]
fn nothing_stale_reclaims_nothing_and_says_so() {
    let root = boss_testing::scratch_dir("boss-dsr-quiet");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let fresh = target_dir(&root, "target-live", 2);
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(fresh.exists(), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("0 stale target"),
        "a quiet pass still reports its count\n{text}"
    );
}

#[test]
fn the_age_threshold_is_validated_like_the_floors() {
    let root = boss_testing::scratch_dir("boss-dsr-badage");
    let _guard = Scratch(root.clone());
    let out = run(&root, &[("BOSS_STALE_TARGET_H", "soon")]);
    assert_eq!(out.status.code(), Some(64), "{}", say(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("STALE_TARGET_H"),
        "{}",
        say(&out)
    );
}
