//! The deploy-convergence sweep's Inspect step fires the self-inspection
//! handler with its own target — and the empty-decisions sweep's does
//! not fire it for deploy-convergence (each rule names one target, so
//! two rules never inspect one sweep).

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

mod common;

/// The whole authored directory, read the way the seed reads it.
fn shipped_rules() -> Registry {
    common::authored_registry()
}

fn inspect_ready(target: &str) -> serde_json::Value {
    json!({
        "job_id": "sweep-1",
        "step_id": "inspect-1",
        "kind": "checklist",
        "subject_kind": "custom",
        "subject_id": target,
        "metadata": {},
    })
}

#[test]
fn the_deploy_convergence_sweep_inspects_itself_with_its_own_target() {
    let reg = shipped_rules();
    let hits = match_event(
        &reg,
        "step.ready.checklist",
        &inspect_ready("deploy-convergence"),
        &NoHelpers,
    )
    .matched;
    let inspections: Vec<_> = hits
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|d| d.handler == "maintenance.sweep.inspect")
        .collect();
    let targets: Vec<_> = inspections
        .iter()
        .map(|d| {
            d.args
                .iter()
                .find(|(k, _)| k == "target")
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert!(
        targets.contains(&Some(Value::String("deploy-convergence".into()))),
        "no maintenance.sweep.inspect with target = deploy-convergence fired; got {targets:?}"
    );
}

#[test]
fn a_disk_sweep_does_not_fire_the_self_inspection() {
    let reg = shipped_rules();
    let hits = match_event(
        &reg,
        "step.ready.checklist",
        &inspect_ready("disk-headroom"),
        &NoHelpers,
    )
    .matched;
    let named: Vec<_> = hits
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|d| d.handler == "maintenance.sweep.inspect")
        .filter_map(|d| {
            d.args
                .iter()
                .find(|(k, _)| k == "target")
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert!(
        named.is_empty(),
        "a disk sweep's Inspect fired a targeted self-inspection: {named:?}"
    );
}
