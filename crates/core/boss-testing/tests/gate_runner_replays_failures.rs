//! A failed gate must be explainable from the RECEIPT, and from
//! `kubectl logs` when the receipt is not enough.
//!
//! THE FAILURE THIS PINS. `gate.log` is written to `/gate-target`, a
//! volume only the gate container mounts. When that container exits the
//! last reader of the file is gone, so a red verdict left a receipt
//! naming WHICH check failed and no way at all to learn WHY. Three
//! branches were called red by the gate-runner and then passed those
//! same checks run by hand; no theory could be tested, because the
//! evidence was destroyed with the pod every time (backlog 9c7ed804).
//!
//! THE SECOND FAILURE, one level deeper (backlog 4a4d1227). Gate-run
//! 9dc722d2 recorded `verdict: failed` with a 62-entry `checks` array
//! correctly naming `test` as the single failure — and nothing at all
//! about WHICH test. The name was one grep away in the pod log, and the
//! pod log is reaped; the receipt is the durable record. So the same
//! extractor that replays the failed sections to stdout now also writes
//! the part a reader ACTS on — the failing test names and the panic line
//! with its `file:line` — into the receipt's `fails`, bounded, with
//! every reduction stated.
//!
//! ONE PARSER, TWO OUTPUTS. The replay and `fails` are the same fact
//! ("what did the failed checks say"), so they are read once from
//! gate.log rather than twice (CLAUDE.md §9a): the block writes the
//! replay text to a file the runner later `cat`s, and merges `fails`
//! into the receipt before it is reported.
//!
//! WHY THIS TEST RUNS THE EXTRACTOR INSTEAD OF READING IT. The sibling
//! test `build_image_retry.rs` asserts on the TEXT of a shell block, and
//! that is precisely how the `/tmp/kaniko.log` break shipped: the text
//! said "retry on a transient fault", every assertion passed, and the
//! script still died on the first line because the kaniko image has no
//! `/tmp`. A property you can only check by reading is a property you
//! are guessing at. So this test extracts the block from `run.sh`
//! exactly as it ships and EXECUTES it against crafted logs.
//!
//! Skips rather than fails when `python3` is absent, so a machine
//! without it does not manufacture a red.

use boss_testing::repo_root;
use serde_json::Value;
use std::io::Write;
use std::process::Command;

/// The failure-detail extractor, lifted out of `run.sh` verbatim.
///
/// It lives inline in the runner because the pod receives exactly one
/// file — the `gate-runner-script` ConfigMap is built with a single
/// `--from-file=run.sh=...`, so a second script would simply not be
/// there. Lifting it here is what makes it testable anyway.
fn extractor_source() -> String {
    let run_sh = repo_root().join("infra/gate-runner/run.sh");
    let src = std::fs::read_to_string(&run_sh)
        .unwrap_or_else(|e| panic!("reading {}: {e}", run_sh.display()));

    const OPEN: &str =
        "python3 - \"$RECEIPT\" /gate-target/gate.log /gate-target/failed-checks.txt <<'PY'";
    let start = src.find(OPEN).unwrap_or_else(|| {
        panic!(
            "no failure-detail heredoc in run.sh — if the runner stopped extracting failed \
             checks, a red gate is unexplainable again (neither the receipt nor the log names \
             the failing test) and this test is the thing that should have said so"
        )
    });
    let after_open = &src[start + OPEN.len()..];
    let body_start = after_open
        .find('\n')
        .expect("heredoc opener has no line ending")
        + 1;
    let body = &after_open[body_start..];
    let end = body
        .find("\nPY\n")
        .expect("failure-detail heredoc is not terminated by a PY marker");
    body[..end].to_string()
}

/// What one run of the extractor produced.
struct Extracted {
    /// The replay text it wrote for the runner to print.
    replay: String,
    /// The receipt file as it stands afterwards, raw.
    receipt: String,
    /// Whether the block exited 0.
    ok: bool,
    /// Anything it said on stdout (diagnostics only).
    stdout: String,
}

impl Extracted {
    /// `fails` as the receipt now carries it. `None` when the receipt is
    /// unparseable or the key is absent — the two cases this packet
    /// exists to tell apart from an empty list.
    fn fails(&self) -> Option<Vec<String>> {
        let v: Value = serde_json::from_str(&self.receipt).ok()?;
        Some(
            v.get("fails")?
                .as_array()?
                .iter()
                .map(|e| e.as_str().unwrap_or("<not a string>").to_string())
                .collect(),
        )
    }

