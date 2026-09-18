//! The platform step-plugin bundle (`infra/platform/step-plugins/`)
//! declares exactly the ACTIVE rows the migrations produce — every
//! column — and can be the registry's only home.
//!
//! MEASURED 2026-09-18 on origin/main 6286857f (backlog 393d3234,
//! consolidation H4, car 2 of 4). Seven migrations were the only place a
//! step plugin's row was declared: 03-jobs.sql seeded seven kinds, 126
//! `answer-question`, 135 `sign-off` v1, 146 `correction-verdict`, 155
//! `incident-review`, 156 `scope-declaration`, and 202609082130 retired
//! sign-off below v3 and inserted v3 (v2 was published live through the
//! API and has no migration). Twelve active rows, and the live system of
//! record listed the same twelve at the same versions. The JS each row
//! points at already lived in the tree (`infra/step-plugins/`); only
//! the row did not. Stations made this move in car 1; this is the same
//! move for step plugins, on the same seed path.
//!
//! TWO PINS, in the order the move needs them:
//!
//!   1. The bundle is COMPLETE before it becomes the home: the active
//!      rows a TestDb holds after the migrations equal the rows the
//!      bundle declares, kind for kind and column for column
//!      (`created_at` excepted — it is when the deployment was built,
//!      not part of the declaration).
//!   2. The bundle can be the ONLY home: with the `step_plugins` table
//!      emptied, the seed's publish function recreates the migration
//!      rows exactly — same versions, same columns, active.
//!
//! And the refusal that makes editing safe: a bundle row that differs
//! from the live active row of the same (kind, version) is refused by
//! kind and field, and the live row is left untouched. A version bump
//! is the edit path, as it is for workflows and stations.

use boss_core::actor::ActorId;
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_step_plugins;
use boss_jobs::step_plugin_seed::{
    SeedOutcome, StepPluginSeedError, platform_step_plugins_path, seed_step_plugins,
};
use boss_jobs::{PgStepPlugins, StepPluginRegistry, StepPluginSpec};
use boss_testing::TestDb;
use std::collections::BTreeMap;

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// A row as a declaration: everything but `created_at`, keyed by kind.
fn declarations(rows: &[StepPluginSpec]) -> BTreeMap<String, serde_json::Value> {
    rows.iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).expect("a plugin serializes");
            v.as_object_mut()
                .expect("a plugin is an object")
                .remove("created_at");
            (s.kind.clone(), v)
        })
        .collect()
}

fn bundle() -> Vec<StepPluginSpec> {
    load_step_plugins(platform_step_plugins_path()).expect("the platform step-plugin bundle parses")
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, column for column, and the bundle declares nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let live = registry.list_active(None).await.expect("list_active");
    assert!(!live.is_empty(), "the migrations seed at least one plugin");

    let from_migrations = declarations(&live);
    let from_bundle = declarations(&bundle());

    let migration_kinds: Vec<&String> = from_migrations.keys().collect();
    let bundle_kinds: Vec<&String> = from_bundle.keys().collect();
    assert_eq!(
        migration_kinds, bundle_kinds,
        "the bundle's kinds must be exactly the migrations' active kinds \
         (a plugin the migrations seed and the bundle does not declare has no home \
         once migrations declare schema only)"
    );
    for (kind, migrated) in &from_migrations {
        let declared = &from_bundle[kind];
        assert_eq!(
            declared, migrated,
            "infra/platform/step-plugins/{kind}.toml must equal the active row the \
             migrations produce, every column (left = bundle, right = migrations)"
        );
    }
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// migration rows: same versions, same columns, all active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let before = registry.list_active(None).await.expect("list_active");
    let expected = declarations(&before);

    sqlx::query("DELETE FROM step_plugins")
        .execute(&db.pool)
        .await
        .expect("empty the step_plugins table");
    assert!(
        registry
            .list_active(None)
            .await
            .expect("list_active")
            .is_empty(),
        "the table is empty before the seed runs"
    );

    let report = seed_step_plugins(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Inserted)),
        expected.len(),
        "every bundle plugin is inserted into an empty registry: {report}"
    );

    let after = registry.list_active(None).await.expect("list_active");
    assert_eq!(
        declarations(&after),
        expected,
        "the seed must recreate the migration rows exactly — the bundle can be the only home"
    );
    for row in &after {
        assert_eq!(row.status, WorkflowStatus::Active, "{} is active", row.kind);
        let versions = registry
            .list_versions(&row.kind)
            .await
            .expect("list_versions");
        assert_eq!(
            versions.len(),
            1,
            "{} has exactly the declared version and no synthetic history: {:?}",
            row.kind,
            versions.iter().map(|v| v.version).collect::<Vec<_>>()
        );
    }
    let sign_off = registry
        .get_active("sign-off")
        .await
        .expect("sign-off is active");
    assert_eq!(
        sign_off.version, 3,
        "sign-off lands at the version the migrations produced, not at v1"
    );
}

