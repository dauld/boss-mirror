//! The cron chores' jobs-API helper re-sends a request only while it
//! never left — the same rule as the door, through the same function.
//!
//! MEASURED 2026-09-23 (backlog c51967b5): `infra/boss-api-curl.sh`
//! (25e518c0) re-sent on curl exits 6, 7, 28, 35, 52, 55 and 56. Exits
//! 28 (a timeout, which curl also gives a transfer that timed out after
//! the body went), 52 (empty reply), 55 (send failure) and 56 (receive
//! failure) can each follow a request the server already received, so
//! a chore's POST or PUT — a maintenance packet opened, a step
//! completed, an observation filed — could land twice. The door and
//! the pre-flight lints already share `curl_through_a_roll`
//! (`infra/lib/curl-through-a-roll.sh`, 834ddb7c), which re-sends only
//! 6 and 7. The helper now sources that one definition rather than
//! carrying a second retry rule (CLAUDE.md §9a).
//!
//! The helper runs against a stub `curl` on PATH that fails the way
//! real curl does and a stub `sleep` that records the wait and returns
//! at once, so the backoff is read without being spent.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HELPER: &str = "infra/boss-api-curl.sh";
const LIB: &str = "infra/lib/curl-through-a-roll.sh";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const CRAWL: &str = "infra/cluster/manifests/boss-playground-crawl.yaml";
const OBSERVER: &str = "infra/cluster/manifests/boss-estate-observe.yaml";

struct Stubs {
    bin: PathBuf,
    calls: PathBuf,
    sleeps: PathBuf,
}