    fn fails_joined(&self) -> String {
        self.fails().map(|f| f.join("\n")).unwrap_or_default()
    }
}

/// Run the extractor over a crafted receipt + log.
fn run_extractor(receipt: &str, log: &str) -> Extracted {
    let dir = std::env::temp_dir().join(format!(
        "gate-detail-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let script = dir.join("extract.py");
    let receipt_path = dir.join("receipt.json");
    let log_path = dir.join("gate.log");
    let replay_path = dir.join("failed-checks.txt");
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
        .arg(&replay_path)
        .output()
        .expect("python3 runs");
    let result = Extracted {
        replay: std::fs::read_to_string(&replay_path).unwrap_or_default(),
        receipt: std::fs::read_to_string(&receipt_path).unwrap_or_default(),
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
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

/// A real cargo test failure, in the shape cargo actually prints it —
/// copied from the run that filed backlog 4a4d1227.
const CARGO_LOG: &str = "\
::group::gate: test
running 7 tests
test sweeps::other_thing ... ok
test every_sweep_spawner_guards_on_its_own_subject ... FAILED

failures:

---- every_sweep_spawner_guards_on_its_own_subject stdout ----

thread 'every_sweep_spawner_guards_on_its_own_subject' panicked at crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5:
expected the seven daily sweep spawners, found 6 — if sweeps moved, move this pin with them
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    every_sweep_spawner_guards_on_its_own_subject

test result: FAILED. 6 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
::endgroup::
";

fn red_receipt(checks: &str) -> String {
    format!("{{\"verdict\":\"failed\",\"head\":\"abc\",\"mode\":\"full\",\"checks\":[{checks}]}}")
}

#[test]
fn it_replays_only_the_checks_that_failed() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = red_receipt(
        "{\"name\":\"fmt\",\"result\":\"pass\"},\
         {\"name\":\"clippy\",\"result\":\"fail\"},\
         {\"name\":\"test\",\"result\":\"fail\"}",
    );
    let got = run_extractor(&receipt, LOG);
    let out = &got.replay;

    assert!(
        got.ok,
        "extractor should succeed when it has failures to report: {}",
        got.stdout
    );
    assert!(
        out.contains("error: aborting due to 1 previous error"),
        "the failing check's actual error must reach the replay — that is the entire point:\n{out}"
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
/// A CHECK THAT COULD NOT REACH THE NETWORK JUDGED NOTHING (478347ad).
/// Gate-run 7522c115: `web-suite` failed with 232 lines of bun's
/// "error: Unable to connect. Is the computer able to access the url?"
/// and no test or compile failure; two minutes later the registry
/// answered in 0.06 s. The receipt recorded verdict=failed against the
/// branch, and its own `fails` line already said "no cargo test failure
/// in this check's output" over 232 connect errors — the diagnosis was
/// on the record and nothing acted on it. A check whose failure is only
/// connect/resolve errors, with no test, panic or compile failure, is a
/// REFUSAL: the receipt says `refused` with a `refused_because` naming
/// the check and the count, in the shape gate.sh writes for its own
/// disk-floor refusal, so every reader that spares the branch on a
/// refusal spares this one too.
#[test]
fn a_check_that_only_failed_to_reach_the_network_is_a_refusal_not_a_red() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let mut connect_errors = String::new();
    for _ in 0..40 {
        connect_errors.push_str(
            "error: Unable to connect. Is the computer able to access the url?\n\
             \n  https://registry.npmjs.org/svelte\n\n",
        );
    }
    let log = format!(
        "::group::gate: web install\nbun install v1.2.0\n{connect_errors}error: InstallFailed\n::endgroup::\n"
    );
    let receipt = red_receipt(
        "{\"name\":\"fmt\",\"result\":\"pass\"},\
         {\"name\":\"web install\",\"result\":\"fail\"}",
    );
    let got = run_extractor(&receipt, &log);
    assert!(got.ok, "{}", got.stdout);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(
        v["verdict"], "refused",
        "only connect errors and no judged failure: the run was refused, not the branch:\n{}",
        got.receipt
    );
    let why = v["refused_because"].as_str().unwrap_or("");
    assert!(
        why.contains("web install") && why.contains("network") && why.contains("40"),
        "refused_because names the check, the cause and the count: {why}"
    );
    assert!(
        got.fails_joined().contains("no cargo test failure"),
        "the fails ladder still says what it saw: {}",
        got.fails_joined()
    );
}

/// The same connect errors BESIDE a judged failure are noise around a
/// red, not a refusal: a test that failed is a verdict on the branch.
#[test]
fn connect_errors_beside_a_judged_failure_stay_a_red() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let log = format!(
        "::group::gate: test\nerror: Unable to connect. Is the computer able to access the url?\n\
         error: Unable to connect. Is the computer able to access the url?\n{}",
        LOG.split("::group::gate: test\n").nth(1).unwrap_or("")
    );
    let receipt = red_receipt("{\"name\":\"test\",\"result\":\"fail\"}");
    let got = run_extractor(&receipt, &log);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(v["verdict"], "failed", "{}", got.receipt);
    assert!(v.get("refused_because").is_none(), "{}", got.receipt);
}

#[test]
fn it_keeps_what_a_killed_check_managed_to_say() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(&red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"), LOG);

    assert!(got.ok);
    assert!(
        got.replay.contains("test boss::thing ... FAILED"),
        "an unterminated ::group:: (the check was killed) must still be replayed:\n{}",
        got.replay
    );
}

/// "No receipt" and "no failed check" are the cases where the extractor
/// cannot explain anything, and both used to exit non-zero so run.sh's
/// `|| tail -200` fired. The fallback now lives INSIDE the one parser —
/// it writes the raw tail into the replay itself — because a reduction
/// decided by a shell `||` chain is a reduction nobody can see.
#[test]
fn it_falls_back_to_the_raw_tail_when_it_cannot_explain_anything() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }

    let got = run_extractor("{ this is not json", LOG);
    assert!(
        got.replay.contains("formatting is fine"),
        "an unreadable receipt must still leave the reader the raw log tail:\n{}",
        got.replay
    );
    assert_eq!(
        got.receipt, "{ this is not json",
        "an unreadable receipt is evidence too — it must not be rewritten or truncated"
    );

    let all_passed = red_receipt("{\"name\":\"fmt\",\"result\":\"pass\"}");
    let got = run_extractor(&all_passed, LOG);
    assert!(
        got.replay.contains("outside a check"),
        "a failed verdict with no failed check means the run died OUTSIDE a check (headroom \
         guard, crash before the receipt) — it must say which case it hit:\n{}",
        got.replay
    );
    assert!(
        got.replay.contains("formatting is fine"),
        "...and still hand over the raw tail:\n{}",
        got.replay
    );
    assert!(
        got.fails_joined().contains("outside a check"),
        "the receipt is the durable record, so it must carry that finding too, not just the \
         pod log:\n{}",
        got.fails_joined()
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
    let got = run_extractor(&red_receipt("{\"name\":\"web\",\"result\":\"fail\"}"), LOG);

    assert!(got.ok);
    assert!(
        got.replay.contains("no ::group:: block"),
        "an absent block must be reported, not rendered as an empty section:\n{}",
        got.replay
    );
    assert!(
        got.fails_joined().contains("no ::group:: block"),
        "and the receipt must say so as well — it is the copy that outlives the pod:\n{}",
        got.fails_joined()
    );
}

// ---------------------------------------------------------------------
// backlog 4a4d1227 — the receipt names the failing TEST
// ---------------------------------------------------------------------

/// `fails` was `null` on the red receipt that filed this packet, and
/// `[]` on the green ones beside it — so a reader could not tell "no
/// failures" from "nobody wrote the field". The field is now always
/// present, and empty means empty.
#[test]
fn a_green_receipt_carries_an_empty_fails_not_a_missing_one() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let green = "{\"verdict\":\"green\",\"head\":\"abc\",\"mode\":\"full\",\
                 \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\"}]}";
    let got = run_extractor(green, "::group::gate: fmt\nfine\n::endgroup::\n");

    assert!(got.ok, "green must not be an error path: {}", got.stdout);
    assert_eq!(
        got.fails().as_deref(),
        Some(&[][..]),
        "a green receipt must carry `fails: []` — present and empty. `null` on one verdict and \
         `[]` on the other is the asymmetry this packet is about:\n{}",
        got.receipt
    );
}

/// The whole point: the name a reader acts on, in the durable record.
#[test]
fn a_red_receipt_names_the_failing_test_and_where_it_panicked() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG,
    );
    let fails = got.fails_joined();

    assert!(got.ok, "extractor failed: {}", got.stdout);
    assert!(
        fails.contains("every_sweep_spawner_guards_on_its_own_subject"),
        "the failing TEST name is what the reader acts on; the check name only gets them to \
         `test`:\n{fails}"
    );
    assert!(
        fails.contains("crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5"),
        "the panic's file:line is the other half — without it the name still has to be \
         located:\n{fails}"
    );
    assert!(
        fails.contains("found 6"),
        "the panic MESSAGE says what was actually wrong:\n{fails}"
    );
    assert!(
        fails.contains("test"),
        "the failing check stays named, so `fails` reads on its own:\n{fails}"
    );
}

