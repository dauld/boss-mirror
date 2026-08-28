//! A gate that cannot record its verdict has to say so truthfully.
//!
//! THE INCIDENT (cf0021ae, 2026-08-25). `infra/gate.sh` passed 30/30 on
//! `chore/the-build-leaves-the-control-plane`, wrote `verdict: green`,
//! and then w-1 rebooted before the pod finished. The verdict existed
//! in exactly two places and both went quiet at once:
//!
//! - the receipt was on the PVC, and had to be recovered by mounting
//!   the disk in a throwaway pod;
//! - the step write was refused, because the gate-run packet had been
//!   reused across relaunches and was already TERMINAL.
//!
//! The fallback said `packet will go overdue (the alarm still works)`.
//! That is true of an OPEN packet and false of a closed one — a
//! terminal packet can never go overdue, so the reassurance was
//! strongest exactly where it was wrong.
//!
//! Two properties are pinned. The first is cheap insurance: the verdict
//! goes to stdout unconditionally, so `kubectl logs` is a third copy
//! that needs no volume mount. The second is the one that matters: the
//! failure branch must distinguish an open packet from a terminal one,
//! and must not promise an alarm in the case where none can fire.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn run_sh() -> String {
    let path = repo_root().join("infra/gate-runner/run.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The receipt reaches the pod log whatever else happens to it.
#[test]
fn the_verdict_is_echoed_before_it_is_reported() {
    let sh = run_sh();
    let echo = sh
        .find("echo \"gate-runner: receipt")
        .expect("run.sh must echo the receipt summary to stdout");
    let report = sh
        .find("if ! report \"$VERDICT\"")
        .expect("run.sh must still report the verdict to the packet");
    assert!(
        echo < report,
        "the receipt must be echoed BEFORE the report is attempted — the whole point is that \
         the log survives when the report does not"
    );
}

/// THE ONE THAT WAS WRONG BEFORE.
#[test]
fn a_failed_report_does_not_promise_an_alarm_that_cannot_fire() {
    let sh = run_sh();

    // Comment lines are skipped. The property is that run.sh must not
    // PRINT the unconditional claim — not that it may never mention it.
    // The fix documents the old wording verbatim to explain why it was
    // wrong, and a test that forbade the words outright would delete its
    // own rationale, the way a lint that excludes everything but the
    // spec proving it does.
    let printed: String = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !printed.contains("the alarm still works"),
        "run.sh still PRINTS an unconditional overdue-alarm claim. A TERMINAL packet cannot \
         go overdue, and that is precisely the case that lost the verdict on 2026-08-25."
    );

    assert!(
        sh.contains("packet status:") || sh.contains("$state"),
        "the failure branch must read the packet's status — an open packet and a closed one \
         need opposite messages"
    );
    assert!(
        sh.contains("NOTHING WILL GO OVERDUE"),
        "the terminal-packet branch must say plainly that no alarm will fire; that silence \
         is what made the run look like it never happened"
    );
}

/// Failing to RECORD a green gate must not turn it into a red one.
///
/// The sibling half of cf0021ae is that the Job status already lies
/// about green gates whose pods die. Exiting non-zero here because a
/// packet write failed would manufacture the same lie from the other
/// direction.
#[test]
fn the_exit_status_still_follows_the_gate_not_the_report() {
    let sh = run_sh();
    let tail = sh
        .rfind("[ \"$VERDICT\" = green ]")
        .expect("run.sh must end by exiting on the gate verdict");
    let report = sh
        .find("if ! report \"$VERDICT\"")
        .expect("report block present");
    assert!(
        tail > report,
        "the final exit must be decided by the gate verdict, after the reporting block"
    );
    let block = &sh[report..tail];
    assert!(
        !block.contains("\nexit 1"),
        "the reporting-failure branch must not exit non-zero: that would report a green gate \
         as a failed run, which is the confusion cf0021ae is about"
    );
}

/// The step selector is LIFTED OUT OF run.sh AND RUN, not read.
///
/// It used to match `"Record" in title` — a substring of human-facing
/// prose in a registry row anyone may edit. Zero matches raised an
/// IndexError forty minutes into a gate, with the work done and nowhere
/// to report it; two matches reported onto whichever step the API
/// happened to order first. Filed as 48bed517; the fix keys on
/// `spec_slug`, which is what train.rs's own find_step prefers.
///
/// Asserting the source contains "spec_slug" would pass on a script that
/// never reaches the branch, so the selector is extracted and executed
/// against crafted packets.
fn selector() -> String {
    let sh = run_sh();
    let start = sh
        .find("| python3 -c 'import sys,json\n")
        .expect("run.sh no longer selects its step with an inline python3 -c");
    let body = &sh[start + "| python3 -c '".len()..];
    let end = body
        .find("') || return 1")
        .expect("selector is not closed as expected");
    body[..end].to_string()
}

fn run_selector(packet: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(selector())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("python3 runs the selector");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(packet.as_bytes())
        .expect("write packet");
    child.wait_with_output().expect("selector finishes")
}

#[test]
fn the_selector_finds_the_step_by_key_not_by_title() {
    // Titles deliberately say nothing about "Record" — keying on prose
    // would find nothing here, which is the point.
    let packet = r#"{"steps":[
        {"id":"aaa","spec_slug":"launched","title":"Gate launched"},
        {"id":"bbb","spec_slug":"record-verdict","title":"Whatever an operator renamed this to"}
    ]}"#;
    let out = run_selector(packet);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "bbb");
}

#[test]
fn a_packet_without_the_step_fails_loudly_and_names_the_receipt() {
    let packet = r#"{"steps":[{"id":"aaa","spec_slug":"launched","title":"Gate launched"}]}"#;
    let out = run_selector(packet);
    assert!(!out.status.success(), "a missing step must not be silent");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("found 0"), "{err}");
    assert!(
        err.contains("receipt.json"),
        "the gate has already run — say where its result is: {err}"
    );
}

/// Two matches must be an error, never a pick. The old selector took
/// `[0]` and reported onto an arbitrary step.
#[test]
fn a_packet_with_two_matching_steps_is_refused() {
    let packet = r#"{"steps":[
        {"id":"aaa","spec_slug":"record-verdict","title":"Record the receipt"},
        {"id":"bbb","spec_slug":"record-verdict","title":"Record the receipt again"}
    ]}"#;
    let out = run_selector(packet);
    assert!(
        !out.status.success(),
        "ambiguity must not be resolved silently"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("found 2"));
}
