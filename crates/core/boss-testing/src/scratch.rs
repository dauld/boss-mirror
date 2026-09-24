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
//!
//! ## Why a sequence number as well
//!
//! Every test of one binary runs in ONE process, so the pid tells two
//! of them apart from nothing. A name two tests share — a
//! `plain_tenant()` helper four tests call — was therefore one path,
//! and under the parallel runner one test's `remove_dir_all` raced
//! another's first write: `write` panicked at a path that had existed
//! a moment before (backlog 6eaef658, second instance, 2026-09-18).
//! `scratch_dir` now hands out a fresh child of the per-name root on
//! every call, numbered from a process-wide counter, so the same name
//! from two tests — or twice from one — never shares a directory.
//!
//! ## Why an executable is written by a child process
//!
//! Linux refuses to exec a file any process holds open for writing
//! (`ETXTBSY`). A test that writes a script and runs it closes the
//! descriptor first — and still hit `Text file busy` once in a full
//! run (backlog 6eaef658, first instance). The writer was not the
//! holder: a child spawned by a SIBLING thread inherits every open
//! descriptor of this process until its own exec, so a write
//! descriptor open at the instant any other test spawns is briefly
//! open in that child too, and the exec that follows the close loses
//! the race. Measured with four spawning threads: 3–9 of 50 fresh
//! scripts refused. The only fix that removes the race rather than
//! narrowing it is to never hold the file open for writing in this
//! process at all — `write_exec` streams the body to a child that does
//! the writing and has exited before the call returns.
//!
//! ## …and so is one rewritten
//!
//! `write_exec` covers the file a test CREATES executable. A test that
//! copies a door in with `write_exec` and then EDITS it with
//! `write_file` — to make an uncommitted change, say — truncates a file
//! that is already executable, the mode bits survive the write, and the
//! exec that follows is the same race without a single chmod the
//! `an_executable_is_written_by_write_exec` pin can see. Train 07:52
//! went red on it (`a_door_knows_its_copy_is_stale.rs:232`, `run
//! boss-api: Text file busy`; backlog c9511f54, 2026-09-24). So
//! `write_file` asks first: a path that is already executable goes
//! through the same child writer, in place, mode kept. Measured with
//! four spawning threads: 6 of 50 rewritten executables refused before,
//! 0 after. Writing to a temporary name and renaming it into place is
//! NOT a fix — the rename moves the same inode a sibling's child still
//! holds open, and measured 2 of 50.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// The per-name root this process and uid own, without creating
/// anything. `scratch_dir(name)` hands out a fresh child of it.
///
/// Exposed so a test can assert on the naming, and so a caller that
/// needs a path that must NOT exist can derive one.
pub fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{name}-{}-{}", current_uid(), std::process::id()))
}

/// One per call, process-wide: the component that tells two tests of
/// one binary — one pid — apart (module note, "Why a sequence number").
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An empty scratch directory this process owns outright, distinct
/// from every other call's — including another call with this name.
///
/// Panics naming the path if the directory cannot be cleared or created.
/// A removal failure is NOT discarded: a pre-existing root we cannot
/// clear means the fixture is not in the state the test is about to
/// assume, and carrying on is what made the original failures
/// unreadable. `NotFound` is the normal case and is not an error.
pub fn scratch_dir(name: &str) -> PathBuf {
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    cleared(scratch_path(name).join(seq.to_string()))
}

