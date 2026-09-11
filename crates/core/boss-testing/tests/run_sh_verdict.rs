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

use boss_testing::repo_root;

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
        .find("if report \"$VERDICT\"")
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

/// An unreported verdict is a failed RUN — distinct from a red gate.
///
/// This pin used to say the opposite: that a failed packet write must
/// not change the exit status, because "reporting a green gate as red
/// is the confusion cf0021ae is about". The night of 2026-09-07 showed
/// the cost of that reading (backlog 23188cc5): four gates finished
/// during a SoR roll, each exited 0, each Job read Complete beside a
/// packet that never closed — and nothing on any surface looked wrong
/// until a human noticed the auto-park had not fired. A Job that could
/// not record its result did not finish its job, and Complete says it
/// did.
///
/// The two truths are kept apart by the exit CODE and the log: exit 75
/// (EX_TEMPFAIL) is not a red gate's 1, and the `gate-runner:
/// UNREPORTED` line carries the verdict the packet should have had.
/// `boss gate --wait` already reads "failed Job, silent packet" as
/// "read the log, this is NOT a red gate" — so a green is found, not
/// mistaken for red, and never re-proven.
#[test]
fn an_unreported_verdict_fails_the_run_with_its_own_exit_code() {
    let sh = run_sh();
    let printed: String = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = printed
        .rfind("[ \"$VERDICT\" = green ]")
        .expect("run.sh must end by exiting on the gate verdict");
    let report = printed
        .find("if report \"$VERDICT\"")
        .expect("report block present");
    assert!(
        tail > report,
        "a REPORTED verdict still exits on the gate verdict, after the reporting block"
    );
    let block = &printed[report..tail];
    assert!(
        !block.contains("\nexit 1") && !block.contains(" exit 1\n"),
        "an unreported verdict must not exit 1 — that is a red gate's code, and the two \
         must stay distinguishable from the Job alone"
    );
    let unreported = block
        .find("gate-runner: UNREPORTED verdict=$VERDICT packet=$GATE_RUN_JOB_ID")
        .expect("the unreported branch prints one greppable line naming verdict and packet");
    let exit = block
        .find("exit 75")
        .expect("the unreported branch exits 75 (EX_TEMPFAIL)");
    assert!(
        unreported < exit,
        "the UNREPORTED line must be printed before the run exits"
    );
    assert!(
        block[unreported..exit].contains("receipt $SUMMARY"),
        "the UNREPORTED line carries the receipt summary, so the log alone can re-report it"
    );
}

// ---------------------------------------------------------------------
// The receipt the packet KEEPS
// ---------------------------------------------------------------------
// `infra/gate.sh` writes a rich account of the run — mode, scope, head,
// dirty, host, ci, free_gb, unverifiable[], every check with its result
// and duration, and `refused_because` when it declined. The runner then
// REDUCED it to `{verdict, head, mode, fails}` before reporting, and the
// full account died on the pod's emptyDir with `gate.log`. Measured on a
// live gate-run packet (86553d4f, 2026-09-09): the receipt stored on the
// packet was 101 characters.
//
// Everything the reduction dropped is evidence someone later had to go
// and re-derive, which is the defect class §Diagnosis names first: "a
// verdict someone must go re-derive is not a verdict". `boss-jobs`'
// yard already reads `receipt.checks` to name a red gate's failing check
// and got nothing, because the field never reached the packet.

const SUM_BEGIN: &str = "# --- receipt summary (begin) ---";
const SUM_END: &str = "# --- receipt summary (end) ---";

/// The summary block, lifted out of `run.sh` exactly as it ships, so the
/// test exercises the reduction rather than asserting it was written.
fn summary_block() -> String {
    let src = run_sh();
    let start = src.find(SUM_BEGIN).unwrap_or_else(|| {
        panic!(
            "run.sh has no `{SUM_BEGIN}` marker — the block that decides what the packet \
             keeps must be bracketed so it can be run as it ships"
        )
    });
    let end = src
        .find(SUM_END)
        .unwrap_or_else(|| panic!("run.sh has no `{SUM_END}` marker"));
    assert!(start < end, "receipt-summary markers are out of order");
    src[start..end].to_string()
}

