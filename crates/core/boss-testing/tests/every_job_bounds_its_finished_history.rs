//! EVERY JOB AND CRONJOB THE TREE DECLARES SAYS HOW LONG ITS FINISHED
//! HISTORY LIVES — IN ITS OWN MANIFEST, WITHIN A BOUND.
//!
//! Measured 2026-09-23 by the builder of 85889a52 (backlog 20f85776):
//! Failed pods owned by Jobs were listed in both namespaces — 13 in
//! boss-dev (gate-fix-*, gate-train-*, and a `seed-dir-probe` Job pod
//! from 2026-09-12) and 16 in boss (the CronJob pods of audit-integrity,
//! conservation-invariants, estate-observe, search-reindex, views-catchup
//! and messages-events-purge). The pod reap that car built
//! (`infra/forge/reap-terminated-pods.sh`) leaves a Job's pods to the
//! Job BY DESIGN — deleting one under a Job races the Job — so a Job's
//! own retention is the only thing that ever removes them, and nothing
//! checked that every Job declares one.
//!
//! Re-measured 2026-09-24 ~11:45Z, read-only, the day this pin landed:
//! every declared Job and CronJob already bounds itself. The gate Jobs
//! carry `ttlSecondsAfterFinished: 86400` and none of the 250+ live ones
//! was older than that; each of the ten CronJobs in `boss` keeps 3
//! failed Jobs, and its 16 failed pods are exactly those — the oldest
//! (audit-integrity, 2026-08-25) is old because that chore has not
//! failed since, and keeping the LAST failure's pod is the point of a
//! failure history: its log is the only copy of why it failed. The one
//! unbounded Job was `seed-dir-probe-2`, which no manifest declares — a
//! Job created by hand, which no manifest test can see.
//!
//! So what was missing was not a value but the mechanism: a new Job
//! manifest without a TTL would have gone green and lived forever. This
//! is that mechanism. It reads every YAML file under `infra/` (where
//! every Kubernetes manifest in the tree lives), so a Job added in a new
//! file or a new directory is read without anyone listing it.
//!
//! THE BOUNDS, AND WHY EACH HAS A FLOOR AS WELL AS A CEILING:
//!   * a Job: `ttlSecondsAfterFinished` between one hour and seven days.
//!     The ceiling is the pile-up. The floor is the evidence: a Job that
//!     is deleted the moment it fails takes its pod log with it, and the
//!     gate's own "the Job finished but the packet never reported"
//!     message sends the reader to `kubectl logs job/<job>`, which holds
//!     the receipt for exactly as long as this TTL.
//!   * a CronJob: `successfulJobsHistoryLimit` and
//!     `failedJobsHistoryLimit`, both explicit, both at most 10, and the
//!     failed one at least 1 — same reason: zero would reap the only
//!     record of the last failure. Kubernetes defaults both (3 and 1),
//!     so an omitted limit is not unbounded; it is required anyway so
//!     the retention is read where the CronJob is read, not recalled
//!     from the API reference.
//!   A CronJob's Jobs need no TTL of their own: the history limits own
//!   them, and a TTL under them would delete a kept failure early.
//!
//! Hand-scanned rather than YAML-parsed, as `gate_runner_manifests.rs`
//! is: the workspace carries no YAML parser, and the structure read here
//! is two levels deep.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// One hour: long enough to read a failed Job's log after it failed.
const MIN_JOB_TTL_S: u64 = 60 * 60;
/// Seven days: past this a finished Job is a pile, not a record.
const MAX_JOB_TTL_S: u64 = 7 * 24 * 60 * 60;
/// A CronJob keeps at most this many finished Jobs of each outcome.
const MAX_HISTORY: u64 = 10;

