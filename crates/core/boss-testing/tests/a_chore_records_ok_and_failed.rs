//! An in-cluster chore records its verdict on BOTH legs, through ONE
//! wrapper (backlog 480e183c, 2026-09-18).
//!
//! MEASURED. Ten CronJobs live under infra/cluster/manifests/. Eight of
//! them are single-container boss-image chores, and every one carried
//! its own copy of the same three lines under `set -euo pipefail`:
//!
//!     boss-maintenance-wrap.sh <kind> "<title>"
//!     <the check>
//!     boss-step.sh <kind> run result=ok
//!
//! A check that exits nonzero ends the script at line two, so line
//! three never runs and the packet stays OPEN, looking exactly like a
//! run in progress. For a sweep whose purpose is to surface a violated
//! invariant, an open packet is silence. This is the defect
//! boss-maintenance-wrap.sh's own header documents for the bare-metal
//! units and fixed there on 2026-09-05 by moving the close to
//! ExecStopPost — and the CronJobs that replaced those units never took
//! the failure leg with them. A CronJob has no ExecStopPost; the
//! wrapper `infra/boss-chore.sh` is its equivalent: open the packet
//! (best-effort), run the check capturing its output, record `ok` or
//! `failed` with the exit status and an excerpt of the output, and exit
//! with the check's status so the Kubernetes Job still shows Failed.
//!
//! The other two CronJobs are out of this scope by a measurable shape,
//! not by name: boss-backup is a multi-image Pod (postgres dump,
//! cloud-sdk upload, boss-image open and close in SEPARATE containers)
//! whose verdict is the Pod's own structure — backup_files_its_packet.rs
//! pins that a failed leg leaves its packet open by design; and
//! boss-estate-observe runs alpine/k8s and files no packet at all (the
//! observation IS its record, timers-leave-a-packet's exemption).
//!
//! WHAT THIS PINS:
//!   * the wrapper's contract, executed against planted stubs (the real
//!     helpers need jq and a jobs API): a passing check records
//!     `result=ok`, a failing one records `result=failed` with
//!     `exit_status=<rc>` and the output it printed, the chore exits
//!     with the check's status either way, an unreachable wrap does not
//!     stop the check, an unreachable close does not fail a check that
//!     passed, and a long output rides as head + tail with the omitted
//!     count named;
//!   * the ratchet: every single-container boss-image CronJob invokes
//!     boss-chore.sh with a `maintenance-` kind and NONE invokes
//!     boss-step.sh or boss-maintenance-wrap.sh directly — the ten
//!     copies of the dance are gone and cannot come back;
//!   * the image COPYs the wrapper to the path the manifests run — the
//!     gate never builds the image, so this is the only reader.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHORE: &str = "infra/boss-chore.sh";
const MANIFESTS: &str = "infra/cluster/manifests";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const BOSS_IMAGE: &str = "10.20.0.15:3000/david/boss:";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A scratch bin dir holding the wrapper under test beside two stubs
/// that record every call. The wrapper resolves its helpers
/// next-to-self (the way the image lays them out in /usr/local/bin),
/// so copying it in is what makes the stubs the ones it finds.
///
/// `BOSS_TEST_WRAP_RC` / `BOSS_TEST_STEP_RC` make a stub fail, so the
/// best-effort legs can be exercised.
fn planted(name: &str) -> PathBuf {
    let bin = scratch_dir(name);
    let chore = read(CHORE);
    write_exec(&bin.join("boss-chore.sh"), &chore);
    write_exec(
        &bin.join("boss-maintenance-wrap.sh"),
        "#!/usr/bin/env bash\n\
         printf 'open:%s\\n' \"$*\" >> \"$STUB_LOG\"\n\
         rc=${BOSS_TEST_WRAP_RC:-0}\n\
         [ \"$rc\" = 0 ] || echo 'boss-maintenance-wrap: jobs-api unreachable (stub)' >&2\n\
         exit \"$rc\"\n",
    );
    write_exec(
        &bin.join("boss-step.sh"),
        // One line per call; the output= pair can span lines, so its
        // newlines become record separators (0x1e) and the pairs are
        // joined by unit separators (0x1f) before the line is logged.
        "#!/usr/bin/env bash\n\
         { printf 'close:'; for a in \"$@\"; do printf '%s' \"$a\" | tr '\\n' '\\036'; printf '\\037'; done; echo; } >> \"$STUB_LOG\"\n\
         rc=${BOSS_TEST_STEP_RC:-0}\n\
         [ \"$rc\" = 0 ] || echo 'boss-step: jobs-api unreachable (stub)' >&2\n\
         exit \"$rc\"\n",
    );
    bin
}

