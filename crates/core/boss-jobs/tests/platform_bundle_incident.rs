//! `infra/platform/workflows/incident.toml` is the ONE incident protocol,
//! and it carries the post-mortem's forcing questions (backlog 59d15039,
//! decided by David 2026-09-22). One pin file per kind file (see
//! `platform_bundle.rs`), so a new pin touches no shared line.
//!
//! Three incident protocols existed — `incident`, `incident-post-mortem`
//! and `post-mortem` — and measured on 2026-09-22 none had ever carried
//! a packet, open or closed. The first real incident (55d001b0, the dark
//! dev-pod doors) ran on `incident` v1, and everything the post-mortem
//! protocols would have asked of it — did we just ship this, what should
//! have found it, what can be removed, which packets carry the actions —
//! landed in free-form job metadata because the protocol had no step to
//! hold it. So `incident` keeps `stabilize` first and gains the questions
//! in the order that forces them; the other two are retired, rows and
//! files both.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

fn incident() -> WorkflowSpec {
    bundle()
        .into_iter()
        .find(|w| w.kind == "incident")
        .expect("incident ships in the platform bundle")
}

/// The fold is a consolidation: after it there is one incident
/// protocol, not three. A retired kind whose file stayed in the tree
/// would be re-seeded into every fresh database.
#[test]
fn the_platform_has_one_incident_protocol() {
    let kinds: Vec<String> = bundle().into_iter().map(|w| w.kind).collect();
    for retired in ["incident-post-mortem", "post-mortem"] {
        assert!(
            !kinds.iter().any(|k| k == retired),
            "`{retired}` is retired into `incident` (59d15039) and must not ship in the bundle"
        );
    }
    assert!(kinds.iter().any(|k| k == "incident"));
}

/// Stabilize comes first, then the facts before any theory, then "did
/// we just ship this?" before the root cause (the 2026-08-13 SoR outage
/// lost an hour because nobody asked it first), then what should have
/// found it, then what can be REMOVED before any action is added, then
/// the actions, then a person's review. A chain, each step ready on the
/// one before, so the order is the protocol and not advice.
#[test]
fn the_questions_are_asked_in_the_order_that_forces_them() {
    let wf = incident();
    let order = [
        "raised",
        "stabilize",
        "timeline",
        "attribution",
        "diagnose",
        "detection",
        "simplification",
        "actions",
        "review",
        "closed",
    ];
    let titles: Vec<&str> = wf.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, order, "incident's steps, in order");
    for pair in order.windows(2) {
        let step = wf.steps.iter().find(|s| s.title == pair[1]).unwrap();
        assert_eq!(
            step.ready_when,
            format!("steps.{}.done", pair[0]),
            "`{}` is ready only after `{}`",
            pair[1],
            pair[0]
        );
    }
}

/// Each forcing question is a REQUIRED field, so the step cannot close
/// without an answer — a question the protocol only mentions is one a
/// busy incident skips.
#[test]
fn each_forcing_question_is_a_required_answer() {
    let wf = incident();
    let required = |step: &str, field: &str| {
        let s = wf
            .steps
            .iter()
            .find(|s| s.title == step)
            .unwrap_or_else(|| panic!("incident has a `{step}` step"));
        let f = s
            .fields
            .iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("`{step}` declares `{field}`"));
        assert!(f.required, "`{step}.{field}` is required at done");
        f.field_type.clone()
    };
    required("timeline", "timeline");
    required("timeline", "detected_at");
    assert_eq!(
        required("attribution", "self_inflicted"),
        "yes|no|unknown",
        "attribution is answered, not narrated"
    );
    required("attribution", "what_we_believed_first");
    required("diagnose", "root_cause");
    required("detection", "earliest_possible_detection");
    required("detection", "why_not_caught_earlier");
    required("simplification", "simplification");
    assert_eq!(
        required("actions", "action_packets"),
        "array",
        "MANY actions, each filed as its own packet — an array of refs, not one field"
    );
}

/// The review is a PERSON's, by declaration the step API enforces
/// (`human_only`, refused for an agent assignee): an agent may run every
/// other step, but an incident an agent grades itself on is not
/// reviewed. It renders on the incident-review surface, which lays the
/// packet's findings and every step's answers out as one document.
#[test]
fn the_review_is_a_human_reading_the_findings() {
    let wf = incident();
    let review = wf
        .steps
        .iter()
        .find(|s| s.title == "review")
        .expect("incident has a review step");
    assert!(
        boss_jobs::human_only::declared(&review.metadata_defaults),
        "review declares human_only: {}",
        review.metadata_defaults
    );
    assert_eq!(review.kind, "incident-review");
    assert_eq!(review.authority_role.as_deref(), Some("platform-admin"));
}

/// And it is viable: the same gate a publish runs.
#[test]
fn the_folded_incident_protocol_is_viable() {
    let reg = boss_jobs::step_registry::StepRegistry::v1();
    let problems = boss_jobs::workflow_lint::validate_workflow(&incident(), &reg);
    assert!(problems.is_empty(), "incident is not viable: {problems:?}");
}