/// Every `.yaml` / `.yml` file under `dir`, sorted, so a finding names
/// the same file on every run.
fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            std::fs::read_dir(&d).unwrap_or_else(|e| panic!("reading {}: {e}", d.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml")
            ) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// What the check found wrong with one document, as sentences naming it.
/// Empty for a bounded Job or CronJob and for every other kind.
fn findings(file: &str, doc: &str) -> Vec<String> {
    let kind = top_level(doc, "kind").unwrap_or("");
    let who = format!("{file}: {kind}/{}", name(doc));
    // (the field, its floor, its ceiling, why the floor exists)
    let bounds: &[(&str, u64, u64, &str)] = match kind {
        "Job" => &[(
            "ttlSecondsAfterFinished",
            MIN_JOB_TTL_S,
            MAX_JOB_TTL_S,
            "a Job deleted as it fails takes its pod log with it",
        )],
        "CronJob" => &[
            (
                "successfulJobsHistoryLimit",
                0,
                MAX_HISTORY,
                "a successful run is not evidence",
            ),
            (
                "failedJobsHistoryLimit",
                1,
                MAX_HISTORY,
                "zero reaps the only record of the last failure",
            ),
        ],
        _ => &[],
    };
    bounds
        .iter()
        .filter_map(|&(key, lo, hi, floor_why)| match field(doc, "spec", key) {
            None => Some(format!(
                "{who} declares no spec.{key} — set it between {lo} and {hi}"
            )),
            Some(v) => match v.parse::<u64>() {
                Ok(n) if (lo..=hi).contains(&n) => None,
                Ok(n) if n < lo => {
                    Some(format!("{who} spec.{key} is {n}, below {lo}: {floor_why}"))
                }
                Ok(n) => Some(format!(
                    "{who} spec.{key} is {n}, above {hi}: that is the pile-up this bound exists for"
                )),
                Err(_) => Some(format!(
                    "{who} spec.{key} is `{v}`, not a literal integer this check can bound"
                )),
            },
        })
        .collect()
}

/// The (kind, file, name) of every Job and CronJob document under
/// `infra/`, with the findings of each.
struct Scan {
    declared: Vec<(String, String, String)>,
    findings: Vec<String>,
}

fn scan_infra() -> Scan {
    let root = repo_root();
    let mut declared = Vec::new();
    let mut all = Vec::new();
    for path in yaml_files(&root.join("infra")) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for doc in documents(&text) {
            let Some(kind) = top_level(&doc, "kind") else {
                continue;
            };
            if kind == "Job" || kind == "CronJob" {
                declared.push((kind.to_string(), rel.clone(), name(&doc)));
                all.extend(findings(&rel, &doc));
            }
        }
    }
    Scan {
        declared,
        findings: all,
    }
}

/// A multi-document file split on its `---` separators.
fn documents(text: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for line in text.lines() {
        if line.trim_end() == "---" {
            out.push(String::new());
        } else if let Some(last) = out.last_mut() {
            last.push_str(line);
            last.push('\n');
        }
    }
    out
}

/// The value of a key at column 0 (`kind: Job` -> `Job`).
fn top_level<'a>(doc: &'a str, key: &str) -> Option<&'a str> {
    doc.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .map(scalar)
}

/// A scalar with any trailing comment and surrounding quotes removed.
fn scalar(v: &str) -> &str {
    let v = v.split(" #").next().unwrap_or(v).trim();
    v.trim_matches(|c| c == '"' || c == '\'')
}

/// The value of `key` one level under the column-0 block `block`
/// (`spec:` -> `  ttlSecondsAfterFinished: 86400`). Deeper keys of the
/// same name — a CronJob's `jobTemplate.spec` — are NOT this key.
fn field<'a>(doc: &'a str, block: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            inside = trimmed.strip_prefix(block) == Some(":");
            continue;
        }
        if inside
            && indent == 2
            && let Some(v) = trimmed.strip_prefix(key).and_then(|r| r.strip_prefix(':'))
        {
            return Some(scalar(v));
        }
    }
    None
}

