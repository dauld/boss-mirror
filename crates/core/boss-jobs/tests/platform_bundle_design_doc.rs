//! `infra/platform/workflows/design-doc.toml` keeps its decided shape.
//! One pin file per kind file (see `platform_bundle.rs`), so a new
//! protocol touches no shared line.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The design-doc protocol states its own requirement (packet
/// 2e136a67). The one field the whole protocol exists to process —
/// `questions` — is a FILER field on the review step, bound from the
/// Job, and it declares the element shape the tracker renders:
/// `{anchor, title, proposal}`. Before this the registry said nothing
/// about questions at all, and eight title-less drafts in one session
/// were admitted silently and dead-ended at "nothing to review".
#[test]
fn the_design_doc_protocol_declares_its_questions() {
    use boss_core::job::FilledBy;
    let doc = bundled("design-doc");
    let review = doc
        .steps
        .iter()
        .find(|s| s.title == "review")
        .expect("design-doc has a review step");
    let questions = review
        .fields
        .iter()
        .find(|f| f.name == "questions")
        .expect("the review step declares `questions`");
    assert!(questions.required, "questions is required");
    assert_eq!(
        questions.filled_by,
        FilledBy::Filer,
        "questions is owed by the filer at admission, not by the reviewer at done"
    );
    assert_eq!(questions.field_type, "array");
    assert_eq!(
        questions.item_keys,
        vec!["anchor", "title", "proposal"],
        "each question carries the three keys the tracker renders"
    );
    assert_eq!(
        review.metadata_defaults.get("questions"),
        Some(&serde_json::json!("{metadata.questions}")),
        "the step reads its questions off the Job — the whole-value binding"
    );
    // The live row (v4, 2026-09-05) carries a fold step; the bundle
    // must not fall behind the registry it seeds.
    let titles: Vec<&str> = doc.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["drafted", "review", "fold", "published"]);
}
