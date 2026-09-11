//! `infra/ml/run-inference-batch.sh` must be BEST-EFFORT per model.
//!
//! The daily ML infer-batch trigger iterates a list of independent
//! models and POSTs infer-batch on each. It ran the loop under
//! `set -euo pipefail` with a bare `curl -sSf`, so the FIRST model
//! whose batch failed aborted the loop — every model after it was
//! skipped and left its predictions stale, silently, until someone
//! read the journal. One flaky model took out the rest of the run.
//!
//! The fix makes each model best-effort: a failure is logged and the
//! loop continues, and the script exits non-zero at the end if any
//! model failed, so systemd (Type=oneshot) and the maintenance packet
//! still record the degraded run.
//!
//! The ONE thing best-effort must NOT break is the single real
//! ordering dependency the file documents: high-churn-risk reads what
//! account-churn-risk writes, so if churn-risk fails, high-churn-risk
//! must be SKIPPED (not run on stale input) — while the other
//! independent models still run.
//!
//! These tests drive the real script with a fake `curl` (injected via
//! `BOSS_ML_CURL`, the script's test seam) that records which model
//! ids it was asked to infer and fails for a chosen set. The same
//! shape as `gate_sh.rs` overriding `df` via `BOSS_GATE_DF_CMD`.

use boss_testing::repo_root;

const SCRIPT: &str = "infra/ml/run-inference-batch.sh";

/// The full ordered model list the script iterates, as the source of
/// truth for what a full run should touch. Kept in sync by
/// `the_model_list_and_dependency_comment_survive` below.
const ALL_MODELS: &[&str] = &[
    "mdl-account-churn-risk-v1",
    "mdl-next-action-contract-expiring-v1",
    "mdl-next-action-past-due-invoice-v1",
    "mdl-next-action-missing-primary-contact-v1",
    "mdl-next-action-high-churn-risk-v1",
    "mdl-next-action-stalled-service-ticket-v1",
    "mdl-next-action-preventive-maintenance-due-v1",
];

const CHURN_MODEL: &str = "mdl-account-churn-risk-v1";
const DEPENDENT_MODEL: &str = "mdl-next-action-high-churn-risk-v1";

struct RunOutcome {
    success: bool,
    stdout: String,
    stderr: String,
    /// The model ids the fake curl was actually invoked with, in order.
    invoked: Vec<String>,
}

