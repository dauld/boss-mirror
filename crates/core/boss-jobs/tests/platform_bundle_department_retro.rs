//! `infra/platform/workflows/department-retro.toml` keeps its decided
//! shape (design 3613f0af, backlog 1dffde5d). One pin file per kind
//! file (see `platform_bundle.rs`), so a new protocol touches no shared
//! line.
//!
//! The decision was "the SAME steps as protocol-retro, parameterised by
//! department", so the pin is written as a COMPARISON against the
//! protocol-retro row rather than a second copy of the step list: the
//! two kinds' step titles, kinds, gates and terminals are held equal,
//! and a reshape of either one that the other does not follow fails
//! here by step name (CLAUDE.md §9a — a fact that lives twice gets an
//! equality test).

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The steps a department retro walks are protocol-retro's steps —
/// same titles, same kinds, same `ready_when` gates, same two
/// terminals. Only the words on them (procedures, fields) speak of a
/// department rather than the pipeline.
#[test]
fn a_department_retro_walks_the_same_steps_as_the_protocol_retro() {
    let dept = bundled("department-retro");
    let proto = bundled("protocol-retro");
    assert_eq!(
        dept.subject_kinds, proto.subject_kinds,
        "the Subject is a custom id"
    );

    let shape = |w: &WorkflowSpec| -> Vec<(String, String, String, Option<String>)> {
        w.steps
            .iter()
            .map(|s| {
                (
                    s.title.clone(),
                    s.kind.clone(),
                    s.ready_when.clone(),
                    s.terminal.as_ref().map(|t| t.outcome.clone()),
                )
            })
            .collect()
    };
    assert_eq!(
        shape(&dept),
        shape(&proto),
        "department-retro's steps (title, kind, ready_when, terminal) are protocol-retro's"
    );
    assert_eq!(
        dept.steps.len(),
        8,
        "opened / collect / analyze / gaps / report / review / recorded / cancelled"
    );
}

/// The parameter: a packet of this kind names its department, and the
/// row says so in its admission contract.
#[test]
fn a_department_retro_requires_its_department() {
    let wf = bundled("department-retro");
    let required: Vec<&str> = wf.metadata_schema["required"]
        .as_array()
        .expect("metadata_schema.required is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["department"]);
    assert_eq!(
        wf.metadata_schema["properties"]["department"]["type"], "string",
        "the department is a Class code"
    );
}

/// The constraint protocol-retro learned three times over: the reviewer
/// cannot be handed an empty packet, because `report` cannot complete
/// without `sign_off_context`, and `review` waits on `report`.
#[test]
fn the_sign_off_is_fed_by_the_report_step() {
    let wf = bundled("department-retro");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("department-retro has no `{title}` step"))
    };
    let report = step("report");
    assert!(
        report
            .fields
            .iter()
            .any(|f| f.name == "sign_off_context" && f.required),
        "report requires sign_off_context; got {:?}",
        report.fields
    );
    let review = step("review");
    assert_eq!(review.kind, "sign-off");
    assert_eq!(review.ready_when, "steps.report.done");
    assert_eq!(review.authority_role.as_deref(), Some("platform-admin"));
    assert!(
        review
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required),
        "the sign-off records its decision"
    );

    // The gaps step FILES work with counts, and its output fields say so.
    let gaps = step("gaps");
    for name in ["repeated_manual_work", "protocol_gaps", "protocol_updates"] {
        assert!(
            gaps.fields.iter().any(|f| f.name == name && f.required),
            "gaps requires `{name}`"
        );
    }
    // The collect procedure reads the department's own doors, not IT's.
    let collect = step("collect");
    let procedure = collect.metadata_defaults["procedure"]
        .as_str()
        .expect("collect carries a procedure");
    for door in [
        "/api/departments/{department}/readiness",
        "/api/jobs?department={department}",
        "terminal-report",
        "/api/sensors",
    ] {
        assert!(procedure.contains(door), "collect names `{door}`");
    }
}
