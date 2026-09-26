//! A pre-flight lint that reads the system of record waits out a
//! rollout the way the door does — through the SAME function.
//!
//! MEASURED 2026-09-23 (backlog 834ddb7c): gate-run 44da8e5a was refused
//! before any check ran, because
//! `the-live-protocols-are-the-authored-protocols` curled the jobs API
//! during a stack rollout (train #576 converging) and got HTTP 000; the
//! relaunch a minute later went green on the same diff. The door
//! (`infra/dev/boss-api`) had learned to wait out that dark minute that
//! morning (034002b3, #572), and the three lints that curl a registry
//! themselves had not, so every rollout could refuse any gate that
//! launched inside it. The refusal was correct — exit 3 is an
//! infrastructure refusal, not a red (a26f92c4) — but a refusal still
//! costs a relaunch, and the machine could have waited instead.
//!
//! The wait is ONE definition, `curl_through_a_roll` in
//! `infra/lib/curl-through-a-roll.sh`, sourced by the door and, through
//! `infra/lint/lib/sor-read.sh`, by every lint that reads a registry. A
//! second copy is the defect (CLAUDE.md §9a), so the first test counts
//! definitions across `infra/`.
//!
//! The lints run from the repo root against a stub `curl` on PATH that
//! refuses the way real curl does (exit 7, `000` on stdout) and a stub
//! `sleep` that records the wait and returns at once, so the backoff is
//! read without being spent.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The one file that defines the wait.
const LIB: &str = "infra/lib/curl-through-a-roll.sh";
/// The lint helper every registry-reading lint goes through.
const LINT_LIB: &str = "infra/lint/lib/sor-read.sh";

/// Each registry-reading lint in the pre-flight roster, with the env var
/// its header names for its read surface.
const LIVE_LINTS: &[(&str, &str)] = &[
    (
        "infra/lint/the-live-protocols-are-the-authored-protocols.sh",
        "BOSS_JOBS_URL",
    ),
    (
        "infra/lint/the-live-rules-are-the-authored-rules.sh",
        "BOSS_DISPATCHER_URL",
    ),
    (
        "infra/lint/a-car-stays-under-the-edit-level.sh",
        "BOSS_JOBS_URL",
    ),
];

struct Stubs {
    bin: PathBuf,
    calls: PathBuf,
    sleeps: PathBuf,
}