/// Drive the real script with a fake curl. `fail` lists the model ids
/// whose infer-batch should fail (curl exit 22, an HTTP error).
fn run_with_failures(fail: &[&str]) -> RunOutcome {
    use std::os::unix::fs::PermissionsExt;

    let root = repo_root();
    let dir = std::env::temp_dir().join(format!(
        "boss-infer-batch-{}-{:?}",
        std::process::id(),
        fail
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir tmp");

    let log = dir.join("invoked.log");
    let fake = dir.join("curl");
    // Fake curl: last arg is the URL; extract the model id between
    // `/models/` and `/infer-batch`, record it, then fail if it is in
    // FAKE_CURL_FAIL (space separated).
    std::fs::write(
        &fake,
        "#!/usr/bin/env bash\n\
         url=\"\"\n\
         for a in \"$@\"; do url=\"$a\"; done\n\
         id=\"${url##*/models/}\"\n\
         id=\"${id%%/*}\"\n\
         echo \"$id\" >> \"$FAKE_CURL_LOG\"\n\
         for f in $FAKE_CURL_FAIL; do\n\
           if [ \"$f\" = \"$id\" ]; then exit 22; fi\n\
         done\n\
         exit 0\n",
    )
    .expect("write fake curl");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let out = std::process::Command::new("bash")
        .arg(root.join(SCRIPT))
        .env("BOSS_ML_CURL", &fake)
        .env("BOSS_ML_API_URL", "http://ml.test")
        .env("FAKE_CURL_LOG", &log)
        .env("FAKE_CURL_FAIL", fail.join(" "))
        .current_dir(&root)
        .output()
        .expect("run run-inference-batch.sh");

    let invoked = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    RunOutcome {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        invoked,
    }
}

/// The defect, directly: a mid-list model failing must NOT abort the
/// run. Every model after the failing one still gets its infer-batch,
/// and the script exits non-zero so the failure is recorded.
#[test]
fn one_failing_model_does_not_skip_the_rest() {
    // Fail an INDEPENDENT model in the middle of the list (#2). Its
    // failure must not stop #3..#7, and — since churn (#1) succeeds —
    // must not skip the dependent model either.
    let failing = "mdl-next-action-contract-expiring-v1";
    let r = run_with_failures(&[failing]);

    assert_eq!(
        r.invoked, ALL_MODELS,
        "a single failing model aborted the loop — the rest were skipped. \
         invoked: {:?}\nstderr: {}",
        r.invoked, r.stderr
    );
    assert!(
        !r.success,
        "a model failed, so the script must exit non-zero for systemd to \
         record it.\nstdout: {}\nstderr: {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stderr.contains(failing),
        "the failure must name the model on stderr.\nstderr: {}",
        r.stderr
    );
}

/// The dependency guard: if account-churn-risk fails, the dependent
/// high-churn-risk model is SKIPPED (never invoked) rather than run on
/// stale input — but the OTHER independent models still run.
#[test]
fn dependent_model_is_skipped_when_churn_fails() {
    let r = run_with_failures(&[CHURN_MODEL]);

    assert!(
        r.invoked.iter().any(|m| m == CHURN_MODEL),
        "the churn model must still be attempted.\ninvoked: {:?}",
        r.invoked
    );
    assert!(
        !r.invoked.iter().any(|m| m == DEPENDENT_MODEL),
        "the dependent model ran on stale input — it must be SKIPPED when \
         churn-risk fails.\ninvoked: {:?}\nstderr: {}",
        r.invoked,
        r.stderr
    );

    // Every OTHER model (independent of churn) must still have run.
    for m in ALL_MODELS {
        if *m == DEPENDENT_MODEL {
            continue;
        }
        assert!(
            r.invoked.iter().any(|x| x == m),
            "independent model {m} was skipped when only churn failed — \
             the guard must skip ONLY the dependent.\ninvoked: {:?}",
            r.invoked
        );
    }

    assert!(
        !r.success,
        "churn failed and its dependent was skipped — the run is degraded \
         and must exit non-zero.\nstderr: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains(DEPENDENT_MODEL) && r.stderr.to_lowercase().contains("skip"),
        "the skip must be logged, naming the dependent model.\nstderr: {}",
        r.stderr
    );
}

/// A failure in an unrelated model must NOT trip the dependency guard:
/// churn succeeded, so high-churn-risk must still run.
#[test]
fn dependent_model_runs_when_only_an_unrelated_model_fails() {
    // Fail a model that comes AFTER the dependent in the list too, to
    // be sure the guard keys on churn's outcome, not on any failure.
    let r = run_with_failures(&["mdl-next-action-stalled-service-ticket-v1"]);

    assert!(
        r.invoked.iter().any(|m| m == DEPENDENT_MODEL),
        "high-churn-risk was skipped even though churn-risk succeeded — the \
         guard must depend on churn's outcome only.\ninvoked: {:?}\nstderr: {}",
        r.invoked,
        r.stderr
    );
    assert_eq!(
        r.invoked, ALL_MODELS,
        "all models should have been attempted.\ninvoked: {:?}",
        r.invoked
    );
    assert!(!r.success, "a model failed; exit must be non-zero.");
}

/// The happy path: everything succeeds, everything runs, exit 0.
#[test]
fn all_models_run_and_exit_zero_on_full_success() {
    let r = run_with_failures(&[]);

    assert_eq!(
        r.invoked, ALL_MODELS,
        "a clean run must infer every model, in order.\ninvoked: {:?}",
        r.invoked
    );
    assert!(
        r.success,
        "a run with no failures must exit 0.\nstdout: {}\nstderr: {}",
        r.stdout, r.stderr
    );
}

/// The readable model list and the dependency comment are load-bearing
/// documentation the fix promised to keep. This also pins ALL_MODELS
/// above against the script, so a model added to one without the other
/// names itself here rather than silently under-testing.
#[test]
fn the_model_list_and_dependency_comment_survive() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read script");
    for m in ALL_MODELS {
        assert!(
            script.contains(m),
            "model {m} is in this test's ALL_MODELS but not in {SCRIPT} — \
             the two have drifted"
        );
    }
    assert!(
        script.contains("Order matters")
            && script.contains("BEFORE next-action-high-churn-risk reads them"),
        "the ordering-dependency comment was dropped from {SCRIPT}; it is the \
         one thing that explains the skip guard"
    );
}