/// Every field the receipt already carried keeps its place and meaning —
/// 62 named checks with per-check seconds is what made the last
/// diagnosis a six-minute read. This change is additive.
#[test]
fn it_leaves_every_other_receipt_field_alone() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    const RICH: &str = r#"{"verdict":"failed","mode":"full","scope":"","head":"68ef2957",
        "dirty":false,"host":"gate-x","ci":true,"free_gb":61,"unverifiable":[],
        "checks":[{"name":"fmt","result":"pass","seconds":3},
                  {"name":"test","result":"fail","seconds":96}]}"#;
    let got = run_extractor(RICH, CARGO_LOG);

    let before: Value = serde_json::from_str(RICH).expect("fixture parses");
    let after: Value = serde_json::from_str(&got.receipt).expect("receipt still parses");
    for (k, v) in before.as_object().expect("object") {
        assert_eq!(
            after.get(k),
            Some(v),
            "field `{k}` must survive the merge untouched:\n{}",
            got.receipt
        );
    }
    assert!(
        after.get("fails").is_some(),
        "…and `fails` is the only addition:\n{}",
        got.receipt
    );
}

/// The bound is deliberate and STATED. A suite failing two hundred tests
/// must not write a receipt nobody can read — and must not quietly drop
/// the rest either, which is the `docker build -q` / last-16-KB defect
/// class (CLAUDE.md §Diagnosis).
#[test]
fn it_caps_the_named_tests_and_says_how_many_it_left_out() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let mut log = String::from("::group::gate: test\nrunning 200 tests\n");
    for i in 0..40 {
        log.push_str(&format!("test suite::case_{i} ... FAILED\n"));
    }
    log.push_str("\nfailures:\n\n");
    for i in 0..40 {
        log.push_str(&format!("    suite::case_{i}\n"));
    }
    log.push_str("test result: FAILED. 160 passed; 40 failed\n::endgroup::\n");

    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        &log,
    );
    let fails = got.fails().expect("fails is present");

    assert!(
        fails.len() <= 12,
        "the receipt must stay readable — {} entries is not a receipt:\n{fails:#?}",
        fails.len()
    );
    let joined = fails.join("\n");
    assert!(
        joined.contains("40"),
        "the COUNT of what was left out has to be in the record, or the reduction is silent — \
         the exact defect the 778 KB log truncated to 16 KB taught us:\n{joined}"
    );
    assert!(
        fails.iter().all(|e| e.len() <= 600),
        "no single entry may be unbounded either:\n{joined}"
    );
}

/// Not every check is cargo-shaped. A lint, `svelte-check`, a shell
/// script — the extractor must hand over what the check actually said
/// and SAY that it could not parse it, rather than inventing a test
/// name it never saw.
#[test]
fn a_non_cargo_check_gets_its_own_words_not_an_invented_test_name() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    const SVELTE: &str = "\
::group::gate: web-svelte-check
svelte-check found 1 error
Error: src/it/yard/yard.svelte:12:3 Type 'string' is not assignable to 'number'
::endgroup::
";
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-svelte-check\",\"result\":\"fail\"}"),
        SVELTE,
    );
    let fails = got.fails_joined();

    assert!(got.ok, "extractor failed: {}", got.stdout);
    assert!(
        fails.contains("yard.svelte:12:3"),
        "a non-cargo check's own error line is still what the reader acts on:\n{fails}"
    );
    assert!(
        !fails.contains("panicked"),
        "and nothing may be claimed about a shape that was never there:\n{fails}"
    );
}
