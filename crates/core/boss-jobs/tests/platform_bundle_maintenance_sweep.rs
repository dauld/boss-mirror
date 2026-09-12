//! `infra/platform/workflows/maintenance-sweep.toml` keeps its decided
//! shape. One pin file per kind file (see `platform_bundle.rs`), so a
//! new protocol touches no shared line.
//!
//! WHAT THIS PINS, AND WHY IT EXISTS
//! ---------------------------------
//! Backlog 21edde87, measured 2026-09-11. The sweep protocol had TWO
//! terminals — `clear` and `remediated` — and
//! `spawn-car-on-sweep-remediated` filed a `ship-a-change` packet on
//! `remediated`. That reads "a sweep had to change something" as "a
//! sweep needs code shipped", and the two are not the same thing: of
//! the 21 remediated sweeps the registry had closed by 2026-09-11,
//! NINETEEN were discharged with the hands the sweep already has —
//! reclaim disk, prune a build cache, delete an orphan object, rebuild
//! a CI image — and only two ("nothing deleted, deliberately; filed
//! the real defect instead") needed a tree change. Each of the other
//! nineteen minted a delivery packet that could never progress: no
//! branch was ever cut, so it sat at `scope` until a human recognised
//! it as residue and abandoned it by hand. Three were cleaned up on
//! 2026-09-11 (2e4bd982, fc12c370, 5ce99240).
//!
//! The fix is the sweep SAYING which kind of remediation it is, as a
//! field on the step that did the remediating, and routing to a
//! different terminal for each — because a rule that has to be right
//! about intent will be wrong, and a dispatcher rule reading the closed
//! packet's `outcome` is already the honest trigger it uses.
//!
//! The fail-safe direction is load-bearing and is pinned below: an
//! absent `remediation_kind` routes to the OPERATIONAL terminal, the
//! one that spawns nothing. A missing field must not mint residue.

use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use serde_json::json;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("maintenance-sweep has a `{title}` step"))
}

/// Evaluate a bundled step's `ready_when` against a synthetic packet
/// where `remediate` has completed carrying `metadata` — the same
/// context shape `registry::build_context` builds at run time
/// (`steps.<slug>.{done,metadata}`).
fn remediate_done_with(spec: &WorkflowSpec, title: &str, metadata: serde_json::Value) -> bool {
    let payload = json!({
        "subject": { "id": "disk-headroom", "subject_kind": "custom" },
        "job": { "metadata": { "action_needed": "true", "target": "disk-headroom" } },
        "steps": {
            "opened": { "done": true, "metadata": {} },
            "inspect": { "done": true, "metadata": { "findings": "f", "measured": "m" } },
            "remediate": { "done": true, "metadata": metadata },
        },
    });
    let expr = boss_expr::parse(&step(spec, title).ready_when).expect("ready_when parses");
    boss_expr::eval(
        &expr,
        &boss_expr::Context {
            payload: &payload,
            helpers: &boss_expr::NoHelpers,
        },
    )
    .expect("ready_when evaluates")
    .as_bool()
    .expect("ready_when is boolean")
}

/// The field, not an inference. The `remediate` step declares WHICH
/// kind of remediation it performed, as a closed two-value enum, and it
/// is required — so a sweep cannot close `remediated` without saying.
#[test]
fn the_remediate_step_says_what_kind_of_remediation_it_is() {
    let sweep = bundled("maintenance-sweep");
    let remediate = step(&sweep, "remediate");
    let field = remediate
        .fields
        .iter()
        .find(|f| f.name == "remediation_kind")
        .expect("the remediate step declares `remediation_kind`");
    assert!(
        field.required,
        "required: a remediation that does not say what kind it is is the defect"
    );
    assert_eq!(
        field.field_type, "operational|code-change",
        "a closed enum, so `check_fork_coverage` can prove both branches are handled"
    );
}

/// Three terminals, and the one that spawns a delivery packet is the
/// one that says a tree change is owed.
#[test]
fn the_sweep_has_an_operational_terminal_and_a_change_needed_terminal() {
    let sweep = bundled("maintenance-sweep");
    let outcomes: Vec<(&str, &str)> = sweep
        .steps
        .iter()
        .filter_map(|s| {
            s.terminal
                .as_ref()
                .map(|t| (s.title.as_str(), t.outcome.as_str()))
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("remediated", "remediated"),
            ("change-needed", "change-needed"),
            ("clear", "clear"),
        ],
        "clear / remediated / change-needed — the third is new and is the only one \
         `spawn-car-on-sweep-remediated` fires on"
    );
}

/// An operational remediation is discharged in place: it reaches
/// `remediated`, which the spawn rule does not fire on.
#[test]
fn an_operational_remediation_reaches_the_terminal_that_spawns_nothing() {
    let sweep = bundled("maintenance-sweep");
    let md = json!({ "remediation_kind": "operational", "what_changed": "w", "verified": "v" });
    assert!(
        remediate_done_with(&sweep, "remediated", md.clone()),
        "operational → remediated"
    );
    assert!(
        !remediate_done_with(&sweep, "change-needed", md),
        "operational must NOT also reach change-needed — two ready terminals is a race"
    );
}

/// A finding the sweep cannot discharge routes to `change-needed`,
/// which is what the delivery spawn now fires on.
#[test]
fn a_code_change_remediation_reaches_the_change_needed_terminal() {
    let sweep = bundled("maintenance-sweep");
    let md = json!({ "remediation_kind": "code-change", "what_changed": "w", "verified": "v" });
    assert!(
        remediate_done_with(&sweep, "change-needed", md.clone()),
        "code-change → change-needed"
    );
    assert!(
        !remediate_done_with(&sweep, "remediated", md),
        "code-change must NOT also reach remediated"
    );
}

/// THE FAIL-SAFE. A `remediation_kind` that is absent — a row written
/// before the field existed, a step completed through a path that
/// skipped validation — routes to the operational terminal, the one
/// that spawns nothing. Residue is the failure this packet is about, so
/// the unknown case must not mint any.
#[test]
fn an_absent_remediation_kind_routes_to_the_operational_terminal() {
    let sweep = bundled("maintenance-sweep");
    let md = json!({ "what_changed": "w", "verified": "v" });
    assert!(
        remediate_done_with(&sweep, "remediated", md.clone()),
        "absent → remediated (spawns nothing): a missing field must not mint a delivery packet"
    );
    assert!(
        !remediate_done_with(&sweep, "change-needed", md),
        "absent must not reach change-needed"
    );
}

/// The inspect fork is untouched: `action_needed` still decides whether
/// there is anything to remediate at all, and `clear` still reads it off
/// the Job rather than off a step.
#[test]
fn the_inspect_fork_still_turns_on_action_needed() {
    let sweep = bundled("maintenance-sweep");
    assert_eq!(
        step(&sweep, "remediate").ready_when,
        "steps.inspect.done AND job.metadata.action_needed = \"true\""
    );
    assert_eq!(
        step(&sweep, "clear").ready_when,
        "steps.inspect.done AND job.metadata.action_needed = \"false\""
    );
}