struct Run {
    rc: i32,
    stdout: String,
    stderr: String,
    /// `open:` and `close:` lines, in the order the stubs were called.
    log: Vec<String>,
}

impl Run {
    fn opens(&self) -> Vec<&str> {
        self.log
            .iter()
            .filter_map(|l| l.strip_prefix("open:"))
            .collect()
    }
    /// The argv of every boss-step call, one Vec per call.
    fn closes(&self) -> Vec<Vec<String>> {
        self.log
            .iter()
            .filter_map(|l| l.strip_prefix("close:"))
            .map(|l| {
                l.split('\u{1f}')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.replace('\u{1e}', "\n"))
                    .collect()
            })
            .collect()
    }
    fn pair<'a>(close: &'a [String], key: &str) -> Option<&'a str> {
        close
            .iter()
            .find_map(|p| p.strip_prefix(&format!("{key}=")))
    }
}

fn run_chore(bin: &Path, env: &[(&str, &str)], args: &[&str]) -> Run {
    let log = bin.join("stub.log");
    let _ = std::fs::remove_file(&log);
    let mut cmd = Command::new("bash");
    cmd.arg(bin.join("boss-chore.sh"))
        .args(args)
        .env("STUB_LOG", &log)
        .env("BOSS_JOBS_URL", "http://boss-jobs-internal.test:7900")
        .env("TMPDIR", bin)
        .env_remove("BOSS_TEST_WRAP_RC")
        .env_remove("BOSS_TEST_STEP_RC");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the wrapper");
    Run {
        rc: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect(),
    }
}

// --- the wrapper's contract --------------------------------------------------

#[test]
fn a_passing_check_records_ok_and_the_chore_exits_zero() {
    let bin = planted("chore-ok");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "echo clean; echo checked 8",
        ],
    );
    assert_eq!(
        r.rc, 0,
        "the chore exits with the check's status:\n{}",
        r.stderr
    );
    assert_eq!(
        r.opens(),
        ["maintenance-selftest Self test"],
        "the packet opens through boss-maintenance-wrap.sh, kind then title"
    );
    let closes = r.closes();
    assert_eq!(closes.len(), 1, "exactly one record: {closes:?}");
    let close = &closes[0];
    assert_eq!(&close[..2], ["maintenance-selftest", "run"]);
    assert_eq!(Run::pair(close, "result"), Some("ok"));
    assert_eq!(Run::pair(close, "exit_status"), Some("0"));
    let output = Run::pair(close, "output").expect("the ok leg carries what the run said too");
    assert!(
        output.contains("clean") && output.contains("checked 8"),
        "output= is the check's own words, copied not retyped: {output:?}"
    );
    assert!(
        r.stdout.contains("clean\nchecked 8\n"),
        "the check's output still reaches the container log:\n{}",
        r.stdout
    );
    assert!(
        r.log[0].starts_with("open:") && r.log[1].starts_with("close:"),
        "open, then the check, then the record: {:?}",
        r.log
    );
}

#[test]
fn a_failing_check_records_failed_with_its_exit_status_and_output() {
    let bin = planted("chore-failed");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "echo 'invariant B: 3 violators'; echo 'FAILED (1 of 8)' >&2; exit 3",
        ],
    );
    assert_eq!(
        r.rc, 3,
        "the chore exits with the check's status so the Kubernetes Job shows Failed:\n{}",
        r.stderr
    );
    let closes = r.closes();
    assert_eq!(
        closes.len(),
        1,
        "the FAILURE LEG RECORDS — this is the whole defect: {closes:?}\n{}",
        r.stderr
    );
    let close = &closes[0];
    assert_eq!(Run::pair(close, "result"), Some("failed"));
    assert_eq!(Run::pair(close, "exit_status"), Some("3"));
    let output = Run::pair(close, "output").unwrap();
    assert!(
        output.contains("invariant B: 3 violators") && output.contains("FAILED (1 of 8)"),
        "stdout AND stderr of the check ride the packet: {output:?}"
    );
    // The check's two streams are one capture, so its stderr reaches the
    // container log on the chore's stdout — one stream, in order, the
    // way `kubectl logs` shows it.
    assert!(
        r.stdout
            .contains("invariant B: 3 violators\nFAILED (1 of 8)\n"),
        "and the container log still has it, in order:\n{}",
        r.stdout
    );
}

