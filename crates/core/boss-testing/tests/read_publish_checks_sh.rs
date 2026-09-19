//! `infra/forge/read-publish-checks.sh` is RUN, not read — against the
//! public GitHub API's own answers for PR #239, copied into
//! `tests/fixtures/github/` on 2026-09-19, with every outside party a
//! stub (a `curl` that serves those files and records every write) and
//! never the network.
//!
//! THE GAP (backlog 321f1409). A mirror publish PR's CodeQL result never
//! returned to the system of record: PR #238 was merged over 64 unread
//! alerts (18 critical) on 2026-09-12 and #239 stalled on 109 (10
//! critical) a week later, and nothing inside BOSS had read either —
//! the check runs after the publish packet's sign-off, on a surface only
//! David sees, at merge time. Measured from the pod over the public API:
//! of the 100 annotations GitHub exposes for #239, 81 are
//! cleartext-logging on a name heuristic (account_id / employee_id /
//! uid in a log line), 9 hard-coded-cryptographic-value on `#[cfg(test)]`
//! constants, and the rest of the same kind — zero real findings. A
//! check nobody reads is a check that is not running (CLAUDE.md
//! §Diagnosis); this verb is the reader, and these cases pin what it
//! reads:
//!
//!   * with every check-run completed, the reading lands on the publish
//!     packet's metadata as `code_scanning` — one entry per check with
//!     its conclusion, and for the code-scanning check the annotation
//!     counts by rule and by file, every number equal to the fixture's
//!     — and the `read-checks` step is completed with `conclusion` and
//!     `alerts`; the LAST line is the answer line the dispatcher rule's
//!     `verdict_pattern` reads (one fact, two files, CLAUDE.md §9a);
//!   * with a check-run still running, nothing is written until it
//!     completes, and past the deadline the verb FAILS naming the run
//!     still in flight — a partial reading, never a clean one;
//!   * a publish packet whose `open-pr` step recorded no head sha is a
//!     refusal naming the step, not a read of nothing;
//!   * `--check` asks only for the tools and the addresses, no network.

