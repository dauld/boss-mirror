//! The cluster-conformance sweep's Inspect step files its own
//! measurement: an ops-request `conformance-report` on the forge, linked
//! back as `metadata.for_sweep`. The sibling disk-backed sweeps do the
//! same with `disk-report`; this pins the conformance one, and that the
//! verb it names exists in the ops allowlist.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

fn shipped_rules() -> Registry {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/dispatcher/rules"
    );
    let mut toml = String::new();
    for entry in std::fs::read_dir(dir).expect("rules dir") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "toml") {
            toml.push_str(&std::fs::read_to_string(&p).unwrap());
            toml.push('\n');
        }
    }
    Registry::from_toml(&toml).expect("the shipped rules parse together")
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

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn the_conformance_sweep_files_a_conformance_report_for_itself() {
    let reg = shipped_rules();
    let hits = match_event(
        &reg,
        "step.ready.checklist",
        &inspect_ready("cluster-conformance"),
        &NoHelpers,
    )
    .matched;
    let spawns: Vec<_> = hits
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|d| d.handler == "jobs.spawn")
        .filter(|d| {
            arg(&d.args, "metadata.verb") == Some(&Value::String("conformance-report".into()))
        })
        .collect();
    assert_eq!(
        spawns.len(),
        1,
        "exactly one conformance-report spawn; got {hits:?}"
    );
    let d = spawns[0];
    assert_eq!(
        arg(&d.args, "kind"),
        Some(&Value::String("ops-request".into()))
    );
    assert_eq!(
        arg(&d.args, "metadata.host"),
        Some(&Value::String("forge".into()))
    );
    assert_eq!(
        arg(&d.args, "metadata.for_sweep"),
        Some(&Value::String("sweep-1".into())),
        "for_sweep must carry the sweep's own job id"
    );
}

#[test]
fn a_different_sweep_does_not_file_a_conformance_report() {
    let reg = shipped_rules();
    let hits = match_event(
        &reg,
        "step.ready.checklist",
        &inspect_ready("deploy-convergence"),
        &NoHelpers,
    )
    .matched;
    let any = hits.iter().flat_map(|h| h.invocations.iter()).any(|d| {
        arg(&d.args, "metadata.verb") == Some(&Value::String("conformance-report".into()))
    });
    assert!(
        !any,
        "the deploy-convergence sweep filed a conformance-report: {hits:?}"
    );
}

#[test]
fn the_verb_the_rule_names_is_in_the_ops_allowlist() {
    // One file per verb (5086842d): the verb IS infra/ops/verbs/<name>.json.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/ops/verbs/conformance-report.json"
    );
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path).expect("conformance-report is not an ops verb file"),
    )
    .expect("conformance-report.json parses");
    assert!(v.is_object(), "conformance-report is not an ops verb");
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(
        v["argv"],
        json!(["infra/cluster/undeclared-objects.sh", "--list"])
    );
    assert!(
        v["params"].as_array().is_some_and(Vec::is_empty),
        "a read with no arguments"
    );
}
