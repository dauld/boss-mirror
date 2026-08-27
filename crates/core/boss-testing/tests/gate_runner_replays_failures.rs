//! A failed gate must be explainable from `kubectl logs` alone.
//!
//! THE FAILURE THIS PINS. `gate.log` is written to `/gate-target`, a
//! volume only the gate container mounts. When that container exits the
//! last reader of the file is gone, so a red verdict left a receipt
//! naming WHICH check failed and no way at all to learn WHY. Three
//! branches were called red by the gate-runner and then passed those
//! same checks run by hand; no theory could be tested, because the
//! evidence was destroyed with the pod every time (backlog 9c7ed804).
//!
//! WHY THIS TEST RUNS THE EXTRACTOR INSTEAD OF READING IT. The sibling
//! test `build_image_retry.rs` asserts on the TEXT of a shell block, and
//! that is precisely how the `/tmp/kaniko.log` break shipped: the text
//! said "retry on a transient fault", every assertion passed, and the
//! script still died on the first line because the kaniko image has no
//! `/tmp`. A property you can only check by reading is a property you
//! are guessing at. So this test extracts the replay block from
//! `run.sh` exactly as it ships and EXECUTES it against crafted logs.
//!
//! Skips rather than fails when `python3` is absent, so a machine
//! without it does not manufacture a red.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

/// The replay extractor, lifted out of `run.sh` verbatim.
///
/// It lives inline in the runner because the pod receives exactly one
/// file — the `gate-runner-script` ConfigMap is built with a single
/// `--from-file=run.sh=...`, so a second script would simply not be
/// there. Lifting it here is what makes it testable anyway.
fn extractor_source() -> String {
    let run_sh = repo_root().join("infra/gate-runner/run.sh");
    let src = std::fs::read_to_string(&run_sh)
        .unwrap_or_else(|e| panic!("reading {}: {e}", run_sh.display()));

    const OPEN: &str = "python3 - \"$RECEIPT\" /gate-target/gate.log <<'PY'";
    let start = src.find(OPEN).unwrap_or_else(|| {
        panic!(
            "no replay heredoc in run.sh — if the runner stopped replaying failed checks, a red \
             gate is unexplainable again and this test is the thing that should have said so"
        )
    });
    // The heredoc opener carries the shell fallback on the same line
    // (`<<'PY' || tail -200 ... || true`), so the script body starts at
    // the NEXT line. Including that tail would feed shell to python.
    let after_open = &src[start + OPEN.len()..];
    let body_start = after_open
        .find('\n')
        .expect("heredoc opener has no line ending")
        + 1;
    let body = &after_open[body_start..];
    let end = body
        .find("\nPY\n")
        .expect("replay heredoc is not terminated by a PY marker");
    body[..end].to_string()
}

/// Run the extractor over a crafted receipt + log. Returns (stdout, ok).
fn run_extractor(receipt: &str, log: &str) -> (String, bool) {
    let dir = std::env::temp_dir().join(format!(
        "gate-replay-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let script = dir.join("extract.py");
    let receipt_path = dir.join("receipt.json");
    let log_path = dir.join("gate.log");
    for (path, body) in [
        (&script, extractor_source()),
        (&receipt_path, receipt.to_string()),
        (&log_path, log.to_string()),
    ] {
        let mut fh = std::fs::File::create(path).expect("write scratch file");
        fh.write_all(body.as_bytes()).expect("write scratch file");
    }

    let out = Command::new("python3")
        .arg(&script)
        .arg(&receipt_path)
        .arg(&log_path)
        .output()
        .expect("python3 runs");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn python3_missing() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
}

const LOG: &str = "\
::group::gate: fmt
formatting is fine
::endgroup::
::group::gate: clippy
warning: unused variable `x`
error: aborting due to 1 previous error
::endgroup::
::group::gate: test
running 40 tests
test boss::thing ... FAILED
";

#[test]
fn it_replays_only_the_checks_that_failed() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = r#"{"verdict":"failed","head":"abc","checks":[
        {"name":"fmt","result":"pass"},
        {"name":"clippy","result":"fail"},
        {"name":"test","result":"fail"}]}"#;
    let (out, ok) = run_extractor(receipt, LOG);

    assert!(
        ok,
        "extractor should succeed when it has failures to report"
    );
    assert!(
        out.contains("error: aborting due to 1 previous error"),
        "the failing check's actual error must reach stdout — that is the entire point:\n{out}"
    );
    assert!(
        out.contains("test boss::thing ... FAILED"),
        "every failed check is replayed, not just the first:\n{out}"
    );
    assert!(
        !out.contains("formatting is fine"),
        "a PASSING check's output must not be replayed. A full gate.log is mostly successful \
         build chatter, and burying the three lines that matter is the same defect as \
         printing nothing:\n{out}"
    );
}

/// An unterminated group means the check was still running when the log
/// ended — a timeout, an OOM kill, a node reset mid-gate. That is one of
/// the cases most worth explaining, so it must not be dropped for want
/// of a closing marker.
#[test]
fn it_keeps_what_a_killed_check_managed_to_say() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = r#"{"verdict":"failed","head":"abc","checks":[{"name":"test","result":"fail"}]}"#;
    let (out, ok) = run_extractor(receipt, LOG);

    assert!(ok);
    assert!(
        out.contains("test boss::thing ... FAILED"),
        "an unterminated ::group:: (the check was killed) must still be replayed:\n{out}"
    );
}

/// The runner falls back to a raw `tail` when the extractor exits
/// non-zero, so "no receipt" and "no failed check" MUST be non-zero. If
/// they silently exited 0 the reader would get no output and no tail —
/// the original defect, reintroduced.
#[test]
fn it_defers_to_the_raw_tail_when_it_cannot_explain_anything() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }

    let (_, ok) = run_extractor("{ this is not json", LOG);
    assert!(
        !ok,
        "an unreadable receipt must exit non-zero so run.sh's `|| tail` fallback fires"
    );

    let all_passed =
        r#"{"verdict":"failed","head":"abc","checks":[{"name":"fmt","result":"pass"}]}"#;
    let (out, ok) = run_extractor(all_passed, LOG);
    assert!(
        !ok,
        "a failed verdict with no failed check means the run died OUTSIDE a check (headroom \
         guard, crash before the receipt) — it must fall through to the tail, not report success"
    );
    assert!(
        out.contains("died outside a check"),
        "and it should say which case it hit:\n{out}"
    );
}

/// A check named in the receipt but absent from the log is the signal
/// that gate.sh changed its grouping. Saying so is what stops this
/// extractor from rotting into silence.
#[test]
fn it_says_so_when_a_failed_check_has_no_block() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = r#"{"verdict":"failed","head":"abc","checks":[{"name":"web","result":"fail"}]}"#;
    let (out, ok) = run_extractor(receipt, LOG);

    assert!(ok);
    assert!(
        out.contains("no ::group:: block"),
        "an absent block must be reported, not rendered as an empty section:\n{out}"
    );
}
