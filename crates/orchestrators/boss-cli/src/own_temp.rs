//! A path under the shared temp directory that this uid AND this process
//! own — for the verbs themselves, which cannot reach
//! `boss_testing::scratch` (a dev-dependency) and so used to spell the
//! pid alone.
//!
//! WHY THE UID (backlog 307df975, 2026-09-23). `/tmp` is mode 1777 and
//! the dev pod runs `boss` as root AND as the gate's uid 65534. A pid
//! keeps two LIVE processes apart; it says nothing about a leftover from
//! one that has exited, and pids recycle. `boss gate --rebase` named its
//! replay worktree `boss-gate-rebase-<pid>-<attempt>-<head>`, so a
//! leftover from the other uid holding that name is a directory this run
//! can neither remove nor add a worktree at — and `remove_dir_all`'s
//! EPERM was discarded, surfacing one step later as "adding the
//! temporary worktree" failing. With the uid in the name, every path a
//! run can collide with is one it created itself, and so one it can
//! always remove. The reasoning is `boss_testing::scratch`'s module note
//! ("Why both the pid and the uid"); this is the same technique, not a
//! shared fact, so there is nothing for an equality test to pin
//! (CLAUDE.md §9a) — and `a-fixture-path-cannot-be-a-literal.sh` now
//! refuses a computed temp name that carries the pid without the uid.

use std::path::PathBuf;

/// The uid this process runs as. `/proc/self` rather than a libc call,
/// the in-tree technique (`boss_testing::scratch`, `boss-core`'s config
/// tests), so this adds no dependency. Where `/proc` is absent (macOS),
/// the fallback is a value no real uid takes — and there `temp_dir()`
/// is already per-user, so nothing is shared to collide on.
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(u32::MAX)
}

/// `<temp_dir>/<name>-<uid>-<pid>`, created by nobody: the caller makes
/// what it needs there. Two calls in one process with one `name` are one
/// path, so a caller that can run twice concurrently puts its own
/// counter in `name` (the rebase replay does).
pub(crate) fn own_temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{name}-{}-{}", current_uid(), std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name carries the uid as `id -u` reports it — an independent
    /// reading, not the same `/proc` call twice — and the pid.
    #[test]
    fn the_path_carries_the_uid_and_the_pid() {
        let id = std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("run id -u");
        let uid = String::from_utf8_lossy(&id.stdout).trim().to_string();
        let path = own_temp_path("boss-own-temp-test");
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("boss-own-temp-test-{uid}-{}", std::process::id()).as_str()),
            "a verb's temp path must carry the uid AND the pid (307df975)"
        );
    }
}
