//! A stub server announcing its port to the test that spawned it.
//!
//! The python stubs under `tests/` bind port 0 and write the port they
//! got to a file the test polls for. Four of them wrote it with
//! `open(path, "w")` then `f.write(port)`, and the test took
//! `path.exists()` as "the port is there" — but the file exists from the
//! `open`, before a byte is in it. Under gate load the test read an empty
//! file and died `ParseIntError { kind: Empty }`: gate cf3a4d84, a
//! web-only car struck for it (backlog 0d1e557e, 2026-09-24). The
//! same shape sat in `surface_usage_sh.rs`, `protocol_drift_sh.rs`,
//! `codebase_metrics_files_from_a_file.rs` and
//! `gate_runner_report_retry.rs`, each written a copy apart.
//!
//! Both halves of the contract live here, so a fifth stub cannot inherit
//! the race: [`ANNOUNCE_PY`] is the writer, and it makes the file
//! appear WHOLE OR NOT AT ALL; [`await_announced_port`] is the reader,
//! and it may therefore take existence as completeness.

use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

/// Python defining `announce(path, text="ok")`. Prepend it to a stub's
/// source with [`with_announce`] and call it wherever the stub states a
/// fact the test waits on — its port, or that it is listening. The text
/// goes to a sibling temporary name and `os.replace` moves it onto
/// `path`: a rename within one directory is atomic, so `path` goes from
/// absent to complete with no empty moment between.
pub const ANNOUNCE_PY: &str = r#"
import os as _announce_os
def announce(path, text="ok"):
    tmp = path + ".announcing"
    with open(tmp, "w") as f:
        f.write(text)
    _announce_os.replace(tmp, path)
"#;

/// A stub's source with [`ANNOUNCE_PY`] in front of it.
pub fn with_announce(stub: &str) -> String {
    format!("{ANNOUNCE_PY}{stub}")
}

/// Wait up to `limit` for the stub to announce its port in `path`, and
/// return it. Existence is completeness because [`ANNOUNCE_PY`] renames
/// the file into place; a stub that exits first fails fast, since its
/// stderr (inherited) is the diagnosis. Every panic names the FIXTURE,
/// so a stub that never came up cannot be read as the script under test
/// failing.
///
/// # Panics
/// If the stub exits before announcing, `limit` passes, or the file
/// holds something other than a non-zero port.
pub fn await_announced_port(child: &mut Child, path: &Path, limit: Duration) -> u16 {
    let deadline = Instant::now() + limit;
    while !path.exists() {
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "the stub exited early ({status}) before announcing {}; its stderr is above",
                path.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the stub never announced its port in {} after {limit:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read the stub's announcement {}: {e}", path.display()));
    match text.trim().parse::<u16>() {
        Ok(port) if port != 0 => port,
        _ => panic!(
            "{} carries {text:?}, not a port: a stub announces through \
             boss_testing::announce::ANNOUNCE_PY, which writes the file whole or not at all",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{await_announced_port, with_announce};
    use crate::scratch::{scratch_dir, write_file};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn have_python() -> bool {
        Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// The race itself, made deterministic: every `open()` the announce
    /// makes is spied on, and at each one the TARGET is inspected. A
    /// reader polling `exists()` sees exactly what the spy sees, so if
    /// the spy ever finds the target present but unfinished, so could
    /// the test — which is what gate cf3a4d84 did.
    #[test]
    fn an_announcement_is_never_visible_unfinished() {
        if !have_python() {
            eprintln!("skipping: python3 absent — not manufacturing a red");
            return;
        }
        let dir = scratch_dir("announce-unfinished");
        let target = dir.join("started");
        let spy = with_announce(
            r#"
import builtins, os, sys
target = sys.argv[1]
real_open = builtins.open
def spy(p, *a, **k):
    f = real_open(p, *a, **k)
    if os.path.exists(target):
        with real_open(target) as g:
            seen = g.read()
        if seen != "54321":
            print("UNFINISHED " + repr(seen))
    return f
builtins.open = spy
announce(target, "54321")
builtins.open = real_open
print("LEFTOVERS=" + ",".join(sorted(n for n in os.listdir(os.path.dirname(target)) if n not in ("started", "spy.py"))))
"#,
        );
        let script = dir.join("spy.py");
        write_file(&script, &spy);
        let out = Command::new("python3")
            .arg(&script)
            .arg(&target)
            .output()
            .expect("python3");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the spy exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("UNFINISHED"),
            "a reader could see the announcement before it was written:\n{stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("the announcement"),
            "54321"
        );
        assert!(
            stdout.lines().any(|l| l == "LEFTOVERS="),
            "the announce left a temporary file behind:\n{stdout}"
        );
    }

    /// The reader half: a stub that announces late is waited for, and
    /// the port it wrote is the port returned.
    #[test]
    fn the_announced_port_is_the_port_returned() {
        if !have_python() {
            eprintln!("skipping: python3 absent — not manufacturing a red");
            return;
        }
        let dir = scratch_dir("announce-port");
        let target = dir.join("started");
        let script = dir.join("stub.py");
        write_file(
            &script,
            &with_announce(
                "import sys, time\ntime.sleep(0.2)\nannounce(sys.argv[1], '4242')\ntime.sleep(30)\n",
            ),
        );
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(&target)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("python3");
        let port = await_announced_port(&mut child, &target, Duration::from_secs(20));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(port, 4242);
    }

    /// A stub that dies before announcing is the fixture's failure, said
    /// at once rather than after the whole wait.
    #[test]
    #[should_panic(expected = "the stub exited early")]
    fn a_stub_that_dies_first_is_named_at_once() {
        if !have_python() {
            panic!("the stub exited early (python3 absent)");
        }
        let dir = scratch_dir("announce-dies");
        let target = dir.join("started");
        let mut child = Command::new("python3")
            .args(["-c", "import sys; sys.exit(3)"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3");
        await_announced_port(&mut child, &target, Duration::from_secs(20));
    }
}