/// A `curl` that fails its first `STUB_FAILS` calls with exit
/// `STUB_RC` (printing `000`, as `-w '%{http_code}'` would), then
/// answers `{"ok":true}`; and a `sleep` that records what it was asked.
fn stubs(tag: &str) -> Stubs {
    let root = scratch_dir(&format!("chore-curl-{tag}"));
    let bin = root.join("bin");
    create_dir(&bin);
    let calls = root.join("curl-calls.txt");
    let sleeps = root.join("sleeps.txt");
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> '{calls}'\n\
             n=$(wc -l < '{calls}')\n\
             if [ \"$n\" -le \"${{STUB_FAILS:-0}}\" ]; then\n\
                 echo \"curl: ($STUB_RC) stub failure\" >&2\n\
                 printf '000'\n\
                 exit \"$STUB_RC\"\n\
             fi\n\
             printf '%s' '{{\"ok\":true}}'\n",
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

fn run(helper: &Path, s: &Stubs, env: &[(&str, &str)], args: &[&str]) -> Run {
    let mut cmd = Command::new("bash");
    cmd.arg(helper)
        .args(args)
        .stdin(Stdio::null())
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("BOSS_API_RETRY_DEADLINE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("run {HELPER}: {e}"));
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

const POST: &[&str] = &[
    "-fsS",
    "-X",
    "POST",
    "-H",
    "content-type: application/json",
    "-d",
    "{}",
    "http://registry.test:7900/api/jobs",
];

/// THE PACKET'S CASE. A POST answered by an empty reply (exit 52) may
/// have been received — so it is sent once, and the exit is the
/// caller's. Likewise every other exit that can follow a request that
/// left: a timeout, a TLS failure, a send or receive failure.
#[test]
fn a_post_whose_request_may_have_left_is_sent_once() {
    for rc in ["52", "28", "35", "55", "56"] {
        let s = stubs(&format!("once-{rc}"));
        let r = run(
            &repo_root().join(HELPER),
            &s,
            &[("STUB_FAILS", "1"), ("STUB_RC", rc)],
            POST,
        );
        assert_eq!(
            lines(&s.calls).len(),
            1,
            "curl exit {rc}: the POST may have landed, so it is sent exactly once:\n{}",
            r.stderr
        );
        assert!(
            lines(&s.sleeps).is_empty(),
            "curl exit {rc}: and nothing waits"
        );
        assert_eq!(
            r.code.to_string(),
            rc,
            "curl exit {rc} is the helper's exit"
        );
    }
}

/// A refused connect never left, so it is re-sent after the shared
/// function's first backoff step — and stdout carries the ANSWER alone,
/// not the `000` each refused attempt printed before it.
#[test]
fn a_refused_connect_is_waited_out_and_only_the_answer_is_printed() {
    let s = stubs("refused");
    let r = run(
        &repo_root().join(HELPER),
        &s,
        &[("STUB_FAILS", "1"), ("STUB_RC", "7")],
        POST,
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(lines(&s.calls).len(), 2, "refused once, then sent");
    assert_eq!(lines(&s.sleeps), vec!["2"], "the shared first backoff step");
    assert_eq!(r.stdout, "{\"ok\":true}", "the answer, and only the answer");
    assert!(
        r.stderr
            .contains("boss-api-curl: the jobs API is not answering (a rollout?)")
            && r.stderr.contains("POST http://registry.test:7900/api/jobs"),
        "the wait names the helper and the request:\n{}",
        r.stderr
    );
}

/// The deadline is still `BOSS_API_RETRY_DEADLINE`, now as the shared
/// function's window: a zero window asks once, never waits, and
/// returns curl's own exit.
#[test]
fn the_deadline_is_the_window() {
    let s = stubs("deadline");
    let r = run(
        &repo_root().join(HELPER),
        &s,
        &[
            ("STUB_FAILS", "99"),
            ("STUB_RC", "7"),
            ("BOSS_API_RETRY_DEADLINE", "0"),
        ],
        POST,
    );
    assert_eq!(r.code, 7, "{}", r.stderr);
    assert_eq!(lines(&s.calls).len(), 1, "a zero window asks once");
    assert!(lines(&s.sleeps).is_empty(), "and never waits");
    assert!(r.stderr.contains("waited out for up to 0s"), "{}", r.stderr);
}

/// A copy of the helper carried somewhere without `lib/` beside it
/// cannot wait out a roll, and must not quietly send without the wait:
/// it refuses before curl runs, naming the file it could not find.
#[test]
fn a_helper_without_its_lib_refuses_before_curl() {
    let s = stubs("no-lib");
    let lone = scratch_dir("chore-curl-lone").join("boss-api-curl.sh");
    write_exec(&lone, &read(HELPER));
    let r = run(&lone, &s, &[("STUB_RC", "0")], POST);
    assert_ne!(r.code, 0, "{}", r.stdout);
    assert!(lines(&s.calls).is_empty(), "curl did not run");
    assert!(
        r.stderr.contains("lib/curl-through-a-roll.sh"),
        "the refusal names the missing file:\n{}",
        r.stderr
    );
}

/// ONE DEFINITION, AND EVERY PLACE THE HELPER RUNS CARRIES IT. The
/// helper sources the lib from `lib/` beside its real location: in the
/// checkout that is `infra/lib/`; in the boss image the Dockerfile must
/// put it at `/usr/local/bin/lib/`; and the playground crawl copies the
/// helper out of that image into `/tools/`, so it must copy `lib/` too.
/// The gate never builds the image, so this is where a missing COPY is
/// caught (the launcher's same failure bricked production, 2026-09-05).
#[test]
fn every_place_the_helper_runs_carries_the_one_definition() {
    let helper = read(HELPER);
    assert!(
        helper.contains("/lib/curl-through-a-roll.sh"),
        "{HELPER} sources {LIB}"
    );
    let code: Vec<&str> = helper
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        !code.iter().any(|l| l.contains("28")),
        "{HELPER} carries no exit list of its own — the one rule is in {LIB}"
    );

    let dockerfile = read(DOCKERFILE);
    let bin_dir = dockerfile
        .lines()
        .filter(|l| l.starts_with("COPY "))
        .find(|l| l.split_whitespace().any(|w| w == HELPER))
        .and_then(|l| l.split_whitespace().last())
        .map(|d| d.trim_end_matches('/').to_string())
        .expect("the Dockerfile COPYs the helper");
    let want = format!("{bin_dir}/lib/curl-through-a-roll.sh");
    assert!(
        dockerfile.lines().any(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            w.first() == Some(&"COPY") && w.contains(&LIB) && w.last() == Some(&want.as_str())
        }),
        "{DOCKERFILE} must COPY {LIB} to {want}, beside the helper's own COPY — \
         the helper refuses without it"
    );

    let crawl = read(CRAWL);
    assert!(
        crawl.contains(&format!("- {bin_dir}/lib\n")) && crawl.contains("- -r\n"),
        "{CRAWL}: the tools init container copies {bin_dir}/lib (recursively) beside the helper"
    );
}

/// The estate observer cannot source anything — it runs in the
/// alpine/k8s image, which carries none of this repo's scripts — so its
/// inline `api_curl` is a copy of the rule, and a copy is pinned: it
/// re-sends exactly the exits the one definition does. It POSTs the
/// observation and PUTs steps, so it had the same double-send.
#[test]
fn the_observers_inline_copy_resends_the_same_exits() {
    let arm = |text: &str, from: &str| -> String {
        let body: String = text
            .lines()
            .skip_while(|l| !l.trim_start().starts_with(from))
            .take_while(|l| l.trim() != "}")
            .collect::<Vec<_>>()
            .join("\n");
        body.lines()
            .map(str::trim)
            .find(|l| l.ends_with(") ;;") && !l.starts_with('*') && !l.starts_with("0)"))
            .unwrap_or_else(|| panic!("{from}: no re-send arm"))
            .split(')')
            .next()
            .unwrap_or_default()
            .replace(' ', "")
    };
    let lib = arm(&read(LIB), "curl_through_a_roll()");
    let observer = arm(&read(OBSERVER), "api_curl()");
    assert_eq!(lib, "6|7", "{LIB} re-sends 6 and 7");
    assert_eq!(
        observer, lib,
        "{OBSERVER}: the inline api_curl re-sends what {LIB} re-sends"
    );
}
