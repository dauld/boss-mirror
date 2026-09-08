//! The platform Workflow bundle says exactly what the code says.
//!
//! This is protocols-as-data Q4 made mechanical. David's answer: "moving
//! `user-feedback` v10 from code to bundle must produce a row identical
//! to the live v10 — not v11. If the loader publishes a new version
//! instead of recognising the existing one, every in-flight packet keeps
//! its old spec and the board grows a second lineage."
//!
//! The field-by-field comparison against the shipped spec lives in
//! `registry.rs`'s own test module, because the builders it compares
//! against are private and stay that way. This file keeps the half that
//! needs no privileged access.
//!
//! The comparison has to run in this direction. `WorkflowSpec` does NOT
//! serialize to TOML — TOML has no null, so the first `None` field fails
//! with `UnsupportedType(unit)` — so the bundle cannot be generated from
//! the code and diffed. It is authored, and this test is what makes that
//! safe: it parses the bundle with the same reader the tenant bundles
//! use and asserts each row equals the spec it is replacing, field for
//! field, including every step.
//!
//! THE BUNDLE IS A DIRECTORY — `infra/platform/workflows/<kind>.toml`,
//! one file per protocol — and this file holds only the rules that
//! range over EVERY kind. A rule about one protocol's decided shape
//! lives in its own `tests/platform_bundle_<kind>.rs`, so that adding
//! a protocol adds a kind file and a pin file and touches no shared
//! line. That is the point: on 2026-09-08 two protocol cars parked in
//! one day and the second was left behind on the tail of the one
//! bundle file and the tail of this one (backlog 3f3a6fec) — the
//! `manifest.txt` shape CLAUDE.md §9a records, collapsed the same way.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::{bundle_files, load_workflows};

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

/// One protocol per file, and the file is named for it. The loader
/// refuses anything else; this pins the rule against the real
/// directory so the listing IS the kind list — `ls` answers "which
/// kinds does a deployment seed", and every reader (the seed, the
/// timers lint, `boss workflow publish`) derives the same set.
#[test]
fn the_bundle_is_one_kind_file_per_protocol() {
    let dir = std::path::Path::new(platform_bundle_path());
    let files = bundle_files(dir).expect("the bundle directory lists its kind files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    let stems: Vec<String> = files
        .iter()
        .map(|f| {
            f.file_stem()
                .and_then(|s| s.to_str())
                .expect("a kind file has a UTF-8 stem")
                .to_string()
        })
        .collect();
    for (file, stem) in files.iter().zip(&stems) {
        let rows = load_workflows(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let kinds: Vec<&str> = rows.iter().map(|w| w.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![stem.as_str()],
            "{}: a kind file holds exactly one [[workflow]], named after the file",
            file.display()
        );
    }
    let loaded: Vec<String> = bundle().into_iter().map(|w| w.kind).collect();
    assert_eq!(
        loaded, stems,
        "loading the directory yields the kind files' rows, in file-name order"
    );
}

/// Every row in the bundle passes the same viability gate a publish
/// runs, so a malformed bundle fails here rather than at boot on the
/// deployment that loaded it.
#[test]
fn every_bundled_workflow_is_viable() {
    let reg = boss_jobs::step_registry::StepRegistry::v1();
    for row in bundle() {
        let problems = boss_jobs::workflow_lint::validate_workflow(&row, &reg);
        assert!(
            problems.is_empty(),
            "{} is not viable: {problems:?}",
            row.kind
        );
    }
}

/// A maintenance packet ends in its verdict. Until 2026-09-05 every
/// maintenance kind had one terminal, reached on `steps.run.done`
/// regardless of how the run went — and the unit only completed the
/// run step from ExecStartPost, which systemd skips on failure. So a
/// failed run either sat open looking like a run in progress
/// (disk-floor-sweep, 16:10, two FLOOR UNMET runs) or was closed "ok"
/// by the next run's recovery (forge-converge, 17:39, exit 1). Now the
/// run step's `result` routes: `ok` completes, anything else fails —
/// in the bundle AND in the three kinds still compiled into
/// platform_workflows(), which are one contract.
#[test]
fn every_maintenance_kind_ends_in_its_verdict() {
    let mut kinds: Vec<WorkflowSpec> = bundle()
        .into_iter()
        .filter(|w| w.kind.starts_with("maintenance-"))
        .collect();
    kinds.extend(
        boss_jobs::registry::platform_workflows()
            .into_iter()
            .filter(|w| w.kind.starts_with("maintenance-")),
    );
    assert!(
        kinds.len() >= 19,
        "expected the 16 bundled + 3 compiled maintenance kinds"
    );
    for w in kinds {
        let terminals: Vec<(&str, &str)> = w
            .steps
            .iter()
            .filter_map(|s| {
                s.terminal
                    .as_ref()
                    .map(|t| (t.outcome.as_str(), s.ready_when.as_str()))
            })
            .collect();
        assert_eq!(
            terminals.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            vec!["completed", "failed"],
            "{}: a maintenance packet ends in completed or failed, nothing else",
            w.kind
        );
        assert!(
            terminals[0]
                .1
                .contains("steps.run.metadata.result = \"ok\""),
            "{}: completed must require the run's result to be ok",
            w.kind
        );
        assert!(
            terminals[1]
                .1
                .contains("steps.run.metadata.result != \"ok\""),
            "{}: failed must be every other result",
            w.kind
        );
    }
}
