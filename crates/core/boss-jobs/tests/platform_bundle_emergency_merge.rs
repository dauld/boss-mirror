//! `infra/platform/workflows/emergency-merge.toml` keeps its decided
//! shape. One pin file per kind file (see `platform_bundle.rs`), so a
//! new protocol touches no shared line.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The emergency lane keeps its decided shape (packet c6c0f3b1). The
/// one trust decision — bypassing the train — is a sign-off owed by a
/// platform-admin; the merge cannot become ready without an approved
/// decision; the rollback lever is tried first and only `no-good-
/// revision` opens the lane; the filer must state the outage; and the
/// packet closes only with a retro filed. If any of these bends, the
/// lane is back to being improvised.
#[test]
fn the_emergency_merge_keeps_its_decided_shape() {
    use boss_core::job::FilledBy;
    let lane = bundled("emergency-merge");
    let step = |title: &str| {
        lane.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("emergency-merge has a `{title}` step"))
    };
    let outage = step("undo-first")
        .fields
        .iter()
        .find(|f| f.name == "outage")
        .expect("the filer states the outage");
    assert!(outage.required && outage.filled_by == FilledBy::Filer);
    assert!(
        step("gate-locally")
            .ready_when
            .contains("undo_result = \"no-good-revision\""),
        "only a failed rollback lever opens the merge lane"
    );
    let approve = step("approve");
    assert_eq!(approve.kind, "sign-off");
    assert_eq!(approve.sign_offs_required, vec!["platform-admin"]);
    assert!(
        step("merge").ready_when.contains("decision = \"approved\""),
        "nothing merges without the approved decision"
    );
    assert!(
        step("gate-locally")
            .fields
            .iter()
            .any(|f| f.name == "receipt_sha" && f.required),
        "the approver signs against a receipt sha, not a claim"
    );
    assert!(
        step("merged").ready_when.contains("steps.retro.done"),
        "the lane closes only with its retro filed"
    );
    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["rolled-back", "refused", "merged"]);
}
