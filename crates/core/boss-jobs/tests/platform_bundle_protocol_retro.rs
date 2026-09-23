//! `infra/platform/workflows/protocol-retro.toml` carries the tier mix
//! as a STANDING reading (backlog 79fdc808, design 32f18167). One pin
//! file per kind file (see `platform_bundle.rs`), so a new pin touches
//! no shared line.
//!
//! The decision was that "is the core settling while work moves
//! outward" is answered where a decision is already being made, every
//! period, rather than argued — and protocol-retro is the platform's
//! weekly retro (the `department-retros-weekly` rule opens it beside
//! every department's). A reading the procedure merely MENTIONS is one
//! a busy week skips, so `collect` REQUIRES the field: the step cannot
//! complete without the reading, the same way `report` cannot complete
//! without `sign_off_context`.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn protocol_retro() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "protocol-retro")
        .expect("protocol-retro ships in the platform bundle")
}

#[test]
fn the_retro_collect_step_requires_the_tier_mix_reading() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let field = collect
        .fields
        .iter()
        .find(|f| f.name == "tier_mix")
        .expect("collect declares a tier_mix field");
    assert!(
        field.required,
        "tier_mix is REQUIRED at collect: a standing reading, not a mention"
    );
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    // The verb the reading comes from, and the two rules that make it a
    // reading rather than a target: coverage stated, no ratio to meet.
    for phrase in [
        "boss channels --tiers",
        "coverage",
        "NO TARGET RATIO",
        "OUTWARD",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// Beside the core-changes count (bd93d2be) and the tier mix sits the
/// DIRECTION the work on the core should take (backlog 4332f3cf,
/// design 93d2bddb): a change that REMOVES a defect class buys one core
/// change and then none, while one that ENUMERATES another case buys a
/// core change per incident forever. It is prose by decision, not a
/// metric — nothing in the record marks a change as either — so the pin
/// holds the direction, the refusal to invent a number, and the three
/// readings named together in the one place a retro reads them.
#[test]
fn the_retro_collect_step_names_class_removal_as_the_direction_beside_the_readings() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    for phrase in [
        "4332f3cf",
        "bd93d2be",
        "REMOVED",
        "ENUMERATED",
        "THE DIRECTION WE ARE TRYING TO CAUSE is removal",
        "PROSE, NOT A SECOND NUMBER",
        "the tier mix asks whether work is moving outward",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// The other half of the reading (backlog bd93d2be): the ABSOLUTE
/// number of core changes per day, the crest signal David chose on
/// 2026-09-19 — beside the tier mix, required at collect for the same
/// reason, and read in the direction he named with no target.
#[test]
fn the_retro_collect_step_requires_the_core_changes_reading() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let field = collect
        .fields
        .iter()
        .find(|f| f.name == "core_changes")
        .expect("collect declares a core_changes field");
    assert!(
        field.required,
        "core_changes is REQUIRED at collect: a standing reading, not a mention"
    );
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    for phrase in [
        "boss channels --core-changes",
        "git fetch origin",
        "ABSOLUTE",
        "NO TARGET AND NO THRESHOLD",
        "DOWN",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}
