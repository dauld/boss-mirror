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

/// Is this a maintenance kind a systemd unit runs?
///
/// The selector for the rule below, and it asks about the EXECUTOR
/// rather than the name. A unit-run kind's trigger declares
/// `trigger_name = "systemd-timer"`, which is what the unit's
/// `boss-maintenance-wrap.sh` call puts there — so this is the same
/// fact the timer lint reads, not a second guess at it.
///
/// `kind.starts_with("maintenance-")` was the selector until
/// 2026-09-10, and it was a PROXY for this: it happened to be exact
/// only because the one maintenance protocol with a different executor
/// (`maintenance-sweep`, inspected by a person or an agent, forking on
/// `clear` / `remediated`) was not authored in the tree at all. Writing
/// it down made the proxy wrong, which is the proxy's fault and not the
/// protocol's: a sweep has no `run` step whose `result` could route a
/// verdict, and asserting it ends in completed/failed would have been
/// asserting something about a shape nothing in this rule's story is
/// about.
fn is_unit_run(w: &WorkflowSpec) -> bool {
    w.kind.starts_with("maintenance-")
        && w.steps.iter().any(|s| {
            s.metadata_defaults
                .get("trigger_name")
                .and_then(|v| v.as_str())
                == Some("systemd-timer")
        })
}

/// A unit-run maintenance packet ends in its verdict. Until 2026-09-05
/// every maintenance kind had one terminal, reached on `steps.run.done`
/// regardless of how the run went — and the unit only completed the
/// run step from ExecStartPost, which systemd skips on failure. So a
/// failed run either sat open looking like a run in progress
/// (disk-floor-sweep, 16:10, two FLOOR UNMET runs) or was closed "ok"
/// by the next run's recovery (forge-converge, 17:39, exit 1). Now the
/// run step's `result` routes: `ok` completes, anything else fails.
///
/// The three chores that were still compiled into `platform_workflows()`
/// joined the bundle on 2026-09-11, so this reads one place. It read
/// both, and the second half is gone rather than kept as an empty loop:
/// `platform_workflows()` is empty now, and iterating it would have
/// looked like coverage it no longer provides.
#[test]
fn every_unit_run_maintenance_kind_ends_in_its_verdict() {
    let kinds: Vec<WorkflowSpec> = bundle().into_iter().filter(is_unit_run).collect();
    assert!(
        kinds.len() >= 19,
        "expected at least the 19 bundled unit-run maintenance kinds, got {}",
        kinds.len()
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

/// The carve-out above is a named set, not a silence.
///
/// `is_unit_run` exempts a maintenance kind from the verdict rule, and
/// an exemption nobody can see is how a rule stops covering what it was
/// written for. So the set it exempts is pinned: exactly one protocol
/// today, and a second one is a decision that belongs in the diff a
/// reviewer reads — either the new kind is genuinely not unit-run, or
/// its trigger is missing the `trigger_name` its unit is supposed to
/// stamp, which is itself the defect `timers-leave-a-packet` is about.
#[test]
fn only_the_sweep_is_exempt_from_the_verdict_rule() {
    let exempt: Vec<String> = bundle()
        .into_iter()
        .filter(|w| w.kind.starts_with("maintenance-") && !is_unit_run(w))
        .map(|w| w.kind)
        .collect();
    assert_eq!(
        exempt,
        vec!["maintenance-sweep".to_string()],
        "a maintenance kind no systemd unit runs skips the completed/failed rule — \
         say so here, with why"
    );
}
