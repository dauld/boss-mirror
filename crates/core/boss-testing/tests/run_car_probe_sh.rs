//! `infra/forge/run-car-probe.sh` runs a car's recorded probe on the
//! FORGE HOST. The probe was written on the dev pod, which is a
//! different machine with different tools — so a probe can be correct
//! and unrunnable, and this test pins the mechanism that tells those
//! two apart.
//!
//! THE DEFECT (backlog f9304366, measured 2026-09-09). The auto-proof
//! loop's first live run: a train arrived, the rule filed one
//! ops-request per probed car, the forge's ops-runner answered both,
//! and NEITHER car proved. Both probes were
//! `kubectl -n boss-dev exec deploy/boss-conductor -- …`, which is
//! right from the pod and impossible from the forge (outside the
//! cluster, no kubeconfig). The recorded evidence was
//! `{"exit": 1, "output": ""}` — an exit code with empty streams, which
//! reads exactly like a false claim.
//!
//! The reason the streams were empty is the whole lesson: the probes
//! swallowed their own diagnostics. `kubectl … 2>&1 | grep -q …` sends
//! the shell's own `kubectl: command not found` INTO the pipe, where
//! grep eats it. Nothing run-car-probe.sh does with stderr can recover
//! a message the probe redirected away from it.
//!
//! So the script hands the probe's shell a `command_not_found_handle`
//! and a file descriptor the probe's redirections cannot reach (fd 9,
//! opened before the probe's own text runs). Every command bash could
//! not find is recorded there, and the attempt says `unrunnable` with
//! the tool named — CLAUDE.md §Diagnosis, a verdict must name what
//! failed.
//!
//! This test RUNS that prelude, extracted verbatim from the script, so
//! the mechanism cannot rot into a comment. It needs bash and nothing
//! else — the script's own jq/curl path is covered by the record-shape
//! pin in boss-cli's prove.rs.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

const BEGIN: &str = "# PROBE-PRELUDE-BEGIN";
const END: &str = "# PROBE-PRELUDE-END";

/// The prelude the script wraps every probe in, lifted from the script
/// itself between its two markers. One definition, executed here.
fn probe_prelude() -> String {
    let sh = std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable");
    let after = sh
        .split_once(BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {BEGIN} marker"))
        .1;
    let body = after
        .split_once(END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {END} marker"))
        .0;
    assert!(
        body.contains("command_not_found_handle"),
        "the extracted prelude does not install a command_not_found_handle:\n{body}"
    );
    body.to_string()
}

fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("run-car-probe-sh-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Run one probe exactly as the script runs it: the prelude, then the
/// probe's own text, in one `bash -c`, with the not-found file named in
/// the environment. Returns (exit code, stdout, stderr, not-found log).
fn run_probe(case: &str, probe: &str) -> (i32, String, String, String) {
    let dir = scratch(case);
    let notfound = dir.join("notfound");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}{probe}", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", &notfound)
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&notfound).unwrap_or_default(),
    )
}

/// THE MEASURED CASE, verbatim in shape: a probe that pipes its own
/// stderr into a grep. The tool is missing, the streams the script
/// captures are empty — and the not-found channel still names it.
#[test]
fn a_missing_tool_is_named_even_when_the_probe_swallows_its_own_stderr() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "swallowed",
        "kubectl-no-such-tool -n boss-dev get pods 2>&1 | grep -q Running \
         && echo PROBE_OK || echo PROBE_MISSING",
    );
    // What the script would have recorded before: exit + streams that
    // say nothing about why.
    assert_eq!(rc, 0, "the probe's own `|| echo` masks the exit code");
    assert!(stdout.contains("PROBE_MISSING"), "stdout: {stdout}");
    assert!(
        !stderr.contains("kubectl-no-such-tool"),
        "the probe redirected its diagnostics into the pipe; stderr: {stderr}"
    );
    // What it records now: the tool, on a channel the probe cannot
    // redirect.
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// The other live shape: an `&&` chain whose first link is missing, so
/// the probe exits 1 with nothing on either stream. Exit 1 and silence
/// is indistinguishable from a false claim — unless the tool is named.
#[test]
fn an_and_chain_that_dies_on_a_missing_tool_still_names_it() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "and-chain",
        "kubectl-no-such-tool get cm 2>&1 | grep -q x && echo PROBE_OK",
    );
    assert_eq!(rc, 1, "the chain stops at the missing link");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// A probe that does NOT redirect keeps bash's own message on stderr —
/// the prelude adds a channel, it does not take one away.
#[test]
fn the_prelude_still_prints_command_not_found_on_stderr() {
    let (_, _, stderr, notfound) = run_probe("plain", "kubectl-no-such-tool version");
    assert!(
        stderr.contains("kubectl-no-such-tool") && stderr.contains("command not found"),
        "stderr must still carry the shell's message, got: {stderr:?}"
    );
    assert!(notfound.contains("kubectl-no-such-tool"), "{notfound:?}");
}

/// A probe whose tools all exist records NOTHING on the not-found
/// channel — so `unrunnable` cannot be stamped on a claim that simply
/// turned out to be false.
#[test]
fn a_runnable_probe_leaves_the_not_found_channel_empty() {
    let (rc, stdout, _, notfound) = run_probe("runnable", "echo NOT_THE_WORD | grep -q TOKEN");
    assert_eq!(rc, 1, "the probe ran and its assertion failed");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        notfound.trim().is_empty(),
        "a false claim must not look unrunnable, got: {notfound:?}"
    );
}

/// The prelude must not be able to take the probe down with it: with
/// no writable not-found file the probe still runs and still reports.
#[test]
fn an_unwritable_not_found_channel_does_not_break_the_probe() {
    let dir = scratch("unwritable");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}echo PROBE_TOKEN_OK", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", "/proc/nonexistent/dir/notfound")
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "the probe must still run");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("PROBE_TOKEN_OK"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