/// A `curl` that refuses its first `STUB_REFUSALS` calls (exit
/// `STUB_RC`, default 7, `000` on stdout as `-w '%{http_code}'` prints
/// it), then answers `STUB_CODE` (default 503) with no body; and a
/// `sleep` that records what it was asked for.
///
/// A call to `[::1]:9` is refused and NOT counted: that is the port
/// `the-live-protocols-are-the-authored-protocols` points its own
/// self-test child at on every run (refused by design), and it is not
/// the read this file is about. The child asks for a zero window, so a
/// wait it spent would show up in `sleeps.txt`.
fn stubs(tag: &str) -> Stubs {
    let root = scratch_dir(&format!("lint-roll-{tag}"));
    let bin = root.join("bin");
    create_dir(&bin);
    let calls = root.join("curl-calls.txt");
    let sleeps = root.join("sleeps.txt");
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             case \"$*\" in *'[::1]:9'*) printf '000'; exit 7 ;; esac\n\
             echo call >> '{calls}'\n\
             n=$(wc -l < '{calls}')\n\
             if [ \"$n\" -le \"${{STUB_REFUSALS:-0}}\" ]; then\n\
                 echo 'curl: (7) Failed to connect to registry.test port 7900: No route to host' >&2\n\
                 printf '000'\n\
                 exit \"${{STUB_RC:-7}}\"\n\
             fi\n\
             printf '%s' \"${{STUB_CODE:-503}}\"\n",
            calls = calls.display()
        ),
    );
    write_exec(
        &bin.join("sleep"),
        &format!(
            "#!/usr/bin/env bash\necho \"$1\" >> '{}'\n",
            sleeps.display()
        ),
    );
    Stubs { bin, calls, sleeps }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_lint(s: &Stubs, rel: &str, url_var: &str, env: &[(&str, &str)]) -> Run {
    let root = repo_root();
    let mut cmd = Command::new("bash");
    cmd.arg(root.join(rel))
        .current_dir(&root)
        .stdin(Stdio::null())
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env(url_var, "http://registry.test:7900")
        .env_remove("BOSS_SOR_WAIT_SECONDS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("run {rel}: {e}"));
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Every `.sh` file and extensionless script under `dir`, recursively.
fn scripts_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scripts_under(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// ONE DEFINITION. The door and the lints share the wait; a lint that
/// grew its own copy would drift from the door's exit-code rule the way
/// `infra/boss-api-curl.sh` had (it re-sent 28, 52 and 56 until it
/// sourced this one too, c51967b5 — pinned in a_chore_write_is_sent_once.rs).
#[test]
fn the_roll_wait_has_one_definition_and_the_door_sources_it() {
    let root = repo_root();
    let mut files = Vec::new();
    scripts_under(&root.join("infra"), &mut files);
    let defining: Vec<String> = files
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p).is_ok_and(|t| {
                t.lines()
                    .any(|l| l.trim_start().starts_with("curl_through_a_roll()"))
            })
        })
        .map(|p| {
            p.strip_prefix(&root)
                .expect("under the root")
                .display()
                .to_string()
        })
        .collect();
    assert_eq!(
        defining,
        vec![LIB.to_string()],
        "curl_through_a_roll is defined once, in {LIB}; a second definition is a copy that will drift"
    );
    let door = std::fs::read_to_string(root.join("infra/dev/boss-api")).expect("read boss-api");
    assert!(
        door.contains("../lib/curl-through-a-roll.sh"),
        "the door sources the one definition from beside its REAL location"
    );
    let lint_lib = std::fs::read_to_string(root.join(LINT_LIB)).expect("read the lint helper");
    assert!(
        lint_lib.contains("lib/curl-through-a-roll.sh"),
        "{LINT_LIB} sources the one definition"
    );
}