/// `metadata.name`, or the `generateName` prefix a gate Job uses.
fn name(doc: &str) -> String {
    field(doc, "metadata", "name")
        .or_else(|| field(doc, "metadata", "generateName"))
        .unwrap_or("<unnamed>")
        .to_string()
}

/// THE PIN. Every Job and CronJob under `infra/` bounds its history.
#[test]
fn every_job_and_cronjob_the_tree_declares_bounds_its_finished_history() {
    let scan = scan_infra();
    assert!(
        scan.findings.is_empty(),
        "{} finding(s) — a finished Job or CronJob run must say, in its own manifest, how long \
         it stays; the pod reap leaves Job-owned pods to their Job (backlog 20f85776):\n  {}",
        scan.findings.len(),
        scan.findings.join("\n  ")
    );
}

/// A scan that finds nothing passes on nothing. The gate Job is the one
/// that measured the pile-up, and it is a `generateName` template in a
/// directory of its own, so it is named here: if the walk stopped
/// reaching `infra/gate-runner/`, or the document split stopped seeing
/// the Job behind its RBAC, this fails instead of passing emptily.
#[test]
fn the_scan_reads_the_gate_job_and_the_cronjobs() {
    let scan = scan_infra();
    let has = |kind: &str, file: &str| scan.declared.iter().any(|(k, f, _)| k == kind && f == file);
    assert!(
        has("Job", "infra/gate-runner/gate-runner.yaml"),
        "the scan did not see the gate Job; it saw {:?}",
        scan.declared
    );
    assert!(
        has(
            "CronJob",
            "infra/cluster/manifests/boss-audit-integrity.yaml"
        ),
        "the scan did not see the audit-integrity CronJob; it saw {:?}",
        scan.declared
    );
}

const BOUNDED_JOB: &str = "\
apiVersion: batch/v1
kind: Job
metadata:
  name: once
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 86400 # a day, for the log
  template:
    spec:
      restartPolicy: Never
";

const BOUNDED_CRONJOB: &str = "\
apiVersion: batch/v1
kind: CronJob
metadata:
  name: nightly
spec:
  schedule: \"30 3 * * *\"
  successfulJobsHistoryLimit: 3
  failedJobsHistoryLimit: 3
  jobTemplate:
    spec:
      backoffLimit: 0
";

#[test]
fn a_bounded_job_and_cronjob_have_no_findings() {
    assert_eq!(findings("f.yaml", BOUNDED_JOB), Vec::<String>::new());
    assert_eq!(findings("f.yaml", BOUNDED_CRONJOB), Vec::<String>::new());
    assert_eq!(
        findings("f.yaml", "kind: Deployment\nmetadata:\n  name: d\n"),
        Vec::<String>::new()
    );
}

/// The shape the pile-up needs: a Job with no TTL lives until someone
/// deletes it, and so do its pods.
#[test]
fn a_job_without_a_ttl_is_refused_by_name() {
    let doc = BOUNDED_JOB.replace(
        "  ttlSecondsAfterFinished: 86400 # a day, for the log\n",
        "",
    );
    let f = findings("infra/x.yaml", &doc);
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(
        f[0].contains("infra/x.yaml") && f[0].contains("Job/once"),
        "{f:?}"
    );
    assert!(f[0].contains("ttlSecondsAfterFinished"), "{f:?}");
}

/// A TTL on the pod template, or anywhere but the Job's own spec, is not
/// the Job's TTL — Kubernetes ignores it there.
#[test]
fn a_ttl_below_the_jobs_own_spec_does_not_count() {
    let doc = BOUNDED_JOB
        .replace(
            "  ttlSecondsAfterFinished: 86400 # a day, for the log\n",
            "",
        )
        .replace(
            "      restartPolicy: Never\n",
            "      restartPolicy: Never\n      ttlSecondsAfterFinished: 60\n",
        );
    assert_eq!(findings("f.yaml", &doc).len(), 1);
}

