//! `ci-image-report` (`infra/forge/ci-image-report.sh`) — the read verb
//! the image-freshness sweep files for itself, and the ONE verdict line
//! it ends with, in the shape every sweep report shares
//! (`sweep_report_verdicts_sh.rs`):
//!
//!     verdict: clean
//!     verdict: image_stale local=<digest-prefix> registry=<digest-prefix> age=<h>h
//!     verdict: unanswered (...)
//!
//! WHY (backlog 18df96c4, left by the builder of 970c0c94 on
//! 2026-09-18). The sweep's measure rule filed `disk-report` — a copy
//! of disk-headroom's — whose verdict is the disk floor and says
//! nothing about image age, so its Inspect step could never be judged
//! and sat on the agent. The question the daily rule's `why` records
//! is a RUNNER-LOCAL boss-ci tag older than the registry's: a registry
//! retag does not refresh the runner's local floating tag, and on forge
//! train #1 the runner ran a 31-hour-old image while the registry
//! served a newer one. So the verb compares the system daemon's
//! `boss-ci:<floating tag>` repo digest against the digest the registry
//! serves for that tag, and reports the local image's age (from
//! `{{.Created}}`, the way prune-ci-images.lib.sh reads it) when they
//! differ.
//!
//! The script is RUN here: `docker` (the system daemon) and `curl` (the
//! registry) are stubs the script's own env knobs name, so every
//! verdict below is one the script actually printed, and nothing here
//! touches a daemon or the LAN.

use boss_testing::repo_root;
use boss_testing::scratch::{scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/ci-image-report.sh";
const REPO: &str = "10.20.0.15:3000/david/boss-ci";
const LOCAL: &str = "sha256:1bee89bbb5df89f27837ece77db52887ad4a7421db2aefc19587595883928823";
const NEWER: &str = "sha256:9c0ffee0ffee0ffee0ffee0ffee0ffee0ffee0ffee0ffee0ffee0ffee0ffee00";

/// The last `verdict:` line, the way `maintenance.sweep.judge` reads it.
fn verdict_of(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("verdict: "))
}

