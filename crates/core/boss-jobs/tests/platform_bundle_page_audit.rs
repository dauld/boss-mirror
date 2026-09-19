//! `infra/platform/workflows/page-audit.toml` keeps its decided shape
//! (design 0e07ce64, backlog 0c4ff12b). One pin file per kind file
//! (see `platform_bundle.rs`), so a new protocol touches no shared
//! line.
//!
//! The decision was a march of measure / file / test / review /
//! styled-deferred / audited, one packet per catalogued route with the
//! route and its department on the packet, the agent drafting and the
//! founder deciding at `review`. Each of those is pinned below by step
//! name, so a reshape that drops one fails naming it.

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::audience::Audience;
use boss_jobs::registry::{StepSpec, WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;
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
        .unwrap_or_else(|| panic!("page-audit has a `{title}` step"))
}

/// The march's steps, in order: the decided list plus the `revise`
/// route the review's second verdict needs and the `withdrawn` exit
/// for a route that leaves the catalog.
#[test]
fn a_page_audit_walks_measure_file_test_review_styled_audited() {
    let wf = bundled("page-audit");
    let shape: Vec<(&str, &str, Option<&str>)> = wf
        .steps
        .iter()
        .map(|s| {
            (
                s.title.as_str(),
                s.kind.as_str(),
                s.terminal.as_ref().map(|t| t.outcome.as_str()),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("opened", "trigger", None),
            ("measure", "task", None),
            ("file", "task", None),
            ("test", "task", None),
            ("review", "sign-off", None),
            ("revise", "task", None),
            ("styled", "outcome", None),
            ("audited", "outcome", Some("audited")),
            ("withdrawn", "outcome", Some("withdrawn")),
        ],
        "page-audit's steps (title, kind, terminal outcome)"
    );
    assert_eq!(
        wf.subject_kinds,
        vec!["custom".to_string()],
        "the Subject is the route"
    );
    let errs = boss_jobs::workflow_lint::validate_workflow(&wf, &StepRegistry::v1());
    assert!(
        errs.is_empty(),
        "the bundled page-audit lints clean: {errs:?}"
    );
}

/// The parameter: a packet names its route and its department in the
/// row's admission contract, so a hand-filed audit without them is
/// refused at the filer, not discovered by the agent.
#[test]
fn a_page_audit_requires_its_route_and_department() {
    let wf = bundled("page-audit");
    let required: Vec<&str> = wf.metadata_schema["required"]
        .as_array()
        .expect("metadata_schema.required is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["route", "department"]);
    for key in ["route", "department"] {
        assert_eq!(
            wf.metadata_schema["properties"][key]["type"], "string",
            "`{key}` is a string"
        );
    }
}

/// The agent's steps declare the platform operator's role as their
/// audience — the `role` shape, agent-workable — and nothing else
/// about who they are for. NOT an `individual` automation actor (that
/// is pr-train's shape, for steps one automation completes) and not a
/// bare `authority_role` beside the audience (two declarations).
#[test]
fn the_agents_steps_are_platform_admin_tasks_and_the_founder_decides() {
    let wf = bundled("page-audit");
    for slug in ["measure", "file", "test", "revise"] {
        let s = step(&wf, slug);
        assert_eq!(s.kind, "task");
        assert_eq!(
            s.audience,
            Some(Audience::Role("platform-admin".into())),
            "`{slug}` is a platform-admin task"
        );
        assert_eq!(
            s.metadata_defaults["human_only"],
            json!(false),
            "`{slug}` is agent-workable"
        );
        assert!(
            s.metadata_defaults["procedure"]
                .as_str()
                .is_some_and(|p| p.len() > 200),
            "`{slug}` carries a procedure that names its reads"
        );
    }
    let review = step(&wf, "review");
    assert_eq!(review.kind, "sign-off");
    assert_eq!(review.ready_when, "steps.test.done");
    assert_eq!(
        review.audience,
        Some(Audience::Role("platform-admin".into()))
    );
    let decision = review
        .fields
        .iter()
        .find(|f| f.name == "decision")
        .expect("review records its decision");
    assert!(decision.required);
    assert_eq!(decision.field_type, "approved|changes-requested");
}

/// The constraint protocol-retro learned three times over: the reviewer
/// cannot be handed an empty packet, because `test` cannot complete
/// without `sign_off_context`, and `review` waits on `test`.
#[test]
fn the_sign_off_is_fed_by_the_test_step() {
    let wf = bundled("page-audit");
    let test = step(&wf, "test");
    for name in ["spec_path", "car", "sign_off_context"] {
        assert!(
            test.fields.iter().any(|f| f.name == name && f.required),
            "test requires `{name}`; got {:?}",
            test.fields
        );
    }
    // The measured gap is a list in three named fields, and the file
    // step's output is ids.
    let measure = step(&wf, "measure");
    for name in ["controls_md", "needs_md", "gaps_md"] {
        assert!(
            measure.fields.iter().any(|f| f.name == name && f.required),
            "measure requires `{name}`"
        );
    }
    let file = step(&wf, "file");
    assert!(
        file.fields
            .iter()
            .any(|f| f.name == "filed_ids" && f.required),
        "file requires `filed_ids`"
    );
    // The measure procedure reads the department's own doors.
    let procedure = measure.metadata_defaults["procedure"]
        .as_str()
        .expect("measure carries a procedure");
    for door in [
        "/api/departments/{department}/readiness",
        "/api/jobs?department={department}",
        "apps/web/src",
    ] {
        assert!(procedure.contains(door), "measure names `{door}`");
    }
}

/// THE TWO DRAFTING PROCEDURES NAME THE COMPLETION VERB (backlog
/// d1c03a44, 2026-09-19). `boss brief` renders the procedure, and the
/// procedure is all an analyst reads about how to record what it
/// found. Left silent, each of the ~94 measure/file completions is a
/// hand-built `boss-api PUT` — and each one is an opportunity for the
/// three failures `boss step complete` refuses: an undeclared name
/// stored as an annotation and answered 204, a wholesale `metadata`
/// replace that deletes the step's own `procedure` and `agent` block,
/// and a 204 read as evidence.
///
/// `measure` names the FILE door as well, because its three required
/// fields are whole markdown documents: a document through argv is a
/// quoting fight, and a backtick inside it is command substitution
/// (backlog 2376b89e).
#[test]
fn the_drafting_procedures_name_the_completion_verb() {
    let wf = bundled("page-audit");
    for slug in ["measure", "file"] {
        let procedure = step(&wf, slug).metadata_defaults["procedure"]
            .as_str()
            .unwrap_or_else(|| panic!("`{slug}` carries a procedure"));
        assert!(
            procedure.contains("boss step complete"),
            "`{slug}` names the completion verb"
        );
        assert!(
            procedure.contains(&format!("--step {slug}")),
            "`{slug}` names its own slug in the call"
        );
    }
    let measure = step(&wf, "measure").metadata_defaults["procedure"]
        .as_str()
        .expect("measure carries a procedure");
    for field in ["controls_md", "needs_md", "gaps_md"] {
        assert!(
            measure.contains(&format!("--field-file {field}=")),
            "measure passes `{field}` through the file door"
        );
    }
}

/// `styled` is the deferral, recorded by name: a marker the machine
/// completes when the review approves (or the revision lands), feeding
/// the ONE happy terminal. Two terminals off one decision would race;
/// this pins that there is one, and that the deferral sits before it.
#[test]
fn styled_is_a_deferred_marker_feeding_the_audited_terminal() {
    let wf = bundled("page-audit");
    let styled = step(&wf, "styled");
    assert_eq!(styled.kind, "outcome");
    assert!(
        styled.terminal.is_none(),
        "styled is a marker, not a terminal"
    );
    assert_eq!(styled.metadata_defaults["outcome_kind"], json!("skipped"));
    assert_eq!(
        styled.ready_when,
        "(steps.review.done AND steps.review.metadata.decision = \"approved\") OR steps.revise.done"
    );
    let audited = step(&wf, "audited");
    assert_eq!(audited.ready_when, "steps.styled.done");
    assert_eq!(
        audited.metadata_defaults["outcome_kind"],
        json!("completed")
    );
    let revise = step(&wf, "revise");
    assert_eq!(
        revise.ready_when,
        "steps.review.done AND steps.review.metadata.decision = \"changes-requested\""
    );
    let happy: Vec<&str> = wf
        .steps
        .iter()
        .filter(|s| {
            s.terminal.is_some() && s.metadata_defaults["outcome_kind"] == json!("completed")
        })
        .map(|s| s.title.as_str())
        .collect();
    assert_eq!(happy, vec!["audited"], "one completed terminal");
}

/// Materialised for a route, the agent's steps are born unassigned
/// with the role projected — the executor lane nominates them — and
/// the markers are nobody's.
#[test]
fn a_materialised_audit_is_born_in_the_platform_admin_queue() {
    let wf = bundled("page-audit");
    let subject = Subject::new("custom", "/ux/support");
    let steps = materialize_steps(
        &wf,
        &subject,
        JobId::new(),
        &json!({ "route": "/ux/support", "department": "support" }),
        StepId::new,
    );
    for slug in ["measure", "file", "test", "review", "revise"] {
        let s = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("`{slug}` materialised"));
        assert_eq!(s.assignee_id, None, "`{slug}` is nobody's by name");
        assert_eq!(
            s.metadata.get("authority_role"),
            Some(&json!("platform-admin")),
            "`{slug}` carries the projected role for the queue"
        );
    }
    for slug in ["opened", "styled", "audited", "withdrawn"] {
        let s = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("`{slug}` materialised"));
        assert_eq!(s.assignee_id, None, "`{slug}` is a marker, nobody's");
        assert!(s.metadata.get("authority_role").is_none());
    }
}

