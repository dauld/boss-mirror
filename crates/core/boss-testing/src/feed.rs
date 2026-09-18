//! Feeding a child process its stdin when the child may refuse before
//! it reads.
//!
//! A script under test that REFUSES on its arguments (no declaration
//! file, a usage error) exits before it reads stdin. The test's write
//! then races the exit: most runs the pipe buffer takes the bytes first;
//! under gate load the child is gone first and the write fails with
//! `EPIPE`. Both check-declared twins (`talos`, `dns`) had this race;
//! the talos one was fixed on backlog 28f29f0b and the dns one, written
//! with the same `write_all(..).unwrap()` a week apart, kept it — and
//! redded a car that touched no DNS file on 2026-09-18 (gate 8a1c65e7,
//! backlog d0eafe94). The ONE definition of "a closed pipe is the
//! child's verdict, not the test's failure" lives here so a third twin
//! cannot inherit the unwrap.

use std::io::{ErrorKind, Write};
use std::process::Child;

/// Write `bytes` to the child's piped stdin and close it. A broken pipe
/// is NOT an error: the child exited before reading, which is exactly
/// what a refusal under test does, and its verdict is its exit status —
/// read that, not this. Any other write error is the test's failure and
/// names itself.
///
/// # Panics
/// If the child's stdin was not piped (`Stdio::piped()`), or the write
/// fails for a reason other than `BrokenPipe`.
pub fn feed_stdin(child: &mut Child, bytes: &[u8]) {
    let mut stdin = child
        .stdin
        .take()
        .expect("the child's stdin must be Stdio::piped() to be fed");
    match stdin.write_all(bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin to the child: {e}"),
    }
    // Dropping `stdin` closes the pipe so a child that DOES read sees EOF.
}

#[cfg(test)]
mod tests {
    use super::feed_stdin;
    use std::process::{Command, Stdio};

    /// A child that exits without reading: the write may or may not
    /// hit EPIPE depending on timing, and either way the call returns
    /// and the exit status is the verdict.
    #[test]
    fn a_child_that_refuses_before_reading_is_its_exit_status() {
        for _ in 0..20 {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 4"])
                .stdin(Stdio::piped())
                .spawn()
                .expect("spawn sh");
            // Larger than a pipe buffer so a slow writer cannot finish
            // before the child is gone.
            feed_stdin(&mut child, &vec![b'x'; 1 << 20]);
            let status = child.wait().expect("wait");
            assert_eq!(status.code(), Some(4));
        }
    }

    /// A child that reads gets every byte and EOF.
    #[test]
    fn a_child_that_reads_gets_the_bytes_and_eof() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "wc -c"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        feed_stdin(&mut child, &[b'y'; 12345]);
        let out = child.wait_with_output().expect("wait");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "12345");
    }
}