fn rfc3339_hours_ago(hours: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{}", now - hours * 3600),
            "+%Y-%m-%dT%H:%M:%S.000000000Z",
        ])
        .output()
        .expect("date runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Report {
    dir: PathBuf,
    /// `None`: the daemon holds no floating tag at all.
    local: Option<(String, &'static str)>,
    /// What the registry serves for the tag; `None`: curl fails.
    registry: Option<&'static str>,
    /// The daemon cannot be read at all (`sudo -n docker` refused).
    daemon_dark: bool,
}

impl Report {
    fn new(case: &str) -> Self {
        Report {
            dir: scratch_dir(&format!("ci-image-report-{case}")),
            local: Some((rfc3339_hours_ago(31), LOCAL)),
            registry: Some(LOCAL),
            daemon_dark: false,
        }
    }

    fn run(&self) -> (i32, String) {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");

        // The system daemon: a listing of per-train sha tags with their
        // ages (the context the report prints above its verdict), and
        // the floating tag's creation time + repo digest.
        let listing = self.dir.join("listing");
        write_file(
            &listing,
            "aaaa1111 11ab11ab11ab11ab11ab11ab11ab11ab11ab11ab\nbbbb2222 rust1.96\ncccc3333 latest\n",
        );
        let meta = self.dir.join("meta");
        write_file(
            &meta,
            &format!(
                "aaaa1111 {} 3000000000\nbbbb2222 {} 3000000000\ncccc3333 {} 100\n",
                rfc3339_hours_ago(2),
                rfc3339_hours_ago(31),
                rfc3339_hours_ago(400)
            ),
        );
        let floating = self.dir.join("floating");
        match &self.local {
            Some((created, digest)) => {
                write_file(&floating, &format!("{created} {REPO}@{digest}\n"))
            }
            None => {
                let _ = std::fs::remove_file(&floating);
            }
        }
        let docker_calls = self.dir.join("docker-calls");
        write_exec(
            &bin.join("boss-stub-docker"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >>{calls}\n\
                 {dark}\n\
                 case \"$1\" in\n\
                 info) echo /var/lib/docker; exit 0 ;;\n\
                 images) cat {listing}; exit 0 ;;\n\
                 image) shift; [ \"$1\" = inspect ] || exit 1; ref=\"${{!#}}\";\n\
                   case \"$ref\" in\n\
                   *:*) [ -r {floating} ] || {{ echo 'Error: No such image' >&2; exit 1; }}; cat {floating}; exit 0 ;;\n\
                   esac\n\
                   line=\"$(grep \"^$ref \" {meta})\" || exit 1; printf '%s\\n' \"${{line#* }}\"; exit 0 ;;\n\
                 esac\n\
                 exit 127\n",
                calls = docker_calls.display(),
                dark = if self.daemon_dark { "exit 1" } else { ":" },
                listing = listing.display(),
                meta = meta.display(),
                floating = floating.display(),
            ),
        );

        // The registry, over the anonymous pull-token flow the forge
        // answers (verified from the pod 2026-09-18: /v2/ is 401 with a
        // Bearer realm, the token endpoint hands out a token with no
        // credentials, and the tag's manifest HEAD carries
        // Docker-Content-Digest). The stub answers by URL and honours
        // `-D` / `-o` the way curl does.
        let curl_calls = self.dir.join("curl-calls");
        write_exec(
            &bin.join("boss-stub-curl"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >>{calls}\n\
                 hdr=/dev/null; body=/dev/null; url=\"\"\n\
                 while [ $# -gt 0 ]; do\n\
                   case \"$1\" in\n\
                     -D) hdr=\"$2\"; shift ;;\n\
                     -o) body=\"$2\"; shift ;;\n\
                     http://*) url=\"$1\" ;;\n\
                   esac\n\
                   shift\n\
                 done\n\
                 {fail}\n\
                 case \"$url\" in\n\
                   */v2/) printf 'HTTP/1.1 401 Unauthorized\\r\\nWww-Authenticate: Bearer realm=\"http://10.20.0.15:3000/v2/token\",service=\"container_registry\",scope=\"*\"\\r\\n\\r\\n' >\"$hdr\"; printf '401' ;;\n\
                   */v2/token*) printf '{{\"token\":\"anon\"}}' >\"$body\"; printf 'HTTP/1.1 200 OK\\r\\n\\r\\n' >\"$hdr\"; printf '200' ;;\n\
                   */manifests/*) printf 'HTTP/1.1 200 OK\\r\\nContent-Type: application/vnd.oci.image.manifest.v1+json\\r\\nDocker-Content-Digest: {digest}\\r\\n\\r\\n' >\"$hdr\"; printf '200' ;;\n\
                   *) printf '000'; exit 3 ;;\n\
                 esac\n\
                 exit 0\n",
                calls = curl_calls.display(),
                fail = if self.registry.is_none() {
                    "echo 'curl: (7) Failed to connect' >&2; printf '000'; exit 7"
                } else {
                    ":"
                },
                digest = self.registry.unwrap_or(""),
            ),
        );

        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .env_clear()
            .env("PATH", path)
            .env("BOSS_CI_IMAGE_REPO", REPO)
            .env("BOSS_CI_IMAGE_DOCKER", bin.join("boss-stub-docker"))
            .env("BOSS_CI_REGISTRY_CURL", bin.join("boss-stub-curl"))
            .output()
            .expect("ci-image-report.sh runs");
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), all)
    }

    fn curl_calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("curl-calls")).unwrap_or_default()
    }
}

/// The runner's floating tag IS what the registry serves: clean, exit 0.
#[test]
fn a_local_tag_that_matches_the_registrys_digest_reads_clean() {
    let r = Report::new("clean");
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "an answered report exits 0:\n{all}");
    assert_eq!(verdict_of(&all), Some("verdict: clean"), "{all}");
    // The registry was actually asked: the token flow, then the tag.
    let calls = r.curl_calls();
    assert!(
        calls.contains("/v2/token") && calls.contains("/manifests/rust1.96"),
        "the registry must be read through the token flow:\n{calls}"
    );
    assert!(
        calls.contains("Bearer anon"),
        "the manifest read must carry the pull token:\n{calls}"
    );
}

