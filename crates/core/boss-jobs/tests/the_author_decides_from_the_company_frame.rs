//! The procedures that draft for David DECIDE from the company frame,
//! and leave him only the five kinds of question the frame cannot
//! answer (backlog 4f71e608).
//!
//! David, 2026-09-24 ~03:30Z: "I do sort of think you are pushing too
//! many questions to me that you could answer on your own." Measured
//! the same night: four page-audit sign-off briefs (0e4fef17, 7510b0c1,
//! 5ff999f8, 65a273d5) led with a decision for him, and design e1dba350
//! carried three questions — every one of which the agent then answered
//! from the frame. The drafting procedures said to write a gap list or
//! file a design with questions and never said who decides, so every
//! choice rode up to the founder by default.
//!
//! The fix is words, not machinery: each drafting procedure states the
//! frame in David's own words, tells its author to decide with a stated
//! reason recorded as `decided_<date>`, and names the five classes that
//! may still be asked. This pins those words at every site an author
//! reads them, so a later edit that drops them from one site fails
//! naming that site — the frame living in six places is the 9a shape,
//! held by this test rather than a comment.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

/// David's words, verbatim from the packet. The procedures quote them
/// rather than paraphrase them: a paraphrase is the drift this pins.
const FRAME: &str = "Remember we are building a company, run by one person plus agents, that is going to run a hosting business based on its open source release.";
const CHEAP_CHANGE: &str = "I can always provide feedback later too as our software is designed for making low cost changes.";
/// The ONLY things a drafting author may leave as a question for David.
const ESCALATE_ONLY: &str = "strategy or priority trade-offs, trust and security boundaries, credentials, money, and brand or voice";
/// Where a decision is recorded: a key on the item, dated.
const DECIDED: &str = "decided_<date>";

fn procedure(bundle: &[WorkflowSpec], kind: &str, step: &str) -> String {
    bundle
        .iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
        .steps
        .iter()
        .find(|s| s.title == step)
        .unwrap_or_else(|| panic!("{kind} has a `{step}` step"))
        .metadata_defaults["procedure"]
        .as_str()
        .unwrap_or_else(|| panic!("{kind}.{step} carries a procedure"))
        .to_string()
}

/// Every step where an author turns a finding into something David
/// reads states the frame, the escalation list and where a decision
/// is recorded.
#[test]
fn every_drafting_procedure_decides_from_the_frame() {
    let bundle = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    for (kind, step) in [
        ("page-audit", "measure"),
        ("page-audit", "test"),
        ("backlog-item", "draft-design"),
        ("user-feedback", "draft-design"),
    ] {
        let text = procedure(&bundle, kind, step);
        for (what, phrase) in [
            ("David's frame, verbatim", FRAME),
            ("David's cheap-change line, verbatim", CHEAP_CHANGE),
            ("the five escalation classes", ESCALATE_ONLY),
            ("where a decision is recorded", DECIDED),
        ] {
            assert!(
                text.contains(phrase),
                "{kind}.{step} states {what}: `{phrase}`"
            );
        }
    }
}

/// The two `draft-design` steps are one step in two kinds, so their
/// deciding paragraph is one text: equal, not merely similar.
#[test]
fn both_draft_design_steps_carry_the_same_deciding_paragraph() {
    let bundle = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    let paragraph = |kind: &str| {
        procedure(&bundle, kind, "draft-design")
            .split_once("\n\n")
            .unwrap_or_else(|| panic!("{kind}.draft-design has a second paragraph"))
            .1
            .to_string()
    };
    assert_eq!(paragraph("backlog-item"), paragraph("user-feedback"));
}

/// `file` is the low-effort copy step: it decides nothing itself, but
/// it is the step that puts each decision ON the gap's item, so it
/// names the key.
#[test]
fn the_file_step_carries_each_decision_onto_its_item() {
    let bundle = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    let text = procedure(&bundle, "page-audit", "file");
    assert!(text.contains(DECIDED), "page-audit.file names `{DECIDED}`");
    assert!(
        text.contains("--metadata"),
        "page-audit.file names the door the key rides through"
    );
}

/// The analyst rules said "Decide nothing that is the founder's" and
/// never said what that was, so an analyst read everything as the
/// founder's. The rule now names the five classes.
#[test]
fn the_analyst_rules_name_what_is_the_founders() {
    let path = boss_testing::repo_root().join("infra/platform/documents/analyst-rules.md");
    let text = std::fs::read_to_string(&path).expect("the analyst rules are authored");
    assert!(
        text.contains(ESCALATE_ONLY),
        "{} names the five escalation classes",
        path.display()
    );
}
