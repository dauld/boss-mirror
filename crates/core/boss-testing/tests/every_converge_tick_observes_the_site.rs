//! EVERY converge tick observes the tenant SITE — what the instance's
//! gateway serves for `/` under the site hostname — and records it on
//! the run step beside the connector: `site` as an OBJECT (`host`,
//! `namespace`, `http_status`, `bytes`, `hash`, `observed_at`) and
//! `site_line` as prose. The lib function is RUN against a stub
//! `kubectl` (the gateway Service's address) and a stub `curl` (the
//! answer), the way every_converge_tick_observes_the_connector runs
//! `observe_connector`, so every field below is one it recorded and
//! every call below is one it made.
//!
//! WHY (backlog e114238a; design b64c4377). Site ops by protocol — the
//! tenant workflow publish-the-landing-page in david/algedonic-llc —
//! closes its loop on the converge's RECORD of what it served, never on
//! a belief: the publish step records `site_hash` = sha256 of
//! site/index.html at the merge, and the dispatcher rule
//! (jobs.complete_step_matching, event_path `steps.run.site.hash`)
//! completes the `live` step when a closed converge's run step carries
//! the same hash. So the converge must hash the BODY the gateway served
//! for `/`, exactly as received, and record it where the rule reads —
//! `site.hash` on the run step, a nested object. Unreachable is recorded
//! as such, in curl's own words, and never fails the converge: the site
//! is not what a train delivers.
//!
//! What each case pins: a 200 records status, byte count, the sha256 of
//! the body and the line; the request carries `Host: <site>` to the
//! address the gateway Service reports (read off the cluster, not
//! assumed); a refused connection records `unreachable (<curl's
//! words>)` with no hash and exits 0; a Service with no address records
//! unreachable without calling curl; no declared site records nothing;
//! several declared sites ride `sites` keyed by host with `site` the
//! source instance's; and both ticks — unchanged and deploying — call
//! the one function.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RUN_SUMMARY: &str = "infra/run-summary.sh";
const SITE: &str = "www.algedonic.dev";
/// The body the stub gateway answers: a trailing newline and a
/// non-ASCII byte, so a hash of anything but the bytes as received
/// would differ.
const BODY: &str = "<!doctype html>\n<title>Algedonic — BOSS</title>\n";

/// Instance rows as `render-instance.sh --instances` prints them:
/// nine tab-separated columns, the ninth the site.
fn rows(instances: &[(&str, &str, &str)]) -> String {
    instances
        .iter()
        .map(|(name, ns, site)| {
            format!("{name}\t{ns}\ttenant\tfalse\t{name}.example\t\t\t\t{site}\n")
        })
        .collect()
}

/// The stub kubectl: logs every call; answers `get svc boss-gateway`
/// with the address in STUB_ADDR (empty = no LoadBalancer address yet).
/// The stub curl: logs its argv, writes BODY to the `-o` file, prints
/// STUB_CODE — or, when STUB_CURL_FAIL is set, prints curl's own words
/// to stderr and exits 7 with nothing written.
fn stubs(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    write_exec(
        &bin.join("kubectl"),
        r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
case "$*" in
  *"get svc boss-gateway -n "*) printf '%s' "${STUB_ADDR-10.0.0.1:80}" ;;
esac
exit 0
"#,
    );
    write_exec(
        &bin.join("curl"),
        r#"#!/usr/bin/env bash
echo "curl $*" >> "$STUB_LOG"
if [ -n "${STUB_CURL_FAIL:-}" ]; then
  echo "curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused" >&2
  exit 7
fi
out=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; esac
  shift
done
printf '%s' "$STUB_BODY" > "$out"
printf '%s' "${STUB_CODE-200}"
"#,
    );
    bin
}

struct Run {
    rc: i32,
    out: String,
    err: String,
    recorded: serde_json::Value,
    calls: String,
}

