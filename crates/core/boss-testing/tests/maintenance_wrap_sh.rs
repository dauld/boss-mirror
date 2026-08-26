//! `infra/boss-maintenance-wrap.sh` is the visibility half of every
//! nightly chore: the timer executes, this script leaves the Job that
//! makes the run visible and overdue if it never happens.
//!
//! THE FAILURE THIS PINS (2026-08-25 and 2026-08-26). The jobs API runs
//! as a single replica, so every deploy opens a window with no ready
//! pod. `boss-search-reindex` fired into that window twice and died at
//! the first `curl` in two milliseconds — `curl: (7) ... Couldn't
//! connect to server` — which under `set -e` ended the run.
//!
//! That is worse than an ordinary failed chore because of the loop in
//! it: the script exists to record that work did not happen, and the
//! write that records it goes through the very service that is down. An
//! outage here erases its own evidence, so nothing is overdue, nothing
//! alarms, and a dark check is indistinguishable from a passing one.
//!
//! Two properties hold the fix, and the second matters more than the
//! first because it is the one a well-meaning edit would undo:
//!
//! - the READ retries past a refused connection, waiting the rollout
//!   window out;
//! - the WRITE does not retry at all. `curl --retry` also retries 5xx,
//!   and a 5xx arriving after the server created the Job would leave
//!   two open packets for one chore — breaking the single-open contract
//!   the script is built around. The read runs first and must succeed,
//!   so it already serves as the readiness gate the write needs.
//!
//! Both tests name what they found when they fail.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn script() -> String {
    let path = repo_root().join("infra/boss-maintenance-wrap.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every `curl` invocation in the script, each flattened to one line so
/// a flag can be found regardless of where the author wrapped it.
///
/// Comment lines are skipped: the script explains this exact failure in
/// prose above the call, and prose that quotes `curl` must not be read
/// as an invocation.
fn curl_invocations(script: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;

    for line in script.lines() {
        let is_comment = line.trim_start().starts_with('#');
        let continues = line.trim_end().ends_with('\\');
        let piece = line.trim().trim_end_matches('\\').trim();

        match current {
            None => {
                if is_comment {
                    continue;
                }
                if let Some(idx) = line.find("curl ") {
                    let start = line[idx..].trim_end_matches('\\').trim().to_string();
                    if continues {
                        current = Some(start);
                    } else {
                        found.push(start);
                    }
                }
            }
            Some(ref mut acc) => {
                acc.push(' ');
                acc.push_str(piece);
                if !continues {
                    found.push(current.take().expect("in a continuation"));
                }
            }
        }
    }
    if let Some(acc) = current {
        found.push(acc);
    }
    found
}

fn read_call(calls: &[String]) -> String {
    calls
        .iter()
        .find(|c| c.contains("status=open") && !c.contains("-X POST"))
        .unwrap_or_else(|| {
            panic!("no open-Job read found among {} curl calls: {calls:#?}", calls.len())
        })
        .clone()
}

fn write_call(calls: &[String]) -> String {
    calls
        .iter()
        .find(|c| c.contains("-X POST"))
        .unwrap_or_else(|| panic!("no Job-creating POST found among {} curl calls", calls.len()))
        .clone()
}

/// The read must survive the deploy window that took search-reindex out.
///
/// `--retry-connrefused` is named specifically, not just `--retry`:
/// plain `--retry` covers timeouts and 5xx but treats a REFUSED
/// connection as a hard failure, and refused is precisely the error the
/// chore died on. A fix that added only `--retry` would look right and
/// still fail identically.
#[test]
fn the_open_job_read_waits_out_a_restarting_jobs_api() {
    let calls = curl_invocations(&script());
    let read = read_call(&calls);

    assert!(
        read.contains("--retry-connrefused"),
        "the open-Job read must retry past a REFUSED connection — a restarting \
         jobs API is the documented failure (search-reindex, 2026-08-25/26) and \
         plain --retry does not cover it. Found: {read}"
    );
    assert!(
        read.contains("--retry "),
        "--retry-connrefused only takes effect alongside --retry <n>. Found: {read}"
    );
}

/// THE HALF THAT IS EASY TO BREAK BY BEING HELPFUL.
///
/// Adding the same retry flags to the POST is the obvious next edit and
/// it is wrong: `curl --retry` retries 5xx as well, so a 5xx returned
/// after the Job row was written produces a second open Job for one
/// chore. The single-open contract is what lets a failed run be
/// recovered by the next one instead of piling up.
#[test]
fn the_job_creating_write_is_not_retried() {
    let calls = curl_invocations(&script());
    let write = write_call(&calls);

    assert!(
        !write.contains("--retry"),
        "the Job-creating POST must NOT retry: curl --retry also retries 5xx, and a \
         5xx after the server already created the Job leaves two open packets for one \
         chore. The read above is the readiness gate. Found: {write}"
    );
}
