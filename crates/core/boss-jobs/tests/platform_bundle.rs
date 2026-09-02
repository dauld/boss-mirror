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

use boss_jobs::registry::WorkflowSpec;
use boss_jobs::seed_loader::load_workflows;

const BUNDLE: &str = "../../../infra/platform/workflows.toml";

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(BUNDLE).expect("the platform bundle parses")
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

/// The experiment protocol ships as data, shaped the way the design
/// review decided (network-experiments.md, packet 574c2adf; built for
/// 6ea5a12a). Q3: the experiment IS a packet with `promoted` /
/// `retired` terminal states. Q4: experimenting is IT + platform-admins
/// for now — every acting step wears the platform-admin gate. If
/// someone reshapes the bundle row, this names exactly which decided
/// property broke.
#[test]
fn the_experiment_protocol_keeps_its_decided_shape() {
    let rows = bundle();
    let exp = rows
        .iter()
        .find(|w| w.kind == boss_jobs::experiments::EXPERIMENT_KIND)
        .expect(
            "protocol-experiment ships in the platform bundle — a fresh \
                 deployment must be able to run an experiment at all",
        );

    // Q3: promoted / retired are terminals (inconclusive is the
    // publish-gate-required fallback for the closed verdict set, and
    // abandoned the withdrawal door — both may exist; the two decided
    // terminals must).
    for wanted in ["promoted", "retired"] {
        let step = exp
            .steps
            .iter()
            .find(|s| s.title == wanted)
            .unwrap_or_else(|| panic!("Q3 decided a `{wanted}` terminal; it is gone"));
        assert_eq!(
            step.terminal.as_ref().map(|t| t.outcome.as_str()),
            Some(wanted),
            "`{wanted}` must be a terminal, not a mere step"
        );
    }

    // Q4: every acting (non-terminal) step is platform-admin gated.
    for step in exp.steps.iter().filter(|s| s.terminal.is_none()) {
        assert_eq!(
            step.authority_role.as_deref(),
            Some("platform-admin"),
            "Q4 limits experimenting to IT + platform-admins; step `{}` \
             dropped the gate",
            step.title
        );
    }
}
