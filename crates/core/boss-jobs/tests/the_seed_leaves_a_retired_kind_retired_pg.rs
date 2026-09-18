//! The platform Workflow seed never reverts a retire: a kind whose
//! every live version is retired is PRESENT, not missing.
//!
//! MEASURED 2026-09-18 (backlog 8b2eaff2). The operator retired
//! `maintenance-deploy-confirm` on the live registry at 12:20Z (POST
//! /retire; GET answered 404, no active kind) and the next boot's seed
//! published it again at 12:42:24Z as v3, active —
//! /api/workflows/maintenance-deploy-confirm/versions read v1 retired,
//! v2 retired, v3 active. The seed read "present" as `get_active(kind)
//! .is_ok()`, so a retired kind whose bundle file was still in the tree
//! was missing by that reading and was re-inserted on every boot. A
//! retire is an operator's decision; re-activation is an explicit
//! publish, and the seed is neither.
//!
//! Three rows of the seed's table, against a real registry:
//!
//!   - a kind with a retired row and no active row, its file in the
//!     bundle: nothing is inserted, and the report says why —
//!     `retired (left alone)`;
//!   - a kind with no row at all: inserted (the behaviour every fresh
//!     database relies on);
//!   - a kind with an active row: present, untouched.

use boss_core::actor::ActorId;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowStatus, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::workflow_seed::{SeedOutcome, seed_workflows};
use boss_testing::TestDb;

fn seed_actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

fn operator() -> ActorId {
    ActorId::Human("emp-david".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_kind_with_a_bundle_file_is_left_retired() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());
    let bundle = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    assert!(bundle.len() >= 2, "an empty bundle would prove nothing");
    let now = chrono::Utc::now();

    // A fresh database: every bundle kind has no row and is inserted.
    let report = seed_workflows(&registry, &bundle, &seed_actor(), now, false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| *o == SeedOutcome::Inserted),
        bundle.len(),
        "a kind with no row at all is inserted: {report}"
    );

    // The operator retires one kind, as they did at 12:20Z.
    let retired = &bundle[0].kind;
    registry
        .retire(retired, &operator(), now)
        .await
        .expect("the operator's retire");
    assert!(
        matches!(
            registry.get_active(retired).await,
            Err(boss_jobs::registry::WorkflowError::NotFound(_))
        ),
        "no active row after the retire"
    );

    // The next boot: the file is still in the bundle. The seed must
    // not publish it again.
    let report = seed_workflows(&registry, &bundle, &seed_actor(), now, false)
        .await
        .expect("a retired kind is not a failure");
    let row = report
        .rows
        .iter()
        .find(|r| r.name == *retired)
        .expect("the retired kind is in the report");
    assert_eq!(
        row.outcome,
        SeedOutcome::Retired { newest: 1 },
        "the seed re-published a retired kind — the 12:42Z revert: {report}"
    );
    assert!(
        report
            .to_string()
            .contains(&format!("{retired}: retired (left alone)")),
        "the report names the kind as retired (left alone): {report}"
    );
    assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 0);
    assert_eq!(
        report.count(|o| *o == SeedOutcome::Present),
        bundle.len() - 1,
        "every other kind is present, untouched: {report}"
    );

    let versions = registry.list_versions(retired).await.expect("versions");
    assert_eq!(
        versions
            .iter()
            .map(|v| (v.version, v.status))
            .collect::<Vec<_>>(),
        vec![(1, WorkflowStatus::Retired)],
        "the lineage is exactly what the operator left: v1 retired, nothing after it"
    );
    assert!(
        matches!(
            registry.get_active(retired).await,
            Err(boss_jobs::registry::WorkflowError::NotFound(_))
        ),
        "still no active row after the boot"
    );
}
