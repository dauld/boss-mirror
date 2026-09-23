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