/// `dir`, emptied of whatever a recycled pid left there and created.
fn cleared(dir: PathBuf) -> PathBuf {
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
///
/// A path that is ALREADY executable is rewritten by the child writer
/// `write_exec` uses, keeping its mode (module note, "…and so is one
/// rewritten").
pub fn write_file(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let executable = std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if executable {
        write_through_a_child(path, body);
    } else {
        std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}

/// Write an executable file (mode `0755`), naming the path on failure.
///
/// The body is written by a child process, not by this one, so no
/// thread of this process ever holds the file open for writing and a
/// sibling's spawn cannot inherit that descriptor into a child that
/// keeps it past our exec (module note, "Why an executable is written
/// by a child process"). The child has exited before this returns.
pub fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write_through_a_child(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 0755 {}: {e}", path.display()));
}

/// `body` into `path` by a `sh -c 'cat > "$1"'` that has exited before
/// this returns, so this process never holds `path` open for writing.
/// Truncates in place: an existing file keeps its inode and its mode.
fn write_through_a_child(path: &Path, body: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("write {}: spawn the writer: {e}", path.display()));
    let stdin = child.stdin.take();
    stdin
        .expect("a piped stdin")
        .write_all(body.as_bytes())
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    let out = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "write {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
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

    /// A fresh root is empty even when an earlier process with this pid
    /// left files at the same path — the removal is real, not
    /// best-effort. Exercised on the clearing step itself, because
    /// `scratch_dir` never hands the same path out twice.
    #[test]
    fn a_scratch_root_comes_back_empty() {
        let dir = scratch_dir("boss-scratch-reuse");
        write_file(&dir.join("stale"), "from an earlier process");
        let again = cleared(dir.clone());
        assert_eq!(dir, again, "clearing keeps the path");
        assert!(
            !again.join("stale").exists(),
            "a scratch root must be cleared of what a recycled pid left, \
             so a test never reads an earlier run's file: {} survived",
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

    /// Two tests of ONE binary share a pid, so a name shared between
    /// them — a `plain_tenant()` helper four tests call — was one path
    /// until 2026-09-18: one test's `remove_dir_all` raced another's
    /// first write (backlog 6eaef658, panicked at the write). Every call
    /// now gets its own root, even for the same name in the same
    /// process.
    #[test]
    fn two_calls_with_one_name_yield_two_roots() {
        let a = scratch_dir("boss-scratch-twice");
        let b = scratch_dir("boss-scratch-twice");
        assert_ne!(
            a, b,
            "two calls with the same name must not share a root: tests \
             of one binary share a pid, and a shared root is a race \
             between one test's clear and another's first write"
        );
        write_file(&a.join("mine"), "a");
        assert!(a.join("mine").exists() && !b.join("mine").exists());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// An executable `write_exec` wrote can be run at once, while
    /// another thread of this process keeps spawning children — the
    /// shape of a parallel test binary. Linux refuses to exec a file
    /// any process holds open for writing (ETXTBSY), and a child forked
    /// by a SIBLING thread inherits every open descriptor until its own
    /// exec, so a write descriptor that is still open in this process
    /// at the instant a sibling spawns is briefly open in that child
    /// too. That is the once-in-a-run `Text file busy` the shim test hit
    /// (backlog 6eaef658, 2026-09-18). The write must therefore never
    /// hold the file open in this process at all.
    #[test]
    fn an_executable_just_written_can_be_execd_while_siblings_spawn() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = scratch_dir("boss-scratch-exec-race");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let spawners: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("true").output();
                    }
                })
            })
            .collect();
        let mut busy = 0;
        for i in 0..50 {
            let exe = dir.join(format!("exe-{i}"));
            write_exec(&exe, "#!/bin/sh\nexit 0\n");
            match std::process::Command::new(&exe).output() {
                Ok(o) => assert!(o.status.success(), "{} failed", exe.display()),
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => busy += 1,
                Err(e) => panic!("exec {}: {e}", exe.display()),
            }
        }
        stop.store(true, Ordering::Relaxed);
        for s in spawners {
            s.join().expect("a spawner thread ends");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            busy, 0,
            "{busy} of 50 fresh executables answered ExecutableFileBusy: \
             write_exec left its file open for writing in this process \
             while a sibling thread spawned"
        );
    }

    /// An executable REWRITTEN through `write_file` — a test editing a
    /// copied door, then running it — can be run at once while siblings
    /// spawn. The truncating write keeps the mode bits, so the file is an
    /// executable this process held open for writing without a single
    /// chmod the `write_exec` pin could see: train 07:52 went red on
    /// exactly this in `a_door_knows_its_copy_is_stale` (`run boss-api:
    /// Text file busy`, backlog c9511f54, 2026-09-24).
    #[test]
    fn an_executable_rewritten_by_write_file_can_be_execd_while_siblings_spawn() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = scratch_dir("boss-scratch-rewrite-race");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let spawners: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("true").output();
                    }
                })
            })
            .collect();
        let mut busy = 0;
        for i in 0..50 {
            let exe = dir.join(format!("exe-{i}"));
            write_exec(&exe, "#!/bin/sh\nexit 1\n");
            write_file(&exe, "#!/bin/sh\nexit 0\n# an edit in progress\n");
            match std::process::Command::new(&exe).output() {
                Ok(o) => assert!(o.status.success(), "{} ran the old body", exe.display()),
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => busy += 1,
                Err(e) => panic!("exec {}: {e}", exe.display()),
            }
        }
        stop.store(true, Ordering::Relaxed);
        for s in spawners {
            s.join().expect("a spawner thread ends");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            busy, 0,
            "{busy} of 50 executables rewritten by write_file answered ExecutableFileBusy: \
             the rewrite held the file open for writing in this process while a sibling \
             thread spawned"
        );
    }

    /// `write_file` over a plain file stays a plain in-process write: the
    /// child writer is for executables only, and a data file must not
    /// come out executable because it went through the same function.
    #[test]
    fn a_plain_file_rewritten_by_write_file_stays_plain() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("boss-scratch-rewrite-plain");
        let p = dir.join("data.json");
        write_file(&p, "{}\n");
        write_file(&p, "{\"a\":1}\n");
        let mode = std::fs::metadata(&p).expect("stat").permissions().mode();
        assert_eq!(std::fs::read_to_string(&p).expect("read"), "{\"a\":1}\n");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode & 0o111, 0, "a plain file came out executable");
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