#[test]
fn a_ttl_outside_the_bounds_is_refused() {
    for (ttl, why) in [
        ("0", "deleted before anyone reads its log"),
        ("60", "deleted before anyone reads its log"),
        ("2592000", "thirty days is a pile"),
        ("forever", "not a number"),
    ] {
        let doc = BOUNDED_JOB.replace("86400", ttl);
        let f = findings("f.yaml", &doc);
        assert_eq!(f.len(), 1, "ttl {ttl} ({why}) must be refused: {f:?}");
    }
    let edge = BOUNDED_JOB.replace("86400", &MIN_JOB_TTL_S.to_string());
    assert!(findings("f.yaml", &edge).is_empty());
    let edge = BOUNDED_JOB.replace("86400", &MAX_JOB_TTL_S.to_string());
    assert!(findings("f.yaml", &edge).is_empty());
}

#[test]
fn a_cronjob_must_state_both_limits() {
    let doc = BOUNDED_CRONJOB
        .replace("  successfulJobsHistoryLimit: 3\n", "")
        .replace("  failedJobsHistoryLimit: 3\n", "");
    let f = findings("f.yaml", &doc);
    assert_eq!(f.len(), 2, "{f:?}");
    assert!(f.iter().any(|s| s.contains("successfulJobsHistoryLimit")));
    assert!(f.iter().any(|s| s.contains("failedJobsHistoryLimit")));
    assert!(f.iter().all(|s| s.contains("CronJob/nightly")), "{f:?}");
}

/// Zero failed Jobs kept reaps the only record of the last failure;
/// more than the ceiling is the pile-up again.
#[test]
fn a_cronjob_limit_outside_the_bounds_is_refused() {
    let zero_failed =
        BOUNDED_CRONJOB.replace("failedJobsHistoryLimit: 3", "failedJobsHistoryLimit: 0");
    assert_eq!(findings("f.yaml", &zero_failed).len(), 1);
    let many = BOUNDED_CRONJOB.replace(
        "successfulJobsHistoryLimit: 3",
        "successfulJobsHistoryLimit: 50",
    );
    assert_eq!(findings("f.yaml", &many).len(), 1);
    let zero_ok = BOUNDED_CRONJOB.replace(
        "successfulJobsHistoryLimit: 3",
        "successfulJobsHistoryLimit: 0",
    );
    assert!(
        findings("f.yaml", &zero_ok).is_empty(),
        "keeping no SUCCESSFUL runs loses no evidence"
    );
    let at_max = BOUNDED_CRONJOB.replace(
        "failedJobsHistoryLimit: 3",
        &format!("failedJobsHistoryLimit: {MAX_HISTORY}"),
    );
    assert!(findings("f.yaml", &at_max).is_empty());
}

/// The limits belong on the CronJob's spec; a copy under `jobTemplate`
/// is a field Kubernetes does not read.
#[test]
fn a_cronjob_limit_under_the_job_template_does_not_count() {
    let doc = BOUNDED_CRONJOB
        .replace("  failedJobsHistoryLimit: 3\n", "")
        .replace(
            "      backoffLimit: 0\n",
            "      backoffLimit: 0\n      failedJobsHistoryLimit: 3\n",
        );
    assert_eq!(findings("f.yaml", &doc).len(), 1);
}

#[test]
fn documents_split_on_separators_and_keys_read_at_their_level() {
    let text = format!("kind: ServiceAccount\n---\n{BOUNDED_JOB}---\n{BOUNDED_CRONJOB}");
    let docs = documents(&text);
    assert_eq!(docs.len(), 3);
    assert_eq!(top_level(&docs[1], "kind"), Some("Job"));
    assert_eq!(name(&docs[2]), "nightly");
    assert_eq!(
        field(&docs[1], "spec", "ttlSecondsAfterFinished"),
        Some("86400")
    );
    assert_eq!(field(&docs[2], "spec", "backoffLimit"), None);
}