/// Every registry-reading lint re-sends a refused connect once the stub
/// lets it through, waiting the door's first backoff step, and reads the
/// answer it then gets. The answer is a 503, so each still refuses
/// (exit 3) — what this pins is the SECOND call and the one wait.
#[test]
fn every_live_lint_waits_out_a_refused_connect() {
    for (rel, var) in LIVE_LINTS {
        let tag = Path::new(rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lint");
        let s = stubs(&format!("waits-{tag}"));
        let r = run_lint(&s, rel, var, &[("STUB_REFUSALS", "1")]);
        assert_eq!(
            read_lines(&s.calls).len(),
            2,
            "{rel}: a refused connect is re-sent once the registry answers:\n{}",
            r.stderr
        );
        assert_eq!(
            read_lines(&s.sleeps),
            vec!["2".to_string()],
            "{rel}: one wait, the door's first backoff step:\n{}",
            r.stderr
        );
        assert!(
            r.stderr
                .contains("is not answering (a rollout?) — retrying GET "),
            "{rel}: the wait says so, once, on stderr:\n{}",
            r.stderr
        );
        assert_eq!(
            r.code, 3,
            "{rel}: the 503 that came back is still a refusal:\n{}\n{}",
            r.stdout, r.stderr
        );
        assert!(
            r.stderr.contains("HTTP 503"),
            "{rel}: the lint judged the answer it got AFTER the wait:\n{}",
            r.stderr
        );
    }
}

/// And an answer after the wait is a verdict: the edit-level lint reads
/// a 404 as "this instance has no level door" and passes, exactly as it
/// would have without the roll.
#[test]
fn an_answer_after_the_wait_is_judged_as_an_answer() {
    let s = stubs("answer-after-wait");
    let r = run_lint(
        &s,
        "infra/lint/a-car-stays-under-the-edit-level.sh",
        "BOSS_JOBS_URL",
        &[("STUB_REFUSALS", "2"), ("STUB_CODE", "404")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(read_lines(&s.calls).len(), 3, "two refusals, then the read");
    assert_eq!(read_lines(&s.sleeps), vec!["2", "4"], "the waits back off");
    assert!(r.stdout.contains("clean"), "{}", r.stdout);
}

/// Past the window the lint refuses as it always did — exit 3, the
/// CANNOT ANSWER marker, `HTTP 000` — with the wait's own line naming
/// how long it waited, and never a clean pass.
#[test]
fn a_roll_that_outlasts_the_window_is_still_a_refusal() {
    for (rel, var) in LIVE_LINTS {
        let tag = Path::new(rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lint");
        let s = stubs(&format!("outlasted-{tag}"));
        let r = run_lint(
            &s,
            rel,
            var,
            &[("STUB_REFUSALS", "99"), ("BOSS_SOR_WAIT_SECONDS", "0")],
        );
        assert_eq!(r.code, 3, "{rel}:\n{}\n{}", r.stdout, r.stderr);
        assert_eq!(
            read_lines(&s.calls).len(),
            1,
            "{rel}: a zero window asks once"
        );
        assert!(read_lines(&s.sleeps).is_empty(), "{rel}: and never waits");
        assert!(
            r.stderr.contains("refused every connection for ")
                && r.stderr.contains("waited out for up to 0s")
                && r.stderr.contains("CANNOT ANSWER")
                && r.stderr.contains("HTTP 000"),
            "{rel}: the refusal names the wait and keeps the lint's own marker:\n{}",
            r.stderr
        );
        assert!(
            !r.stdout.contains("clean"),
            "{rel}: a refusal must not read as a pass:\n{}",
            r.stdout
        );
    }
}

/// A window that is not a whole number of seconds is not read as zero
/// or as forever: the read is refused before curl runs, and the lint
/// refuses with it.
#[test]
fn a_window_that_is_not_a_number_is_refused_before_curl() {
    let s = stubs("bad-window");
    let r = run_lint(
        &s,
        "infra/lint/a-car-stays-under-the-edit-level.sh",
        "BOSS_JOBS_URL",
        &[("BOSS_SOR_WAIT_SECONDS", "1m")],
    );
    assert_eq!(r.code, 3, "{}\n{}", r.stdout, r.stderr);
    assert!(read_lines(&s.calls).is_empty(), "curl did not run");
    assert!(
        r.stderr.contains("BOSS_SOR_WAIT_SECONDS='1m'"),
        "{}",
        r.stderr
    );
}

/// A LINT WAITS LESS THAN THE DOOR. The door's 120s is a write an
/// operator is waiting on; a lint's wait is a gate sitting on a dark
/// registry, and the pre-flight runs three of them in sequence. Both
/// numbers are read from their one definition.
#[test]
fn a_lint_waits_less_than_the_door() {
    let root = repo_root();
    let door = std::fs::read_to_string(root.join("infra/dev/boss-api")).expect("read boss-api");
    let door_window: u64 = door
        .lines()
        .find_map(|l| l.strip_prefix("WAIT_WINDOW=${BOSS_SOR_WAIT_SECONDS:-"))
        .and_then(|rest| rest.strip_suffix('}'))
        .and_then(|n| n.parse().ok())
        .expect("boss-api spells WAIT_WINDOW=${BOSS_SOR_WAIT_SECONDS:-<n>}");
    let lint_lib = std::fs::read_to_string(root.join(LINT_LIB)).expect("read the lint helper");
    let lint_window: u64 = lint_lib
        .lines()
        .find_map(|l| l.strip_prefix("LINT_SOR_WAIT_SECONDS="))
        .and_then(|n| n.trim().parse().ok())
        .expect("sor-read.sh spells LINT_SOR_WAIT_SECONDS=<n>");
    assert!(
        lint_window > 0 && lint_window < door_window,
        "a lint waits out a roll for less than the door's {door_window}s, and for more than nothing: {lint_window}"
    );
}
