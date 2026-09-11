//! Scratch directories a test owns outright, and writes that name their
//! path when they fail.
//!
//! `/tmp` is shared and sticky (`1777`). A fixture rooted at a FIXED name
//! — `std::env::temp_dir().join("boss-thing-test")` — is therefore one
//! path shared by every account on the box, and the dev pod is long-lived
//! with root and the gate's uid 65534 both running suites on it. Two
//! failures follow, and both have happened:
//!
//! 1. `remove_dir_all` on another account's directory fails, and the
//!    error was discarded. Worse, `create_dir_all` on an existing
//!    directory returns `Ok` REGARDLESS of who owns it — so the fixture
//!    reports success and the run dies at the first write inside, up to
//!    130 lines away from the cause.
//! 2. That write `unwrap()`ed an errno, so the verdict was
//!    `PermissionDenied` with no path in it. CLAUDE.md §Diagnosis: a
//!    verdict must name what failed. `Os { code: 13 }` names nothing,
//!    and on one occasion it was reported to an operator as the defect
//!    when the real defect was elsewhere.
//!
//! The fix is a root that is per-process AND per-uid, plus writes that
//! say which path they were at.
//!
//! ## Why both the pid and the uid
//!
//! The pid alone makes the root unique among LIVE processes, which
//! removes every concurrent collision. But the failure this module
//! exists to prevent is a leftover from a process that has already
//! EXITED — root ran the suite hours ago and the directory remains. The
//! pid protects that case only by the improbability of drawing the same
//! number again, and pids recycle at `/proc/sys/kernel/pid_max`. A
//! long-lived pod burns through that, so the collision is rare rather
//! than impossible, and when it lands the symptom is exactly the
//! unreadable failure above.
//!
//! The uid turns the probability into an invariant: any path a run can
//! collide with was created by the same uid, so it is a path that run
//! can always remove. The pid covers concurrency, the uid covers
//! ownership, and this is an ownership defect.

use std::path::{Path, PathBuf};

/// The uid this process runs as, for use in a path segment.
///
/// Read from `/proc/self` rather than a libc call to keep this crate
/// free of a `libc` dependency — the technique already in-tree. If
/// `/proc` is not mounted we cannot learn the uid, and a FIXED fallback
/// would silently restore the very collision this exists to prevent, so
/// the fallback is a value no real uid takes.
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(u32::MAX)
}

/// The path `scratch_dir` would use, without creating anything.
///
/// Exposed so a test can assert on the naming, and so a caller that
/// needs a sibling path can derive one.
pub fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{name}-{}-{}", current_uid(), std::process::id()))
}

/// An empty scratch directory this process owns outright.
///
/// Panics naming the path if the directory cannot be cleared or created.
/// A removal failure is NOT discarded: a pre-existing root we cannot
/// clear means the fixture is not in the state the test is about to
/// assume, and carrying on is what made the original failures
/// unreadable. `NotFound` is the normal case and is not an error.
pub fn scratch_dir(name: &str) -> PathBuf {
    let dir = scratch_path(name);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        assert!(
            e.kind() == std::io::ErrorKind::NotFound,
            "scratch dir {} already exists and cannot be cleared: {e}. \
             It is most likely a leftover owned by another account — \
             /tmp is shared and sticky, so this run can neither remove \
             nor write it. Remove it as its owner and re-run.",
            dir.display()
        );
    }
    create_dir(&dir);
    dir
}

/// Create a directory and every parent, naming the path on failure.
pub fn create_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("create dir {}: {e}", path.display()));
}

/// Write a file, naming the path on failure.
///
/// A bare `unwrap()` on a write reports the errno and not the file,
/// which is the whole diagnosis.
pub fn write_file(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Write an executable file (mode `0755`), naming the path on failure.
pub fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write_file(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 0755 {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the module: the root carries both the uid and the
    /// pid, so no two accounts and no two live processes share one.
    #[test]
    fn a_scratch_root_carries_the_uid_and_the_pid() {
        let dir = scratch_path("boss-scratch-naming");
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .expect("the root has a final component");
        let expected = format!(
            "boss-scratch-naming-{}-{}",
            current_uid(),
            std::process::id()
        );
        assert_eq!(
            name, expected,
            "a scratch root must be per-uid and per-process, or a \
             leftover from another account collides with it"
        );
    }

    /// A fresh root is empty even when the previous run of the same
    /// process left files in it — the removal is real, not best-effort.
    #[test]
    fn a_scratch_root_comes_back_empty() {
        let dir = scratch_dir("boss-scratch-reuse");
        write_file(&dir.join("stale"), "from an earlier call");
        let again = scratch_dir("boss-scratch-reuse");
        assert_eq!(dir, again, "the same name yields the same root");
        assert!(
            !again.join("stale").exists(),
            "scratch_dir must clear what it finds, so a test never reads \
             an earlier run's file: {} survived",
            again.join("stale").display()
        );
        let _ = std::fs::remove_dir_all(&again);
    }

    /// A write that fails must name its path. This is the §Diagnosis
    /// requirement, and the reason the original failures cost so much:
    /// `Os { code: 13 }` alone sends a human to re-derive the path.
    #[test]
    fn a_failed_write_names_its_path() {
        let dir = scratch_dir("boss-scratch-write-failure");
        // A directory cannot be overwritten as a file, so this write
        // fails for every uid including root — no ownership needed.
        let clash = dir.join("a-directory");
        create_dir(&clash);
        let err = std::panic::catch_unwind(|| write_file(&clash, "body"))
            .expect_err("writing a file over a directory fails");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("<not a string panic>")
            .to_string();
        assert!(
            msg.contains(&clash.display().to_string()),
            "a write failure must name the path it was at; got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root that cannot be cleared must REFUSE, naming the path —
    /// never carry on into `create_dir_all`, which returns `Ok` on a
    /// directory belonging to someone else and so hides the problem
    /// until the first write.
    ///
    /// The live hazard is a root-owned leftover, which a test cannot
    /// manufacture without `CAP_CHOWN`. An occupied path reaches the
    /// same branch: `remove_dir_all` over a REGULAR FILE returns
    /// `NotADirectory`, not `NotFound`, for every uid including root.
    #[test]
    fn a_root_that_cannot_be_cleared_refuses_by_name() {
        let name = "boss-scratch-unclearable";
        let occupied = scratch_path(name);
        let _ = std::fs::remove_dir_all(&occupied);
        write_file(&occupied, "a regular file where the root should go");

        let err = std::panic::catch_unwind(|| scratch_dir(name))
            .expect_err("an unclearable root must panic, not proceed");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("<not a string panic>")
            .to_string();
        assert!(
            msg.contains(&occupied.display().to_string()),
            "the refusal must name the path that could not be cleared, \
             because an errno alone is what made this defect unreadable; \
             got: {msg}"
        );
        let _ = std::fs::remove_file(&occupied);
    }
}
