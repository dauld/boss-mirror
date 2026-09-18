//! The nightly revenue-recognition chore records WHAT it did, on both
//! hosts that run it (backlog 18d6a6c9, design e2580840 car 5,
//! 2026-09-17) — part (d).
//!
//! MEASURED. `boss-ledger-recognize` runs `recognize::run_tick`: one
//! query for `revenue_schedules` rows due today, one transaction per
//! row. On a table with no schedules it considers 0, posts 0, exits 0
//! and its packet's `run` step recorded `result=ok` and nothing else —
//! on prod, nightly, over nothing. Stopping it per tenant was weighed
//! and refused: `finance` is on for every tenant (a sponsorship invoice
//! is a revenue schedule the moment one is issued), the sweep's
//! `interval_minutes.maintenance-ledger-recognize = 1440` in
//! infra/dispatcher/rules/cadence-silence-sweep-daily.toml would alarm
//! within a day of the chore going quiet, and an idempotent tick over
//! an empty table costs one pod. So the chore runs, and what it lacked
//! was the RECORD: the binary now leaves the tick's counts at
//! `BOSS_RUN_SUMMARY_FILE` (infra/run-summary.sh's contract) and
//! boss-step.sh merges them onto the step under `result=`, so a run
//! over nothing reads `recognized=0` — distinguishable from a run that
//! posted a year. Both deployments must declare the path, or the
//! binary writes nothing and the packet is `ok` alone again.

use boss_testing::repo_root;

const UNIT: &str = "infra/boss-ledger-recognize.service";
const CRONJOB: &str = "infra/cluster/manifests/boss-ledger-recognize.yaml";
const BIN: &str = "crates/modules/boss-ledger/src/bin/boss_ledger_recognize.rs";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn the_systemd_unit_declares_the_summary_path_once_for_both_exec_lines() {
    let unit = read(UNIT);
    let declared: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("Environment=BOSS_RUN_SUMMARY_FILE="))
        .collect();
    assert_eq!(
        declared.len(),
        1,
        "{UNIT} declares BOSS_RUN_SUMMARY_FILE exactly once (the run and ExecStopPost inherit it): {declared:?}"
    );
    assert!(
        unit.lines().any(|l| l.starts_with("ExecStopPost=")
            && l.contains("boss-step.sh maintenance-ledger-recognize run")),
        "ExecStopPost still closes the packet through boss-step.sh, which merges the summary"
    );
}

#[test]
fn the_cluster_cronjob_declares_the_summary_path_and_closes_after_the_run() {
    let yaml = read(CRONJOB);
    assert!(
        yaml.contains("name: BOSS_RUN_SUMMARY_FILE"),
        "{CRONJOB} must hand the chore container BOSS_RUN_SUMMARY_FILE"
    );
    // Since 480e183c the CronJob hands the binary to boss-chore.sh,
    // which runs boss-step.sh AFTER the check on both legs — so the
    // summary file exists when it is read, and a tick that failed
    // still records what it counted. The ordering is the wrapper's
    // (a_chore_records_ok_and_failed.rs); here it is enough that the
    // binary is the wrapper's check.
    let line = yaml
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("/usr/local/bin/boss-chore.sh maintenance-ledger-recognize "))
        .expect("the CronJob records through boss-chore.sh under the recognize kind");
    assert!(
        line.contains(" -- /usr/local/bin/boss-ledger-recognize --config"),
        "the binary is the wrapper's check, after the `--`: {line}"
    );
}

#[test]
fn the_binary_reads_the_declared_path_and_never_fails_the_run_over_it() {
    let src = read(BIN);
    assert!(
        src.contains("std::env::var_os(\"BOSS_RUN_SUMMARY_FILE\")"),
        "the binary reads BOSS_RUN_SUMMARY_FILE"
    );
    assert!(
        src.contains("recognize::write_run_summary("),
        "and writes the summary through the library's one writer"
    );
    // Visibility is never a precondition (infra/run-summary.sh): a
    // write failure is logged, not propagated with `?`.
    let call = src
        .lines()
        .find(|l| l.contains("recognize::write_run_summary("))
        .unwrap();
    assert!(
        call.contains("if let Err(e) ="),
        "a failed summary write must not fail a recognition that succeeded: {call}"
    );
}