/// THE MEASURED DRIFT: the registry serves a newer digest under the
/// same tag and the runner still holds a 31-hour-old image. The finding
/// names both digests (a prefix each, the way `docker images` names an
/// image) and the local image's age, so the inspector reads the gap off
/// the step; the exit code is still 0 — a finding is an answer.
#[test]
fn a_local_tag_behind_the_registry_reads_stale_with_both_digests_and_its_age() {
    let mut r = Report::new("stale");
    r.registry = Some(NEWER);
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "a finding is an answer, not a failure:\n{all}");
    assert_eq!(
        verdict_of(&all),
        Some("verdict: image_stale local=1bee89bbb5df registry=9c0ffee0ffee age=31h"),
        "{all}"
    );
}

/// A tag the runner has never pulled cannot be served stale (ci.yml:
/// "a per-commit tag cannot be served stale — the runner has never
/// seen it"). No local floating tag is clean, and the report says why.
#[test]
fn no_local_floating_tag_is_clean_because_nothing_can_be_served_stale() {
    let mut r = Report::new("absent");
    r.local = None;
    r.registry = Some(NEWER);
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "{all}");
    assert_eq!(verdict_of(&all), Some("verdict: clean"), "{all}");
    assert!(
        all.contains("holds no") && all.contains("rust1.96"),
        "the report must say the runner holds no local floating tag:\n{all}"
    );
}

/// A registry that cannot be read is NOT a fresh image: the verdict says
/// the question went unanswered and names the reason, which the judging
/// rule reads as a finding and leaves for the inspector.
#[test]
fn an_unreadable_registry_is_unanswered_never_clean() {
    let mut r = Report::new("registry-dark");
    r.registry = None;
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "{all}");
    let v = verdict_of(&all).unwrap_or_else(|| panic!("no verdict line:\n{all}"));
    assert!(
        v.starts_with("verdict: unanswered"),
        "an unreadable registry must read as unanswered, never clean: {v}\n{all}"
    );
    assert!(v.contains("registry"), "the reason names the registry: {v}");
}

/// A daemon that cannot be read (`sudo -n docker` refused, or the wrong
/// daemon) is likewise unanswered — a prune aimed at the wrong daemon
/// reports success and frees nothing (prune-ci-images.lib.sh), and a
/// report aimed at it would read clean about an image it never saw.
#[test]
fn an_unreadable_daemon_is_unanswered_never_clean() {
    let mut r = Report::new("daemon-dark");
    r.daemon_dark = true;
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "{all}");
    let v = verdict_of(&all).unwrap_or_else(|| panic!("no verdict line:\n{all}"));
    assert!(
        v.starts_with("verdict: unanswered"),
        "an unreadable daemon must read as unanswered, never clean: {v}\n{all}"
    );
    assert!(
        v.contains("docker"),
        "the reason names the daemon read: {v}"
    );
}

/// The verb file runs this script, on the forge, with no parameters —
/// declared the way disk-report is (infra/ops/verbs/README.md).
#[test]
fn the_ci_image_report_verb_runs_the_script_read_only_on_the_forge() {
    let path = repo_root().join("infra/ops/verbs/ci-image-report.json");
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the verb file {} is readable: {e}", path.display())),
    )
    .expect("the verb file is JSON");
    assert_eq!(verb["argv"][0].as_str(), Some(SCRIPT), "{verb}");
    assert_eq!(
        verb["argv"].as_array().map(Vec::len),
        Some(1),
        "no arguments: {verb}"
    );
    assert_eq!(verb["params"].as_array().map(Vec::len), Some(0), "{verb}");
    assert_eq!(verb["hosts"], serde_json::json!(["forge"]), "{verb}");
    let about = verb["about"].as_str().unwrap_or_default();
    assert!(
        about.starts_with("READ-ONLY") && !about.contains("MUTATING"),
        "a report verb declares itself read-only: {about}"
    );
    assert!(
        Path::new(&repo_root().join(SCRIPT)).exists(),
        "the verb's script must be in the tree"
    );
}
