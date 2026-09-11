//! The platform Workflow seed inserts what is missing and touches
//! nothing else.
//!
//! protocols-as-data Q1, as David answered it: "the seed binary inserts
//! what is missing and touches nothing that exists ... Drift-healing
//! goes away deliberately: it is the feature that reverts operator
//! edits." Both halves are load-bearing and both are asserted here —
//! the insert, and the not-touching.

use boss_core::actor::ActorId;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus};
use boss_jobs::seed_loader::load_workflows;
use boss_testing::TestDb;

const BUNDLE: &str = "../../../infra/platform/workflows";

fn seed_actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// The logic the binary runs, exercised directly so the contract is
/// tested rather than the argument parsing.
async fn seed(registry: &PgWorkflows) -> (usize, usize) {
    let now = chrono::Utc::now();
    let (mut inserted, mut present) = (0, 0);
    for spec in load_workflows(BUNDLE).expect("bundle parses") {
        let kind = spec.kind.clone();
        if registry.get_active(&kind).await.is_ok() {
            present += 1;
            continue;
        }
        registry
            .create_draft(spec, &seed_actor(), now)
            .await
            .expect("draft");
        registry
            .publish(&kind, &seed_actor(), now)
            .await
            .expect("publish");
        inserted += 1;
    }
    (inserted, present)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seed_inserts_the_bundle_then_stops() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
    let bundled = load_workflows(BUNDLE).expect("bundle parses");
    assert!(!bundled.is_empty(), "an empty bundle would prove nothing");

    // The schema seeds no platform workflows, so a fresh deployment
    // starts without them and the first run must supply every one.
    let (inserted, present) = seed(&registry).await;
    assert_eq!(
        inserted,
        bundled.len(),
        "first run inserts the whole bundle"
    );
    assert_eq!(present, 0);

    for spec in &bundled {
        let live = registry
            .get_active(&spec.kind)
            .await
            .expect("active after seeding");
        assert_eq!(live.status, WorkflowStatus::Active);
        assert_eq!(live.version, 1, "{}: a fresh insert is v1", spec.kind);
        assert_eq!(
            live.steps, spec.steps,
            "{}: seeded steps match the bundle",
            spec.kind
        );
    }

    // Second run: everything is present, so nothing is written. This is
    // the half that matters — a seed that "helpfully" republishes is
    // bootstrap_reconcile again, and reverting operator edits is the
    // behaviour being removed.
    let (inserted, present) = seed(&registry).await;
    assert_eq!(inserted, 0, "a second run must insert nothing");
    assert_eq!(present, bundled.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seed_leaves_an_operator_edit_alone() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
    let bundled = load_workflows(BUNDLE).expect("bundle parses");
    let target = bundled.first().expect("bundle has a row").clone();

    seed(&registry).await;

    // Someone edits the seeded protocol the way the UI does.
    let mut edited = registry.get_active(&target.kind).await.expect("seeded");
    edited.label = "Operator's own label".into();
    edited.status = WorkflowStatus::Draft;
    let editor = ActorId::Human("emp-david".into());
    let now = chrono::Utc::now();
    registry
        .create_draft(edited, &editor, now)
        .await
        .expect("draft");
    registry
        .publish(&target.kind, &editor, now)
        .await
        .expect("publish");

    // Booting again must not undo it.
    let (inserted, _) = seed(&registry).await;
    assert_eq!(inserted, 0, "the kind is present — nothing to insert");
    let live = registry
        .get_active(&target.kind)
        .await
        .expect("still active");
    assert_eq!(
        live.label, "Operator's own label",
        "the seed reverted an operator edit — the exact behaviour protocols-as-data removes"
    );
}