#[test]
fn a_check_that_cannot_start_is_a_failed_run_too() {
    let bin = planted("chore-127");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "/no/such/binary",
            "--flag",
        ],
    );
    assert_eq!(r.rc, 127, "{}", r.stderr);
    let closes = r.closes();
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert_eq!(Run::pair(&closes[0], "result"), Some("failed"));
    assert_eq!(Run::pair(&closes[0], "exit_status"), Some("127"));
}

#[test]
fn an_unreachable_wrap_does_not_stop_the_check() {
    let bin = planted("chore-wrap-dark");
    let r = run_chore(
        &bin,
        &[("BOSS_TEST_WRAP_RC", "78")],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "echo ran; exit 0",
        ],
    );
    assert_eq!(
        r.rc, 0,
        "the packet is visibility, never a precondition (CLAUDE.md §Diagnosis):\n{}",
        r.stderr
    );
    assert!(r.stdout.contains("ran\n"), "the check ran:\n{}", r.stdout);
    assert_eq!(
        r.closes().len(),
        1,
        "the close is still attempted: {:?}",
        r.log
    );
    assert!(
        r.stderr.contains("boss-chore") && r.stderr.contains("did not open"),
        "the lost visibility is said out loud, naming the wrapper:\n{}",
        r.stderr
    );
}

#[test]
fn an_unreachable_close_does_not_fail_a_check_that_passed() {
    let bin = planted("chore-close-dark");
    let r = run_chore(
        &bin,
        &[("BOSS_TEST_STEP_RC", "1")],
        &["maintenance-selftest", "Self test", "--", "true"],
    );
    assert_eq!(
        r.rc, 0,
        "a lost HTTP call must not report a good run as failed and spend backoffLimit \
         re-running it:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("did not record"),
        "and the lost record is said out loud:\n{}",
        r.stderr
    );
    // The reverse: a failed check with a dark close still exits with
    // the check's status — the Job shows Failed even when the packet
    // could not be told.
    let r = run_chore(
        &bin,
        &[("BOSS_TEST_STEP_RC", "1")],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "exit 4",
        ],
    );
    assert_eq!(r.rc, 4, "{}", r.stderr);
}

#[test]
fn a_long_output_rides_as_head_and_tail_with_the_omission_named() {
    let bin = planted("chore-long");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "i=1; while [ $i -le 200 ]; do echo \"line $i\"; i=$((i+1)); done; exit 1",
        ],
    );
    assert_eq!(r.rc, 1);
    let closes = r.closes();
    let output = Run::pair(&closes[0], "output").unwrap();
    assert!(
        output.starts_with("line 1\n"),
        "the head is kept — the first lines say what the run was doing:\n{output}"
    );
    assert!(
        output.trim_end().ends_with("line 200"),
        "the tail is kept — the last lines say how it ended:\n{output}"
    );
    assert!(
        !output.contains("\nline 100\n"),
        "the middle is not carried — a step's metadata is a record, not a log:\n{output}"
    );
    assert!(
        output.contains("140 non-verdict lines omitted"),
        "and the excerpt says how much it left out, so nobody mistakes it for the whole:\n{output}"
    );
    assert!(
        r.stdout.contains("\nline 100\n"),
        "the WHOLE output still went to the container log — capture whole, excerpt the record:\n{}",
        r.stdout.len()
    );
}