/// Runs `observe_sites` from the lib with the stub kubectl as `$K`
/// and the stub curl first on PATH, recording through the real
/// run-summary.sh.
fn observe(
    name: &str,
    instances: &str,
    source_ns: &str,
    skipped: &str,
    env: &[(&str, &str)],
) -> Run {
    let dir = scratch_dir(&format!("tick-observes-site-{name}"));
    let bin = stubs(&dir);
    let script = format!(
        "set -euo pipefail\n. '{summary}'\n. '{lib}'\n\
         observe_sites '{k}' \"$STUB_INSTANCES\" '{source_ns}' \"$STUB_SKIPPED\"\n",
        summary = repo_root().join(RUN_SUMMARY).display(),
        lib = repo_root().join(LIB).display(),
        k = bin.join("kubectl").display(),
    );
    let summary = dir.join("summary.json");
    let log = dir.join("calls");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("PATH", path)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", &log)
        .env("STUB_INSTANCES", instances)
        .env("STUB_SKIPPED", skipped)
        .env("STUB_BODY", BODY)
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Run {
        rc: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).to_string(),
        err: String::from_utf8_lossy(&out.stderr).to_string(),
        recorded: std::fs::read_to_string(&summary)
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or(serde_json::Value::Null),
        calls: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

/// sha256 of BODY as `sha256sum` prints it — the same tool the lib
/// hashes with, run here on the same bytes.
fn body_sha256() -> String {
    let dir = scratch_dir("tick-observes-site-sha");
    let f = dir.join("body");
    write_file(&f, BODY);
    let out = Command::new("sha256sum").arg(&f).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn a_served_site_records_status_bytes_and_the_hash_of_the_body_as_received() {
    let r = observe("served", &rows(&[("prod", "boss", SITE)]), "boss", "", &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let site = &r.recorded["site"];
    assert!(
        site.is_object(),
        "`site` is an object the rule's event_path steps.run.site.hash can walk: {}",
        r.recorded
    );
    assert_eq!(site["host"], SITE, "{}", r.recorded);
    assert_eq!(site["namespace"], "boss", "{}", r.recorded);
    assert_eq!(site["http_status"], 200, "{}", r.recorded);
    assert_eq!(site["bytes"], BODY.len(), "{}", r.recorded);
    let sha = body_sha256();
    assert_eq!(
        site["hash"], sha,
        "the sha256 of the body bytes exactly as received: {}",
        r.recorded
    );
    assert!(
        site["observed_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z') && t.len() == 20),
        "observed_at is a UTC instant: {}",
        r.recorded
    );
    assert!(site.get("unreachable").is_none(), "{}", r.recorded);
    assert_eq!(
        r.recorded["site_line"],
        format!(
            "{SITE} → boss: HTTP 200, {} bytes, sha256 {sha}",
            BODY.len()
        ),
        "{}",
        r.recorded
    );
    assert!(
        r.recorded.get("sites").is_none(),
        "one declared site rides `site` alone: {}",
        r.recorded
    );
    // The request: the address the Service reports, the site as Host,
    // a bounded wait, and the body to a file (never a digest of a
    // string a shell has touched).
    assert!(
        r.calls.contains("kubectl get svc boss-gateway -n boss"),
        "the reach is READ off the gateway Service: {}",
        r.calls
    );
    let curl = r
        .calls
        .lines()
        .find(|l| l.starts_with("curl "))
        .expect("curl was called");
    for want in [
        "-H Host: www.algedonic.dev",
        "http://10.0.0.1:80/",
        "--max-time 10",
        "-sS",
        "-o ",
    ] {
        assert!(curl.contains(want), "the request carries `{want}`: {curl}");
    }
    assert!(
        r.out.contains(&format!(
            "cluster-deploy-runner: site: {SITE} → boss: HTTP 200"
        )),
        "{}",
        r.out
    );
}

#[test]
fn an_answer_that_is_not_200_is_still_the_answer_recorded() {
    // A tenant with no site/ yet answers 404 (a_tenant_site_per_instance):
    // the converge records what it got, hash included, and never fakes
    // a 200.
    let r = observe(
        "404",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_CODE", "404"), ("STUB_BODY", "not found\n")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(r.recorded["site"]["http_status"], 404, "{}", r.recorded);
    assert_eq!(r.recorded["site"]["bytes"], 10, "{}", r.recorded);
    assert!(
        r.recorded["site"]["hash"]
            .as_str()
            .is_some_and(|h| h.len() == 64),
        "{}",
        r.recorded
    );
}

#[test]
fn an_unreachable_site_is_recorded_in_curls_words_and_never_fails_the_converge() {
    let r = observe(
        "refused",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_CURL_FAIL", "1")],
    );
    assert_eq!(
        r.rc, 0,
        "the site is not what a train delivers — the observation never fails the tick: {}\n{}",
        r.out, r.err
    );
    let site = &r.recorded["site"];
    assert_eq!(site["host"], SITE, "{}", r.recorded);
    assert_eq!(
        site["unreachable"],
        "curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused",
        "curl's own words, verbatim: {}",
        r.recorded
    );
    for faked in ["hash", "http_status", "bytes"] {
        assert!(
            site.get(faked).is_none(),
            "nothing is faked for an answer that never came: {}",
            r.recorded
        );
    }
    assert_eq!(
        r.recorded["site_line"],
        format!(
            "{SITE} → boss: unreachable (curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused)"
        ),
        "{}",
        r.recorded
    );

    // A Service with no LoadBalancer address yet: unreachable, named,
    // and curl is never asked to guess an address.
    let r = observe(
        "no-address",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_ADDR", "")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["unreachable"],
        "no LoadBalancer address on Service boss-gateway in boss",
        "{}",
        r.recorded
    );
    assert!(
        !r.calls.contains("curl "),
        "no address, no request: {}",
        r.calls
    );

    // An instance the secret gate skipped has no gateway running: its
    // site is unreachable for the reason the packet already names.
    let r = observe(
        "skipped",
        &rows(&[
            ("prod", "boss", ""),
            ("stage", "boss-stage", "stage.example"),
        ]),
        "boss",
        "boss-stage (secrets absent: boss-secrets)",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["unreachable"], "boss-stage skipped (secrets absent)",
        "{}",
        r.recorded
    );
    assert!(!r.calls.contains("curl "), "{}", r.calls);
}

#[test]
fn no_declared_site_records_nothing() {
    let r = observe(
        "none",
        &rows(&[("prod", "boss", ""), ("playground", "boss-playground", "")]),
        "boss",
        "",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    for field in ["site", "sites", "site_line"] {
        assert!(
            r.recorded.get(field).is_none(),
            "nothing is claimed about a site nobody declared: {}",
            r.recorded
        );
    }
    assert!(!r.calls.contains("curl "), "{}", r.calls);
    assert!(
        r.out.contains("no instance declares a site"),
        "the journal says why the packet is silent: {}",
        r.out
    );
}

#[test]
fn several_declared_sites_ride_sites_by_host_and_site_is_the_sources() {
    let r = observe(
        "several",
        &rows(&[
            ("stage", "boss-stage", "stage.example"),
            ("prod", "boss", SITE),
        ]),
        "boss",
        "",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["host"], SITE,
        "`site` is the source instance's whatever the row order: {}",
        r.recorded
    );
    assert_eq!(r.recorded["site"]["namespace"], "boss", "{}", r.recorded);
    let sites = r.recorded["sites"]
        .as_object()
        .expect("`sites` keyed by host");
    assert_eq!(sites.len(), 2, "{}", r.recorded);
    assert_eq!(
        sites["stage.example"]["namespace"], "boss-stage",
        "{}",
        r.recorded
    );
    assert_eq!(sites[SITE]["http_status"], 200, "{}", r.recorded);
    let line = r.recorded["site_line"].as_str().unwrap();
    assert!(
        line.starts_with("stage.example → boss-stage: HTTP 200")
            && line.contains(&format!("; {SITE} → boss: HTTP 200")),
        "one line per instance, in row order: {line}"
    );
    assert!(
        r.calls.contains("get svc boss-gateway -n boss-stage")
            && r.calls.contains("get svc boss-gateway -n boss"),
        "{}",
        r.calls
    );
}

#[test]
fn both_ticks_observe_the_site_through_one_definition() {
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    let f = lib
        .find("observe_sites() {")
        .expect("the lib defines observe_sites");
    let body = &lib[f..f + lib[f..].find("\n}\n").expect("a body")];
    for field in ["site", "site_line"] {
        assert!(
            body.contains(&format!("run_summary_json {field} "))
                || body.contains(&format!("run_summary_field {field} ")),
            "observe_sites records `{field}`: {body}"
        );
    }
    let runner = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let unchanged_start = runner
        .find("if [ \"$HEAD\" = \"$LAST\" ]; then")
        .expect("the unchanged block");
    let unchanged_end = unchanged_start
        + runner[unchanged_start..]
            .find("_stage_started=$(date +%s)")
            .unwrap();
    let unchanged = &runner[unchanged_start..unchanged_end];
    assert!(
        unchanged.contains("observe_sites \"$K\""),
        "the unchanged tick observes the site after the connector: {unchanged}"
    );
    let stamped = runner
        .find("OUTCOME=\"converged=$HEAD\"")
        .expect("the runner marks the roll real");
    let deploying = &runner[stamped..];
    let connector = deploying
        .find("observe_connector \"$K\"")
        .expect("the deploying tick observes the connector");
    assert!(
        deploying[connector..].contains("observe_sites \"$K\""),
        "the deploying tick observes the site after the connector, its own stage: {}",
        &deploying[connector..connector + 400]
    );
    assert_eq!(
        runner.matches("observe_sites \"$K\"").count(),
        2,
        "one call per tick, no third path"
    );
}
