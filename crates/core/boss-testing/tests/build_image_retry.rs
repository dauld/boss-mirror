//! The image build must survive one dropped DNS lookup — and must not
//! survive a broken Dockerfile three times.
//!
//! THE FAILURE THIS PINS (run 248, 2026-08-26 20:59Z). `build-image`
//! died with:
//!
//! ```text
//! error building image: Get "https://production.cloudfront.docker.com/.../data":
//!   dial tcp: lookup production.cloudfront.docker.com on 127.0.0.11:53: no such host
//! ```
//!
//! kaniko had already cached the image manifest and failed fetching a
//! BLOB, so a warm cache does not protect this path. `build-image`
//! failed and locomotive, web, fast and test were skipped — a train
//! carrying six gated cars went red with no car implicated. The forge
//! has no rerun API, so the recovery was `boss train cancel`: six cars
//! back to the dock, PR closed unmerged, and roughly two hours of gate
//! time queued to be spent again.
//!
//! Measured before fixing: 10 failures in runs 199-248, and exactly ONE
//! of them had a network signature. So this is a ~2% event, and it is
//! guarded for BLAST RADIUS rather than frequency. The chain it depends
//! on has no redundancy anywhere — container 127.0.0.11 to the host's
//! systemd-resolved stub to a single upstream at 10.20.0.1, with no
//! secondary — so one dropped answer fails the build outright.
//!
//! TWO PROPERTIES, and the second is the one worth the test. Retrying
//! everything would be easy and wrong: it turns a genuine Dockerfile
//! error into three identical failures and triples the time before
//! anyone sees the real message. The harness already draws this line
//! for the database — `boss_testing::is_transient` retries a closed
//! socket and refuses to retry a rejected statement, because "a
//! rejected statement is a defect in the change under test and must
//! fail loudly on the first try; a closed socket is weather". The image
//! build gets the same rule.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn workflow() -> String {
    let path = repo_root().join(".forgejo/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The `run:` script of the `build-image` job.
///
/// Bounded by the next job at two-space indent, the same way
/// `ci-tools-declared.sh` walks this file.
fn build_image_script(workflow: &str) -> String {
    let mut out = String::new();
    let mut in_job = false;
    for line in workflow.lines() {
        if line.starts_with("  build-image:") {
            in_job = true;
            continue;
        }
        if in_job {
            // A new job at two-space indent ends this one.
            let two_space_key = line.starts_with("  ")
                && !line.starts_with("   ")
                && line.trim_end().ends_with(':');
            if two_space_key || (!line.trim().is_empty() && !line.starts_with("    ")) {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(
        !out.is_empty(),
        "no build-image job found in .forgejo/workflows/ci.yml — if the job was renamed, \
         rename it here too rather than deleting this test: the failure it guards is a red \
         train with no car at fault"
    );
    out
}

/// Signatures that mean "the network moved", not "your build is wrong".
const NETWORK_SIGNATURES: &[&str] = &[
    "no such host",
    "i/o timeout",
    "connection refused",
    "TLS handshake timeout",
    "dial tcp",
];

#[test]
fn the_image_build_retries_a_transient_network_fault() {
    let script = build_image_script(&workflow());

    assert!(
        script.contains("/kaniko/executor"),
        "build-image no longer invokes /kaniko/executor; this test is pinned to the wrong \
         thing"
    );

    let matched: Vec<&str> = NETWORK_SIGNATURES
        .iter()
        .copied()
        .filter(|sig| script.contains(sig))
        .collect();
    assert!(
        !matched.is_empty(),
        "build-image does not recognise any transient network failure. Run 248 died on \
         `no such host` fetching a blob and took a six-car train with it. Expected the step \
         to match at least one of: {NETWORK_SIGNATURES:?}"
    );
}

/// THE HALF THAT IS EASY TO GET WRONG BY BEING GENEROUS.
///
/// An unconditional `for i in 1 2 3` around the executor would satisfy
/// the test above and make every real build error take three times as
/// long to report, three times as noisily. The retry has to be
/// conditional on the failure looking transient.
#[test]
fn a_build_error_is_not_retried() {
    let script = build_image_script(&workflow());

    assert!(
        script.contains("not retrying") || script.contains("not a network"),
        "build-image must say, in the log, when it declines to retry — a silent \
         non-retry is indistinguishable from a retry that did not happen. Expected the \
         step to print a 'not retrying' line on a non-transient failure."
    );

    // The cap has to exist and has to be small: an unbounded retry on a
    // resolver that is genuinely down burns the runner instead of
    // failing.
    let has_cap = (1..=5).any(|n| script.contains(&format!("MAX_ATTEMPTS={n}")));
    assert!(
        has_cap,
        "build-image must declare a small MAX_ATTEMPTS (1-5). An unbounded retry against a \
         resolver that is actually down occupies the single CI runner instead of failing."
    );
}