use boss_testing::{dispatcher_rules_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/read-publish-checks.sh";
const RULE: &str = "complete-publish-read-checks-on-read-publish-checks-answered";
const JOB: &str = "00000000-0000-0000-0000-0000000000aa";
const STEP: &str = "00000000-0000-0000-0000-0000000000cc";
/// PR #239's head, as `open-pr` recorded it (`snapshot_commit`).
const HEAD: &str = "12d4a68279752c2c451b147ec8e21c248c8681a3";
const PR_URL: &str = "https://github.com/algedonic-dev/boss/pull/239";
/// The CodeQL check-run's id in the fixture — the annotations URL
/// GitHub hands back is keyed on it.
const CODEQL_RUN: &str = "105867839495";

fn fixture(name: &str) -> PathBuf {
    repo_root()
        .join("crates/core/boss-testing/tests/fixtures/github")
        .join(name)
}

fn fixture_json(name: &str) -> serde_json::Value {
    let path = fixture(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The rule's `verdict_pattern`, off its file — the regex the handler
/// reads the answer line with (the tag_release_sh idiom).
fn rule_pattern() -> regex::Regex {
    let path = dispatcher_rules_dir().join(format!("{RULE}.toml"));
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args = &doc["rule"][0]["do"][0]["args"];
    let src = args["verdict_pattern"].as_str().expect("verdict_pattern");
    let inner = src
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("verdict_pattern is not an expr string literal: {src}"));
    regex::Regex::new(inner).unwrap_or_else(|e| panic!("{RULE}'s verdict_pattern: {e}"))
}

/// One offline run. The jobs API and GitHub are one `curl` stub: a GET
/// is answered from `routes` (URL substring -> file, first match), a
/// PUT/PATCH is recorded to `writes` with its body and answered 200.
struct Run {
    root: PathBuf,
    routes: PathBuf,
    writes: PathBuf,
    log: PathBuf,
}

impl Run {
    fn new(case: &str, open_pr_metadata: serde_json::Value) -> Run {
        let root = scratch_dir(&format!("read-publish-checks-{case}"));
        let routes = root.join("routes");
        let writes = root.join("writes");
        let log = root.join("curl.log");
        write_file(&routes, "");
        write_file(&writes, "");

        // One open publish-to-github packet: open-pr completed with what
        // the publish verb records, read-checks ready.
        let jobs = root.join("jobs.json");
        write_file(
            &jobs,
            &serde_json::json!({"data": [{
                "id": JOB, "title": "publish to github", "status": "open",
                "metadata": {"target": "origin/main"},
                "steps": [
                    {"id": "00000000-0000-0000-0000-0000000000bb", "spec_slug": "open-pr",
                     "status": "completed", "metadata": open_pr_metadata},
                    {"id": STEP, "spec_slug": "read-checks", "status": "ready",
                     "metadata": {"ops_verb": "read-publish-checks"}}
                ]
            }]})
            .to_string(),
        );

        let stubs = root.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        write_exec(
            &stubs.join("curl"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
url=""; data=""; method=GET; prev=""
for a in "$@"; do
    case "$prev" in --data-binary) data="$a" ;; -X) method="$a" ;; esac
    case "$a" in http://*|https://*) url="$a" ;; esac
    prev="$a"
done
if [ "$method" != GET ]; then
    printf '%s %s\n' "$method" "$url" >> '{writes}'
    case "$data" in @*) cat "${{data#@}}" >> '{writes}'; echo >> '{writes}' ;; esac
    exit 0
fi
# First route whose file still exists wins; a `*.once.json` file is
# served once and removed, so the next poll falls through to the route
# behind it (a check list that fills in between two polls).
while IFS='	' read -r needle file; do
    [ -f "$file" ] || continue
    case "$url" in *"$needle"*)
        cat "$file"
        case "$file" in *.once.json) rm -f "$file" ;; esac
        exit 0 ;;
    esac
done < '{routes}'
echo "curl: (22) The requested URL returned error: 404 for $url" >&2
exit 22
"#,
                log = log.display(),
                writes = writes.display(),
                routes = routes.display(),
            ),
        );
        let run = Run {
            root,
            routes,
            writes,
            log,
        };
        run.route("jobs.invalid/api/jobs?kind=publish-to-github", &jobs);
        run
    }

    /// A GET whose URL contains `needle` answers with `file`.
    fn route(&self, needle: &str, file: &Path) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.routes)
            .unwrap();
        writeln!(f, "{needle}\t{}", file.display()).unwrap();
    }

    /// GitHub's answers for #239, complete: three check-runs, all done,
    /// and one page of a hundred annotations followed by an empty one.
    fn route_pr239_complete(&self) {
        self.route(
            &format!("/commits/{HEAD}/check-runs"),
            &fixture("check-runs-pr239.json"),
        );
        self.route(
            &format!("/check-runs/{CODEQL_RUN}/annotations?per_page=100&page=1"),
            &fixture("codeql-annotations-pr239.json"),
        );
        let empty = self.root.join("empty.json");
        write_file(&empty, "[]");
        self.route(
            &format!("/check-runs/{CODEQL_RUN}/annotations?per_page=100&page=2"),
            &empty,
        );
    }

    fn go(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
        let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:{outer}", self.root.join("stubs").display()),
            )
            .env("BOSS_JOBS_URL", "http://jobs.invalid")
            .env("BOSS_GITHUB_API", "https://api.github.invalid")
            .env("BOSS_MIRROR_SLUG", "fixture-upstream/mirror")
            // No waiting in a test: the loop polls at once and the
            // deadline is the number of polls it tolerates.
            .env("BOSS_CHECKS_POLL_SECONDS", "0")
            .env("BOSS_CHECKS_DEADLINE_SECONDS", "3");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the verb runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn writes(&self) -> String {
        std::fs::read_to_string(&self.writes).unwrap_or_default()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The `code_scanning` reading the verb PATCHed onto the packet.
    fn reading(&self) -> serde_json::Value {
        let writes = self.writes();
        let body = writes
            .lines()
            .skip_while(|l| {
                !l.starts_with(&format!(
                    "PATCH http://jobs.invalid/api/jobs/{JOB}/metadata"
                ))
            })
            .nth(1)
            .unwrap_or_else(|| panic!("no metadata PATCH on the packet; writes:\n{writes}"));
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["code_scanning"].clone()
    }

    /// The body of the PUT that completed the read-checks step.
    fn step_put(&self) -> serde_json::Value {
        let writes = self.writes();
        let body = writes
            .lines()
            .skip_while(|l| {
                !l.starts_with(&format!(
                    "PUT http://jobs.invalid/api/jobs/{JOB}/steps/{STEP}"
                ))
            })
            .nth(1)
            .unwrap_or_else(|| panic!("no PUT on the read-checks step; writes:\n{writes}"));
        serde_json::from_str(body).unwrap()
    }
}

