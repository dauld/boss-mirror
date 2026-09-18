//! A lint that scanned nothing is red — and every lint in the pre-flight
//! roster either says how many things it scanned, or is listed here with
//! the reason it is not a scanner.
//!
//! MEASURED 2026-09-18 (backlog cdf2d959, tech-debt audit H2). Two lints
//! in the roster had been exiting 0 `clean` on every gate for months
//! while examining zero things:
//!
//! | lint                 | why it scanned nothing                         |
//! |----------------------|------------------------------------------------|
//! | `sim-boundary-audit` | its awk used `\s`, which mawk reads as a       |
//! |                      | literal `s`; 0 dependency lines matched, so its |
//! |                      | 6-entry allowlist was never compared to anything|
//! | `seed-bypass-smell`  | it globbed `examples/*/seeds/sql/`, a directory |
//! |                      | retired with the playground bundles             |
//!
//! Both PRINTED the zero — "0 dep declarations scanned", "no seed sql
//! files" — and nothing read it. CLAUDE.md §Diagnosis: "a check nobody
//! reads is a check that is not running." A summary line that no reader
//! turns into a verdict is prose; this file is the reader.
//!
//! THE RULE PINNED HERE. A scanning lint prints `<lint>: scanned <N>
//! <what>` (through `infra/lint/lib/scanned.sh`) and exits 1 when N is
//! 0. This test runs every lint in the gate's own pre-flight roster
//! against the tree and reads the line back: a scanner that prints no
//! count, or a count of 0, fails BY NAME. A lint that is not a scanner —
//! a fixture-driven self-test of one script, a one-file assertion, a
//! branch-diff check that is empty on the trunk by construction — is
//! listed in [`NOT_SCANNERS`] with its reason, and an entry there naming
//! a lint that no longer exists is refused the same way a stale
//! allowlist entry is (`infra/lint/lib/allowlist.sh`). Nothing is
//! skipped silently: every lint is in exactly one of the two sets.
//!
//! The roster is asked of `infra/gate.sh --roster`, never re-derived
//! from the directory: the four lints the gate does not pre-flight
//! (live-DB sweeps, a built-binary check, the web install) are excluded
//! THERE, once, and pinned by gate_sh.rs.

use boss_testing::repo_root;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

/// Lints in the pre-flight roster that are NOT scanners, each with the
/// reason. A lint listed here is still run by the gate; it is only
/// exempt from printing a scanned count. Adding an entry is a decision
/// — write the reason as a sentence a reviewer can disagree with.
const NOT_SCANNERS: &[(&str, &str)] = &[
    // Fixture-driven self-tests of one named script: they build their own
    // world in a temp dir and exercise the script against it. Nothing in
    // the tree is enumerated, so there is no count to report.
    (
        "a-cluster-node-reports-its-headroom",
        "self-test of the estate observer against planted node readings",
    ),
    (
        "a-dead-letter-is-read-from-outside-the-jobs-api",
        "self-test of the estate observer against a planted dispatcher endpoint",
    ),
    (
        "a-dead-runner-is-settled-by-its-observer",
        "self-test of the gate-runner observer against planted Job states",
    ),
    (
        "a-failed-prepare-degrades-the-pod",
        "self-test of the tenant launcher against a planted failing publish",
    ),
    (
        "a-signature-follows-its-decision",
        "reads the source order of ONE handler in sign-off.js; self-tested on planted fixtures",
    ),
    (
        "alerts-are-packets",
        "self-test of alert-lib.sh against a planted jobs API",
    ),
    (
        "boss-gcp-converges-itself",
        "self-test of the boss-gcp converge loop against a planted checkout",
    ),
    (
        "ci-images-are-pruned-by-age",
        "self-test of the CI image prune against a planted docker daemon",
    ),
    (
        "forge-install-covers-the-ops-runner",
        "self-test of the forge installer into a scratch root",
    ),
    (
        "the-cluster-knows-it-is-working",
        "self-test of the cluster watchdog's decision table",
    ),
    (
        "the-converge-rolls-back-to-a-named-build",
        "self-test of the converge's rollback against a planted bricked head",
    ),
    (
        "the-executor-never-waits-on-its-visibility",
        "self-test of the chore wrapper against an unreachable API",
    ),
    (
        "the-journal-door-states-its-freshness",
        "self-test of journal-read.sh against planted timestamps",
    ),
    (
        "the-launcher-checks-its-own-image",
        "self-test of the services launcher's --check",
    ),
    (
        "the-mirrors-own-merge-is-not-foreign-work",
        "self-test of the mirror classifier against a planted history",
    ),
    (
        "the-observer-retains-and-replays",
        "self-test of the estate observer's spool",
    ),
    // One-file assertions: a single named file is read for a single
    // property. A count of 1 would be true and would say nothing.
    (
        "ci-checks-out-from-the-forge",
        "one-file assertion on .forgejo/workflows/ci.yml",
    ),
    (
        "session-key-persists",
        "one-field assertion on the gateway Deployment's session-key volume",
    ),
    // Branch-diff checks: what did THIS branch add, against the trunk.
    // On the trunk itself the diff is empty by construction, so zero is
    // the healthy answer and cannot be the refusal.
    (
        "a-new-style-has-a-caller",
        "branch-diff check on styles.css; empty on the trunk by construction",
    ),
    (
        "migrations-append-only",
        "branch-diff check on the migration set; empty on the trunk by construction",
    ),
    // Environment reports and live reads: their subject is not the tree.
    (
        "a-deleted-manifest-leaves-no-object",
        "reads the live cluster; its subjects are objects there, and a credential that cannot read them is stated, not counted",
    ),
    (
        "cargo-advisories",
        "report-only; soft-skips when cargo-audit is absent and always exits 0",
    ),
    (
        "workspace-declares-what-it-runs",
        "a report of what this workspace cannot run; exits 0 by design",
    ),
];