/// On a registry the migrations already filled, the seed inserts
/// nothing and says every row is present — the insert-if-missing
/// posture every boot relies on.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let before = declarations(&registry.list_active(None).await.expect("list_active"));

    let report = seed_step_plugins(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Present)),
        before.len(),
        "every bundle row is already present: {report}"
    );
    assert_eq!(report.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let outbox: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind LIKE 'jobs.step_plugin.%'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("count plugin events");
    assert_eq!(outbox, 0, "a no-op seed records no plugin event");
    assert_eq!(
        declarations(&registry.list_active(None).await.expect("list_active")),
        before
    );
}

/// The refusal: a bundle row edited WITHOUT a version bump differs
/// from the live active row of the same (kind, version), and the seed
/// refuses it by kind and field rather than overwriting or ignoring.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_row_that_differs_from_the_live_row_is_refused_by_field() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let before = declarations(&registry.list_active(None).await.expect("list_active"));

    let mut specs = bundle();
    let edited = specs
        .iter_mut()
        .find(|s| s.kind == "checklist")
        .expect("the bundle declares the checklist plugin");
    edited.label = "Checklist — edited without a version bump".into();
    edited.category = "edited".into();

    let err = seed_step_plugins(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect_err("a drifted bundle row is refused");
    match &err {
        StepPluginSeedError::Refused { rows: refusals, .. } => {
            assert_eq!(refusals.len(), 1, "{err}");
            assert_eq!(refusals[0].name, "checklist");
            assert_eq!(refusals[0].version, 1);
            assert_eq!(
                refusals[0].fields,
                vec!["category".to_string(), "label".to_string()],
                "the refusal names every differing column"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }
    let text = err.to_string();
    assert!(
        text.contains("checklist")
            && text.contains("version")
            && text.contains("label")
            && text.contains("infra/platform/step-plugins/"),
        "the refusal must name the plugin, the edit path, the field and the bundle: {text}"
    );
    assert_eq!(
        declarations(&registry.list_active(None).await.expect("list_active")),
        before,
        "a refused seed writes nothing"
    );
}

/// A version bump IS the edit path: a bundle row one version ahead of
/// the live active row is published, retiring the live one, and the
/// declared version is the one that lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_bump_publishes_and_retires_the_live_row() {
    let db = TestDb::new().await;
    let registry = PgStepPlugins::new(db.pool.clone());
    let live = registry
        .get_active("checklist")
        .await
        .expect("checklist is active");

    let mut specs = bundle();
    let bumped = specs
        .iter_mut()
        .find(|s| s.kind == "checklist")
        .expect("the bundle declares the checklist plugin");
    bumped.version = live.version + 1;
    bumped.label = "Checklist v2".into();

    let report = seed_step_plugins(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect("a version bump publishes");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Published { .. })),
        1,
        "{report}"
    );

    let now_active = registry
        .get_active("checklist")
        .await
        .expect("checklist is active");
    assert_eq!(now_active.version, live.version + 1);
    assert_eq!(now_active.label, "Checklist v2");
    let old = registry
        .get_version("checklist", live.version)
        .await
        .expect("the prior version is history");
    assert_eq!(old.status, WorkflowStatus::Retired);

    let published: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind = 'jobs.step_plugin.published'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("count published events");
    assert_eq!(
        published, 1,
        "one jobs.step_plugin.published for the row written"
    );
}
