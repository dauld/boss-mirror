//! The platform Workflow seed inserts what is missing and touches
//! nothing else.
//!
//! protocols-as-data Q1, as David answered it: "the seed binary inserts
//! what is missing and touches nothing that exists ... Drift-healing
//! goes away deliberately: it is the feature that reverts operator
//! edits." Both halves are load-bearing and both are asserted here —
//! the insert, and the not-touching.
//!
//! This exercises `boss_jobs::workflow_seed::seed_workflows` — the
//! function the binary calls. Until 2026-09-18 (backlog 8b2eaff2) it
//! carried its own inline copy of the seed's loop, which asserted the
//! same wrong reading of "present" the binary had and so passed while
//! the binary reverted an operator's retire on every boot. A test that
//! restates the logic it tests pins nothing.

use boss_core::actor::ActorId;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::workflow_seed::{SeedOutcome, SeedReport, seed_workflows};
use boss_testing::TestDb;

fn seed_actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// The seed as the binary runs it, minus the argument parsing.
async fn seed(registry: &PgWorkflows) -> SeedReport {
    let bundle = load_workflows(platform_bundle_path()).expect("bundle parses");
    seed_workflows(registry, &bundle, &seed_actor(), chrono::Utc::now(), false)
        .await
        .expect("the platform bundle seeds")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seed_inserts_the_bundle_then_stops() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
    let bundled = load_workflows(platform_bundle_path()).expect("bundle parses");
    assert!(!bundled.is_empty(), "an empty bundle would prove nothing");

    // The schema seeds no platform workflows, so a fresh deployment
    // starts without them and the first run must supply every one.
    let report = seed(&registry).await;
    assert_eq!(
        report.count(|o| *o == SeedOutcome::Inserted),
        bundled.len(),
        "first run inserts the whole bundle: {report}"
    );
    assert_eq!(report.count(|o| *o == SeedOutcome::Present), 0);

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
    let report = seed(&registry).await;
    assert_eq!(
        report.count(|o| *o == SeedOutcome::Inserted),
        0,
        "a second run must insert nothing: {report}"
    );
    assert_eq!(report.count(|o| *o == SeedOutcome::Present), bundled.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seed_leaves_an_operator_edit_alone() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
    let bundled = load_workflows(platform_bundle_path()).expect("bundle parses");
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
    let report = seed(&registry).await;
    assert_eq!(
        report.count(|o| *o == SeedOutcome::Inserted),
        0,
        "the kind is present — nothing to insert: {report}"
    );
    let live = registry
        .get_active(&target.kind)
        .await
        .expect("still active");
    assert_eq!(
        live.label, "Operator's own label",
        "the seed reverted an operator edit — the exact behaviour protocols-as-data removes"
    );
}
