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

/// The reporting step is found by its spec KEY, never by title prose.
///
/// 48bed517: the selector used to grep the rendered title for
/// "Record", which worked by coincidence of wording. A protocol is
/// registry data we retitle freely and on purpose — and `gate-run`
/// lives ONLY as registry data on the deployment, so no repo-side pin
/// can hold the prose equal. What a repo test CAN hold is the
/// selector's contract: it keys on `spec_slug == "record-verdict"`,
/// the same identity advancement pairs steps by, and it must refuse
/// aloud on zero or many matches rather than index whatever the API
/// ordered first.
#[test]
fn the_reporting_step_is_selected_by_slug_not_prose() {
    let sh = run_sh();
    let printed: String = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        printed.contains("spec_slug\") == \"record-verdict\""),
        "run.sh must select the reporting step by spec_slug == record-verdict"
    );
    assert!(
        !printed.contains("\"Record\" in"),
        "run.sh must not match the reporting step by its rendered title — that fact lives in \
         the registry and a retitle would silently orphan every verdict"
    );
    assert!(
        printed.contains("len(hits) != 1"),
        "zero and many matches must both refuse: zero orphans the verdict, many reports it \
         onto whichever step the API happened to order first"
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