/// Run the lifted block over a receipt file and return what it produced.
fn summarize(tag: &str, receipt_body: &str) -> String {
    let dir = std::env::temp_dir().join(format!("boss-gate-summary-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let receipt = dir.join("receipt.json");
    std::fs::write(&receipt, receipt_body).expect("write receipt");
    let harness = dir.join("harness.sh");
    std::fs::write(
        &harness,
        format!(
            "set -euo pipefail\nRECEIPT={r}\nHEAD_SHA=deadbeefdeadbeef\n{block}\nprintf '%s' \"$SUMMARY\"\n",
            r = receipt.display(),
            block = summary_block(),
        ),
    )
    .expect("write harness");
    let out = std::process::Command::new("bash")
        .arg(&harness)
        .output()
        .expect("bash runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the summary block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A receipt in the shape `infra/gate.sh` writes today.
const WIDE_RECEIPT: &str = r#"{
  "verdict": "green",
  "mode": "full",
  "scope": "",
  "head": "c8ccb133770f6507422a7d2438261213e9e897ce",
  "dirty": false,
  "host": "gate-runner-abc123",
  "ci": true,
  "free_gb": 91,
  "unverifiable": ["infra/postgres/schema/0123_x.sql"],
  "checks": [{"name":"fmt","result":"pass","seconds":3},
             {"name":"test","result":"fail","seconds":812}]
}"#;

/// THE ONE THIS CAR EXISTS FOR: the packet keeps the whole receipt.
#[test]
fn the_reported_receipt_is_the_whole_receipt() {
    let summary = summarize("wide", WIDE_RECEIPT);
    let got: serde_json::Value = serde_json::from_str(&summary)
        .unwrap_or_else(|e| panic!("the summary must be JSON ({e}): {summary}"));
    let want: serde_json::Value = serde_json::from_str(WIDE_RECEIPT).expect("fixture parses");

    for key in [
        "verdict",
        "mode",
        "scope",
        "head",
        "dirty",
        "host",
        "ci",
        "free_gb",
        "unverifiable",
        "checks",
    ] {
        assert_eq!(
            got.get(key),
            want.get(key),
            "`{key}` must survive to the packet — the runner reduced the gate's account to \
             four fields and the rest died with the pod:\n{summary}"
        );
    }
    // The durations are the half a reader needs to tell a starved check
    // from a broken one, and they are nested inside `checks`.
    assert_eq!(
        got.pointer("/checks/1/seconds"),
        Some(&serde_json::json!(812)),
        "per-check durations must reach the packet:\n{summary}"
    );
}

/// The summary is ONE line, because the log copy is one `echo`.
///
/// `gate-runner: receipt $SUMMARY` is the third copy of the verdict —
/// the one that survives when the PVC and the packet do not (cf0021ae).
/// A pretty-printed receipt turns that greppable line into twelve.
#[test]
fn the_summary_stays_one_line() {
    let summary = summarize("oneline", WIDE_RECEIPT);
    assert_eq!(
        summary.lines().count(),
        1,
        "the receipt summary must be a single line: {summary}"
    );
}

/// A receipt gate.sh never managed to write must still produce a verdict
/// naming the head — the case where the run died before the receipt.
#[test]
fn an_unreadable_receipt_still_names_the_head() {
    let summary = summarize("unreadable", "not json at all");
    let got: serde_json::Value = serde_json::from_str(&summary)
        .unwrap_or_else(|e| panic!("the fallback must still be JSON ({e}): {summary}"));
    assert_eq!(
        got.get("verdict").and_then(|v| v.as_str()),
        Some("unreadable"),
        "an unreadable receipt is its own verdict, never a silent green: {summary}"
    );
    assert_eq!(
        got.get("head").and_then(|v| v.as_str()),
        Some("deadbeefdeadbeef"),
        "the fallback falls back to the head the runner knows: {summary}"
    );
}