/// THE WINDOW ATE THE VERDICT (backlog 11395970, left by the builder of
/// ac3270c7). The nightly playground crawl prints one
/// `RED <route> <kind>: <error>` line per red surface, and the rule
/// file-backlog-items-on-playground-crawl-red files one item per RED
/// route it reads off the run step. The step held head-20 + tail-40 of
/// the capture, so a night with more than ~38 reds lost RED lines to
/// the marker BEFORE the record was stored - the reduction CLAUDE.md
/// §Diagnosis names, paid by the judge that reads the record. So a
/// verdict-shaped line (`RED `, `GREEN `, `verdict: `, the crawl's own
/// roll-up and its console.error lines) survives the window wherever
/// it sits, the head/tail reduction applies to the rest, and the marker
/// says how many NON-verdict lines it dropped. Sixty reds beneath a
/// hundred lines of crawl chatter, with thirty more after them: every
/// one reaches the step, in order.
#[test]
fn every_verdict_line_reaches_the_step_and_only_the_rest_is_reduced() {
    let bin = planted("chore-verdicts");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-playground-crawl",
            "Playground crawl",
            "--",
            "sh",
            "-c",
            "i=1; while [ $i -le 100 ]; do echo \"[crawl 1] /route-$i 120ms\"; i=$((i+1)); done; \
             echo 'crawled 56 routes at http://gw.test: 60 red, 1 console.error, 1 expected console.error'; \
             echo '  console.error [/ux/views] TypeError: x is not a function'; \
             echo '  expected console.error [/it/design] Failed to load resource: 403 — a guest may not read it'; \
             i=1; while [ $i -le 60 ]; do echo \"RED /it/route-$i pageerror: TypeError: cannot read $i\"; i=$((i+1)); done; \
             i=1; while [ $i -le 30 ]; do echo \"teardown $i\"; i=$((i+1)); done; exit 1",
        ],
    );
    assert_eq!(r.rc, 1, "{}", r.stderr);
    let closes = r.closes();
    let output = Run::pair(&closes[0], "output").unwrap();
    let reds: Vec<&str> = output.lines().filter(|l| l.starts_with("RED ")).collect();
    assert_eq!(
        reds.len(),
        60,
        "every RED line reaches the step - the judge files from this record and nothing \
         else:\n{output}"
    );
    for i in 1..=60 {
        assert!(
            reds.contains(
                &format!("RED /it/route-{i} pageerror: TypeError: cannot read {i}").as_str()
            ),
            "RED line {i} is on the record, copied not retyped:\n{output}"
        );
    }
    for needle in [
        "crawled 56 routes at http://gw.test: 60 red",
        "  console.error [/ux/views] TypeError: x is not a function",
        "  expected console.error [/it/design] Failed to load resource: 403",
    ] {
        assert!(
            output.contains(needle),
            "the crawl's roll-up and its console.error lines are verdict-shaped too:\n{output}"
        );
    }
    assert!(
        output.starts_with("[crawl 1] /route-1 120ms\n") && output.contains("\nteardown 30\n"),
        "the rest still rides as head and tail:\n{output}"
    );
    assert!(
        !output.contains("[crawl 1] /route-50 120ms")
            && !output.contains("[crawl 1] /route-80 120ms"),
        "and only the rest is reduced - the middle of the chatter is not carried:\n{output}"
    );
    // 100 chatter + 30 teardown = 130 non-verdict lines; 20 + 40 kept,
    // so the tail is chatter 91-100 and the teardown.
    assert!(
        output.contains("70 non-verdict lines omitted"),
        "the marker counts what it dropped, and says none of it was a verdict:\n{output}"
    );
    let order: Vec<usize> = [
        "[crawl 1] /route-1 ",
        "non-verdict lines omitted",
        "RED /it/route-1 ",
        "RED /it/route-60 ",
        "teardown 30",
    ]
    .iter()
    .map(|n| {
        output
            .find(n)
            .unwrap_or_else(|| panic!("{n} missing:\n{output}"))
    })
    .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "the record keeps the capture's order - head, the marker, the verdicts, the tail:\n{output}"
    );
}

#[test]
fn a_short_output_is_carried_whole() {
    let bin = planted("chore-short");
    let r = run_chore(
        &bin,
        &[],
        &[
            "maintenance-selftest",
            "Self test",
            "--",
            "sh",
            "-c",
            "echo a; echo b; echo c",
        ],
    );
    let closes = r.closes();
    assert_eq!(Run::pair(&closes[0], "output"), Some("a\nb\nc\n"));
}

#[test]
fn a_bad_invocation_is_refused_before_anything_opens() {
    let bin = planted("chore-usage");
    for args in [
        vec!["maintenance-selftest"],
        vec!["maintenance-selftest", "Self test"],
        vec!["maintenance-selftest", "Self test", "--"],
        vec!["maintenance-selftest", "Self test", "true"],
    ] {
        let r = run_chore(&bin, &[], &args);
        assert_eq!(r.rc, 2, "usage error for {args:?}:\n{}", r.stderr);
        assert!(r.stderr.contains("usage:"), "{}", r.stderr);
        assert!(
            r.log.is_empty(),
            "nothing opened or closed for a malformed call {args:?}: {:?}",
            r.log
        );
    }
}