/// Each of the four drafting steps declares HOW an agent runs it —
/// the block the dispatch door refuses a step without (backlog
/// 4a1b307c). All 47 page-audit packets opened on 2026-09-19 sat at
/// `measure`, every one undispatchable: `human_only = false` said "an
/// agent may do this" and the row said nothing about which agent, at
/// what effort, under what spend.
///
/// THE SETTINGS THEMSELVES live in the bundle-wide roster
/// (`platform_bundle_agent_blocks.rs`) and the reasoning for each
/// lives beside its step in the TOML — three efforts across these
/// four, because car e720dd00 made the declared effort select the
/// definition the step actually runs under. This test asserts only
/// that the four declare, so it cannot drift from that roster.
#[test]
fn the_four_agent_workable_steps_declare_how_an_agent_runs_them() {
    let wf = bundled("page-audit");
    for slug in ["measure", "file", "test", "revise"] {
        assert!(
            step(&wf, slug).agent.is_some(),
            "`{slug}` is agent-workable and declares an agent block"
        );
    }
}

/// The founder's step is NOT an agent's, and neither is a marker. A
/// block on `review` would hand the executor lane a sign-off the
/// design reserved for David ("the agent drafts; David decides at
/// `review`", design 0e07ce64), and a block on a marker would write
/// four agent keys onto a step the machine completes on its own.
#[test]
fn the_sign_off_and_the_markers_declare_no_agent() {
    let wf = bundled("page-audit");
    for slug in ["opened", "review", "styled", "audited", "withdrawn"] {
        assert!(
            step(&wf, slug).agent.is_none(),
            "`{slug}` is not an agent's step and declares no agent block"
        );
    }
}