fn open_pr_done() -> serde_json::Value {
    serde_json::json!({
        "ops_verb": "read-publish-checks",
        "pr_url": PR_URL, "snapshot_commit": HEAD,
        "head": "dauld:publish/2026-09-19", "published_by": "publish-github-pr"
    })
}

#[test]
fn a_completed_prs_checks_and_alerts_are_read_onto_the_packet_and_the_step_completed() {
    let run = Run::new("complete", open_pr_done());
    run.route_pr239_complete();
    let (code, text) = run.go(&[], &[]);
    assert_eq!(code, 0, "{text}");

    // The reading on the packet: every check with its conclusion.
    let reading = run.reading();
    assert_eq!(reading["head"], HEAD);
    assert_eq!(reading["pr_url"], PR_URL);
    assert_eq!(reading["complete"], true);
    let checks = reading["checks"].as_array().expect("checks is a list");
    let mut names: Vec<(&str, &str)> = checks
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["conclusion"].as_str().unwrap(),
            )
        })
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            ("Analyze (javascript-typescript)", "success"),
            ("Analyze (rust)", "success"),
            ("CodeQL", "failure"),
        ]
    );

    // The code-scanning half: the counts the packet was filed with,
    // read from the fixture and never typed.
    let alerts = &reading["alerts"];
    assert_eq!(alerts["check"], "CodeQL");
    assert_eq!(alerts["conclusion"], "failure");
    assert_eq!(
        alerts["title"],
        "109 new alerts including 10 critical severity security vulnerabilities"
    );
    assert_eq!(
        alerts["declared"], 100,
        "annotations_count on the check-run"
    );
    assert_eq!(
        alerts["read"], 100,
        "every annotation the API exposes was read"
    );
    assert_eq!(
        alerts["caveat"], true,
        "the check's own footnote — a snapshot this large reads as all-new code — is on the record"
    );
    let by_rule: Vec<(String, u64)> = alerts["by_rule"]
        .as_array()
        .expect("by_rule is a list")
        .iter()
        .map(|r| {
            (
                r["rule"].as_str().unwrap().to_string(),
                r["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        by_rule,
        vec![
            ("Cleartext logging of sensitive information".to_string(), 81),
            ("Hard-coded cryptographic value".to_string(), 9),
            (
                "Cleartext transmission of sensitive information".to_string(),
                4
            ),
            ("Uncontrolled allocation size".to_string(), 2),
            ("Uncontrolled data used in path expression".to_string(), 2),
            ("Incomplete multi-character sanitization".to_string(), 1),
            ("Server-side request forgery".to_string(), 1),
        ],
        "counts by rule, largest first, ties by name"
    );
    let by_file = alerts["by_file"].as_array().expect("by_file is a list");
    assert_eq!(by_file.len(), 50, "50 distinct files in the fixture");
    assert_eq!(
        by_file[0]["path"],
        "crates/core/boss-gateway/src/passkey.rs"
    );
    assert_eq!(by_file[0]["count"], 8);
    assert_eq!(alerts["by_level"]["failure"], 100);

    // The step: completed, with the two fields the protocol requires
    // and the evidence key.
    let put = run.step_put();
    assert_eq!(put["status"], "completed");
    assert_eq!(put["metadata"]["conclusion"], "failure");
    assert_eq!(put["metadata"]["alerts"], "100");
    assert_eq!(put["metadata"]["rules"], "7");
    assert_eq!(put["metadata"]["read_by"], "read-publish-checks");
    assert_eq!(
        put["metadata"]["ops_verb"], "read-publish-checks",
        "the step's existing metadata is merged, not replaced"
    );

    // The answer line is LAST and the rule reads it.
    let last = text.lines().last().unwrap_or("");
    assert!(
        last.starts_with("read-publish-checks: read failure — 100 alerts in 7 rules over 50 files"),
        "the last line is the answer line: {last}"
    );
    let caps = rule_pattern().captures(last).unwrap_or_else(|| {
        panic!("{RULE}'s verdict_pattern does not read the answer line: {last}")
    });
    assert_eq!(&caps["conclusion"], "failure");
    assert_eq!(&caps["alerts"], "100");
    assert_eq!(&caps["rules"], "7");
    assert!(
        rule_pattern()
            .captures("read-publish-checks: FAILED — 3 check-runs on 12d4a682, 1 still running after 1500s: Analyze (rust)")
            .is_none(),
        "the failure shape shares the prefix and must not read as an answer"
    );
    // Both writes went to the publish packet, in that order: the reading
    // before the step, so a step that reads done always has one.
    let writes = run.writes();
    let patch_at = writes.find("PATCH ").expect("a PATCH");
    let put_at = writes.find("PUT ").expect("a PUT");
    assert!(
        patch_at < put_at,
        "the reading lands before the step completes:\n{writes}"
    );
}

/// A check-run still running is not a reading. The verb polls until it
/// completes; here it never does, and past the deadline the verb FAILS
/// naming the run — with what it saw on the packet as a PARTIAL reading
/// (`complete: false`), and the step left open.
#[test]
fn a_check_still_running_is_waited_for_and_named_when_the_deadline_passes() {
    let run = Run::new("running", open_pr_done());
    let mut checks = fixture_json("check-runs-pr239.json");
    // Analyze (rust) is the slow one: 13 minutes on both #238 and #239.
    let rust = checks["check_runs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["name"] == "Analyze (rust)")
        .expect("the fixture has Analyze (rust)");
    rust["status"] = serde_json::json!("in_progress");
    rust["conclusion"] = serde_json::Value::Null;
    rust["completed_at"] = serde_json::Value::Null;
    let running = run.root.join("check-runs-running.json");
    write_file(&running, &checks.to_string());
    run.route(&format!("/commits/{HEAD}/check-runs"), &running);

    let (code, text) = run.go(&[], &[]);
    assert_eq!(code, 1, "{text}");
    let last = text.lines().last().unwrap_or("");
    assert!(
        last.starts_with("read-publish-checks: FAILED — ") && last.contains("Analyze (rust)"),
        "the failure names the run still in flight: {last}"
    );
    assert!(
        run.log().matches("/check-runs").count() >= 2,
        "the verb polled more than once before giving up:\n{}",
        run.log()
    );
    // What it saw is on the record, marked partial; the step is not done.
    let reading = run.reading();
    assert_eq!(reading["complete"], false);
    assert_eq!(reading["checks"].as_array().unwrap().len(), 3);
    assert!(
        !run.writes().contains("PUT "),
        "a partial reading completes nothing:\n{}",
        run.writes()
    );
}

/// Before the first check-run is registered, the commit's list is empty
/// — GitHub answers `total_count: 0`, which is "not yet", never "no
/// checks". The verb keeps polling and reads the full set once it lands.
#[test]
fn an_empty_check_list_is_not_yet_and_the_next_poll_reads_it() {
    let run = Run::new("empty-then-complete", open_pr_done());
    // Served once, then gone: the second poll falls through to #239's
    // complete listing routed behind it.
    let empty = run.root.join("no-check-runs.once.json");
    write_file(&empty, r#"{"total_count": 0, "check_runs": []}"#);
    run.route(&format!("/commits/{HEAD}/check-runs"), &empty);
    run.route_pr239_complete();

    let (code, text) = run.go(&[], &[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(run.reading()["complete"], true);
    assert_eq!(
        run.log()
            .matches(&format!("/commits/{HEAD}/check-runs"))
            .count(),
        2,
        "the verb polled once more after the empty list, and no more:\n{}",
        run.log()
    );
}

#[test]
fn a_packet_whose_open_pr_recorded_no_head_is_refused_naming_the_step() {
    let run = Run::new("no-head", serde_json::json!({"pr_url": PR_URL}));
    run.route_pr239_complete();
    let (code, text) = run.go(&[], &[]);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("open-pr") && text.contains("snapshot_commit"),
        "the refusal names the step and the missing field:\n{text}"
    );
    assert!(
        !run.log().contains("api.github.invalid"),
        "nothing was read from GitHub:\n{}",
        run.log()
    );
    assert_eq!(run.writes(), "", "nothing was written");
}

#[test]
fn no_packet_waiting_for_its_reading_is_nothing_to_do() {
    let run = Run::new("nothing", open_pr_done());
    let jobs = run.root.join("none.json");
    write_file(&jobs, r#"{"data": []}"#);
    // Routes are first-match and `new` routed the packet on the
    // jobs.invalid host, so this case reads a jobs API of its own.
    run.route("none.invalid/api/jobs?kind=publish-to-github", &jobs);
    let (code, text) = run.go(&[], &[("BOSS_JOBS_URL", "http://none.invalid")]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to do"), "{text}");
    assert_eq!(run.writes(), "");
}

#[test]
fn check_asks_for_tools_and_addresses_and_touches_no_network() {
    let run = Run::new("check", open_pr_done());
    let (code, text) = run.go(&["--check"], &[]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("--check ok"), "{text}");
    assert!(
        text.contains("fixture-upstream/mirror"),
        "the check names the mirror it would read:\n{text}"
    );
    assert_eq!(run.log(), "", "--check made no request");
    assert_eq!(run.writes(), "");
}

/// The verb file: serves the forge, reads the script in the tree, admits
/// only the literal `--check`, and declares a timeout above the deadline
/// the script bounds its own wait with — so the runner's kill never
/// pre-empts the script's own FAILED line.
#[test]
fn the_verb_file_serves_the_forge_and_outlives_the_scripts_own_deadline() {
    let path = repo_root().join("infra/ops/verbs/read-publish-checks.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"][0], SCRIPT);
    assert_eq!(v["params"][0]["one_of"], serde_json::json!(["--check"]));
    assert_eq!(v["params"][0]["optional"], true);
    let timeout = v["timeout"].as_u64().expect("a timeout");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let deadline: u64 = script
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("DEADLINE=\"${BOSS_CHECKS_DEADLINE_SECONDS:-")
                .and_then(|r| r.strip_suffix("}\""))
                .and_then(|n| n.parse().ok())
        })
        .expect("the script declares DEADLINE=\"${BOSS_CHECKS_DEADLINE_SECONDS:-<n>}\"");
    assert!(
        timeout > deadline,
        "verb timeout {timeout}s must exceed the script's deadline {deadline}s"
    );
}