// --- the ratchet -----------------------------------------------------------

/// The CronJob manifests, each with its `image:` lines: the scope is
/// decided by shape (one container, the boss image), never by name.
fn cronjobs() -> Vec<(String, String)> {
    let dir = repo_root().join(MANIFESTS);
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .map(|p| {
            let rel = format!("{MANIFESTS}/{}", p.file_name().unwrap().to_string_lossy());
            let text = read(&rel);
            (rel, text)
        })
        .filter(|(_, text)| text.lines().any(|l| l == "kind: CronJob"))
        .collect();
    out.sort();
    out
}

fn images(yaml: &str) -> Vec<&str> {
    yaml.lines()
        .filter_map(|l| l.trim_start().strip_prefix("image: "))
        .map(str::trim)
        .collect()
}

fn is_single_boss_image_chore(yaml: &str) -> bool {
    let imgs = images(yaml);
    imgs.len() == 1 && imgs[0].starts_with(BOSS_IMAGE)
}

#[test]
fn every_single_container_boss_chore_records_through_the_one_wrapper() {
    let all = cronjobs();
    assert!(
        all.len() >= 5,
        "only {} CronJob manifests under {MANIFESTS} — the scrape broke",
        all.len()
    );
    let (chores, others): (Vec<_>, Vec<_>) = all
        .iter()
        .partition(|(_, yaml)| is_single_boss_image_chore(yaml));
    // Non-vacuity floor, one below the eight measured on 2026-09-18 so
    // a retired chore does not red the tree while a lint that has lost
    // its subject still does (the shape a-workload-declares-the-user
    // uses).
    assert!(
        chores.len() >= 7,
        "only {} single-container boss-image chores: {:?}",
        chores.len(),
        chores.iter().map(|(r, _)| r).collect::<Vec<_>>()
    );
    for (rel, yaml) in &chores {
        let line = yaml
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("/usr/local/bin/boss-chore.sh "))
            .unwrap_or_else(|| {
                panic!(
                    "{rel} does not invoke /usr/local/bin/boss-chore.sh — every boss-image chore \
                     records ok AND failed through the one wrapper"
                )
            });
        let kind = line.split_whitespace().nth(1).unwrap_or_default();
        assert!(
            kind.starts_with("maintenance-"),
            "{rel}: the wrapper's first argument is the maintenance kind: {line}"
        );
        assert!(
            line.contains(" -- "),
            "{rel}: the check follows a `--`, so the title can carry spaces: {line}"
        );
        // Code lines only: a comment may NAME the helpers (the recognize
        // manifest explains its summary file in terms of boss-step.sh);
        // it cannot run them.
        let code: Vec<&str> = yaml
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        for gone in ["boss-step.sh", "boss-maintenance-wrap.sh"] {
            assert!(
                !code.iter().any(|l| l.contains(gone)),
                "{rel} still invokes {gone} directly — that is the three-line dance whose \
                 close is skipped when the check fails; boss-chore.sh does both halves"
            );
        }
        assert!(
            yaml.contains("- name: BOSS_JOBS_URL\n"),
            "{rel}: the wrapper's helpers refuse without BOSS_JOBS_URL (exit 78)"
        );
    }
    // The two out of scope are out by SHAPE, and the shapes are the
    // ones measured: a multi-image Pod and a non-boss image. A third
    // shape would be a new decision, so it is named here.
    for (rel, yaml) in &others {
        let imgs = images(yaml);
        assert!(
            imgs.len() > 1 || !imgs[0].starts_with(BOSS_IMAGE),
            "{rel} is neither a multi-image Pod nor a foreign image: {imgs:?}"
        );
    }
}

#[test]
fn the_image_carries_the_wrapper_where_the_manifests_run_it() {
    let dockerfile = read(DOCKERFILE);
    let copies = dockerfile.lines().any(|l| {
        l.starts_with("COPY ")
            && l.split_whitespace().any(|w| w == CHORE)
            && l.trim_end().ends_with("/usr/local/bin/")
    });
    assert!(
        copies,
        "{DOCKERFILE} must COPY {CHORE} to /usr/local/bin/ beside boss-step.sh and \
         boss-maintenance-wrap.sh — the wrapper resolves them next to itself"
    );
    let chore = read(CHORE);
    assert!(
        chore.starts_with("#!/usr/bin/env bash\n"),
        "the wrapper is bash, like the two scripts it wraps"
    );
}