/// `<name> <path>` per line, as gate.sh prints it.
fn preflight_roster(root: &Path) -> BTreeMap<String, String> {
    let out = Command::new("bash")
        .arg(root.join("infra/gate.sh"))
        .arg("--roster")
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .expect("run infra/gate.sh --roster");
    assert!(
        out.status.success(),
        "infra/gate.sh --roster refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect()
}

/// The count a lint printed, from the first `<lint>: scanned <N>` line.
/// Read off both streams: `lib/pattern-scan.sh` prints it on stderr
/// because its stdout is the hit list the caller captures.
fn scanned_count(out: &str) -> Option<u64> {
    out.lines().find_map(|l| {
        let (_, rest) = l.split_once(": scanned ")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn run_lint(root: &Path, rel: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(root.join(rel))
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("run {rel}: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every roster lint is a scanner that printed a positive count, or is
/// listed above with a reason; and every listed name is still a lint.
#[test]
fn every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner() {
    let root = repo_root();
    let roster = preflight_roster(&root);
    assert!(
        roster.len() > 50,
        "the roster read back {} lints; gate.sh --roster is not answering",
        roster.len()
    );

    // A stale exemption is the allowlist defect this packet is about,
    // one level up: an entry excusing a lint that is gone would excuse a
    // future lint of that name from the rule without anyone deciding so.
    let stale: Vec<&str> = NOT_SCANNERS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !roster.contains_key(*name))
        .collect();
    assert!(
        stale.is_empty(),
        "NOT_SCANNERS names lints that are not in the pre-flight roster: {stale:?}. \
         Remove each entry; a lint that was deleted or moved out of pre-flight \
         does not need excusing."
    );

    // Run the scanners a few at a time: they are independent processes,
    // and the pod is shared with an operator's session.
    let scanners: Vec<(&String, &String)> = roster
        .iter()
        .filter(|(name, _)| !NOT_SCANNERS.iter().any(|(n, _)| *n == name.as_str()))
        .collect();
    let mut failures = Vec::new();
    for chunk in scanners.chunks(6) {
        let handles: Vec<_> = chunk
            .iter()
            .map(|(name, rel)| {
                let root = root.clone();
                let name = (*name).clone();
                let rel = (*rel).clone();
                std::thread::spawn(move || (name, run_lint(&root, &rel)))
            })
            .collect();
        for h in handles {
            let (name, (code, stdout, stderr)) = h.join().expect("a lint thread");
            let tail = |s: &str| {
                s.lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if code != 0 {
                failures.push(format!(
                    "{name}: exited {code} on the tree\n  stdout:\n{}\n  stderr:\n{}",
                    tail(&stdout),
                    tail(&stderr)
                ));
                continue;
            }
            match scanned_count(&format!("{stdout}\n{stderr}")) {
                Some(n) if n > 0 => {}
                Some(n) => failures.push(format!(
                    "{name}: printed `scanned {n}` and still exited 0 — lib/scanned.sh \
                     refuses a zero; this lint is printing the count by hand"
                )),
                None => failures.push(format!(
                    "{name}: printed no `scanned <N>` line. Either call lint_scanned \
                     (infra/lint/lib/scanned.sh) with the number of things it examined, \
                     or list it in NOT_SCANNERS with the reason it is not a scanner.\n  \
                     stdout tail:\n{}",
                    tail(&stdout)
                )),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} scanning lints did not prove they looked at anything:\n\n{}",
        failures.len(),
        scanners.len(),
        failures.join("\n\n")
    );
}

/// The helper itself: a positive count prints the line, a zero exits 1
/// naming the lint, a non-number exits 1 — and none of the refusals
/// reaches stdout, where a `scanned` line would be read as evidence.
#[test]
fn the_scanned_helper_refuses_a_zero_and_a_non_number() {
    let root = repo_root();
    let dir = boss_testing::scratch_dir("scanned-helper");
    let lib = root.join("infra/lint/lib/scanned.sh");
    let run = |n: &str| {
        let script = dir.join(format!("lint-{n}.sh"));
        boss_testing::write_exec(
            &script,
            &format!(
                "#!/usr/bin/env bash\n. '{}'\nlint_scanned probe-lint '{n}' 'thing(s)'\necho 'probe-lint: clean'\n",
                lib.display()
            ),
        );
        let out = Command::new("bash")
            .arg(&script)
            .output()
            .expect("run the probe");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, stdout, stderr) = run("3");
    assert_eq!(code, 0, "a positive count passes: {stderr}");
    assert_eq!(
        stdout,
        "probe-lint: scanned 3 thing(s)\nprobe-lint: clean\n"
    );
    assert_eq!(scanned_count(&stdout), Some(3));

    let (code, stdout, stderr) = run("0");
    assert_eq!(code, 1, "a zero count is refused with exit 1:\n{stderr}");
    assert!(
        !stdout.contains("clean") && !stdout.contains("scanned"),
        "a refused lint must not print `clean` or a scanned line on stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("probe-lint: scanned 0 thing(s)") && stderr.contains("cdf2d959"),
        "the refusal names the lint, the zero and the packet:\n{stderr}"
    );

    let (code, stdout, stderr) = run("");
    assert_eq!(code, 1, "an empty count is refused:\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("not a number"), "{stderr}");
}

/// The allowlist helper: a missing path and an unused entry are each
/// refused by name; a list whose every entry exists and was used passes.
#[test]
fn the_allowlist_helper_refuses_a_missing_path_and_an_unused_entry() {
    let root = repo_root();
    let dir = boss_testing::scratch_dir("allowlist-helper");
    let lib = root.join("infra/lint/lib/allowlist.sh");
    boss_testing::write_file(&dir.join("present.rs"), "fn main() {}\n");
    boss_testing::create_dir(&dir.join("present-dir"));
    let run = |body: &str| {
        let script = dir.join("probe.sh");
        boss_testing::write_exec(
            &script,
            &format!(
                "#!/usr/bin/env bash\ncd '{}'\n. '{}'\n{body}\necho 'probe-lint: clean'\n",
                dir.display(),
                lib.display()
            ),
        );
        let out = Command::new("bash")
            .arg(&script)
            .output()
            .expect("run the probe");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, stdout, stderr) =
        run("allowlist_paths_exist probe-lint present.rs present-dir/ gone.rs also-gone/");
    assert_eq!(code, 1, "a missing path is refused:\n{stderr}");
    assert!(!stdout.contains("clean"), "{stdout}");
    assert!(
        stderr.contains("gone.rs") && stderr.contains("also-gone/") && !stderr.contains("present"),
        "every missing entry is named and no present one is:\n{stderr}"
    );

    let (code, stdout, _) = run("allowlist_paths_exist probe-lint present.rs present-dir/");
    assert_eq!(code, 0, "{stdout}");

    let (code, stdout, stderr) = run(
        "used=$'present.rs\\nother.rs'\nallowlist_entries_used probe-lint \"$used\" present.rs never-tripped.rs",
    );
    assert_eq!(code, 1, "an unused entry is refused:\n{stderr}");
    assert!(!stdout.contains("clean"), "{stdout}");
    assert!(
        stderr.contains("never-tripped.rs") && !stderr.contains("present.rs"),
        "the unused entry is named and the used one is not:\n{stderr}"
    );

    let (code, stdout, _) = run(
        "used=$'present.rs\\nother.rs'\nallowlist_entries_used probe-lint \"$used\" present.rs other.rs",
    );
    assert_eq!(code, 0, "{stdout}");
}
