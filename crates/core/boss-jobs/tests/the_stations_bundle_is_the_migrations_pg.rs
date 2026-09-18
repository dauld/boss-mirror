//! The platform station bundle (`infra/platform/stations/`) declares
//! exactly the ACTIVE rows the migrations produce — every column — and
//! can be the registry's only home.
//!
//! MEASURED 2026-09-18 on origin/main 1a09f660 (backlog 393d3234,
//! consolidation H4, car 1 of 4). Seven migrations were the only place
//! a platform station was declared: 116 seeded `loading-dock` and
//! `design-review`, 118 `my-watchlist`, 124 `repair`, 130 / 133 /
//! 20260910201453 bumped two of them, 20260915023614 authored
//! `q.platform-admin.task`, and 119 / 138 / 139 / 202609101900 UPDATEd
//! columns in place. A migration is the wrong home for a registry row:
//! it runs once, a fresh instance cannot re-declare it without replaying
//! history, nothing drift-checks it against the live row, and every
//! edit is a contended timestamped file. Dispatcher rules left that
//! shape on 2026-09-11 and the Workflow bundle before them; this is the
//! same move for stations.
//!
//! TWO PINS, in the order the move needs them:
//!
//!   1. The bundle is COMPLETE before it becomes the home: the active
//!      rows a TestDb holds after the migrations equal the rows the
//!      bundle declares, name for name and column for column
//!      (`created_at` excepted — it is when the deployment was built,
//!      not part of the declaration).
//!   2. The bundle can be the ONLY home: with the `stations` table
//!      emptied, the seed's publish function recreates the migration
//!      rows exactly — same versions, same columns, active.
//!
//! And the refusal that makes editing safe: a bundle row that differs
//! from the live active row of the same (name, version) is refused by
//! name and field, and the live row is left untouched. A version bump
//! is the edit path, as it is for workflows.

use boss_core::actor::ActorId;
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_stations;
use boss_jobs::station_seed::{
    SeedOutcome, StationSeedError, platform_stations_path, seed_stations,
};
use boss_jobs::{PgStations, StationRegistry, StationSpec};
use boss_testing::TestDb;
use std::collections::BTreeMap;

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// A row as a declaration: everything but `created_at`, keyed by name.
fn declarations(rows: &[StationSpec]) -> BTreeMap<String, serde_json::Value> {
    rows.iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).expect("a station serializes");
            v.as_object_mut()
                .expect("a station is an object")
                .remove("created_at");
            (s.name.clone(), v)
        })
        .collect()
}

fn bundle() -> Vec<StationSpec> {
    load_stations(platform_stations_path()).expect("the platform station bundle parses")
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, column for column, and the bundle declares nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let live = registry.list_active().await.expect("list_active");
    assert!(!live.is_empty(), "the migrations seed at least one station");

    let from_migrations = declarations(&live);
    let from_bundle = declarations(&bundle());

    let migration_names: Vec<&String> = from_migrations.keys().collect();
    let bundle_names: Vec<&String> = from_bundle.keys().collect();
    assert_eq!(
        migration_names, bundle_names,
        "the bundle's station names must be exactly the migrations' active names \
         (a station the migrations seed and the bundle does not declare has no home \
         once migrations declare schema only)"
    );
    for (name, migrated) in &from_migrations {
        let declared = &from_bundle[name];
        assert_eq!(
            declared, migrated,
            "infra/platform/stations/{name}.toml must equal the active row the \
             migrations produce, every column (left = bundle, right = migrations)"
        );
    }
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// migration rows: same versions, same columns, all active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let before = registry.list_active().await.expect("list_active");
    let expected = declarations(&before);

    sqlx::query("DELETE FROM stations")
        .execute(&db.pool)
        .await
        .expect("empty the stations table");
    assert!(
        registry
            .list_active()
            .await
            .expect("list_active")
            .is_empty(),
        "the table is empty before the seed runs"
    );

    let report = seed_stations(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Inserted)),
        expected.len(),
        "every bundle station is inserted into an empty registry: {report}"
    );

    let after = registry.list_active().await.expect("list_active");
    assert_eq!(
        declarations(&after),
        expected,
        "the seed must recreate the migration rows exactly — the bundle can be the only home"
    );
    for row in &after {
        assert_eq!(row.status, WorkflowStatus::Active, "{} is active", row.name);
        let versions = registry
            .list_versions(&row.name)
            .await
            .expect("list_versions");
        assert_eq!(
            versions.len(),
            1,
            "{} has exactly the declared version and no synthetic history: {:?}",
            row.name,
            versions.iter().map(|v| v.version).collect::<Vec<_>>()
        );
    }
}

/// On a registry the migrations already filled, the seed inserts
/// nothing and says every row is present — the insert-if-missing
/// posture every boot relies on.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let before = declarations(&registry.list_active().await.expect("list_active"));

    let report = seed_stations(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Present)),
        before.len(),
        "every bundle row is already present: {report}"
    );
    assert_eq!(report.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let outbox: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind LIKE 'jobs.station.%'")
            .fetch_one(&db.pool)
            .await
            .expect("count station events");
    assert_eq!(outbox, 0, "a no-op seed records no station event");
    assert_eq!(
        declarations(&registry.list_active().await.expect("list_active")),
        before
    );
}

/// The refusal: a bundle row edited WITHOUT a version bump differs
/// from the live active row of the same (name, version), and the seed
/// refuses it by name and field rather than overwriting or ignoring.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_row_that_differs_from_the_live_row_is_refused_by_field() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let before = declarations(&registry.list_active().await.expect("list_active"));

    let mut specs = bundle();
    let edited = specs
        .iter_mut()
        .find(|s| s.name == "repair")
        .expect("the bundle declares the repair station");
    edited.title = "Repair — edited without a version bump".into();
    edited.wip_limit = Some(3);

    let err = seed_stations(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect_err("a drifted bundle row is refused");
    match &err {
        StationSeedError::Refused(refusals) => {
            assert_eq!(refusals.len(), 1, "{err}");
            assert_eq!(refusals[0].name, "repair");
            assert_eq!(refusals[0].version, 1);
            assert_eq!(
                refusals[0].fields,
                vec!["title".to_string(), "wip_limit".to_string()],
                "the refusal names every differing column"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }
    let text = err.to_string();
    assert!(
        text.contains("repair") && text.contains("version") && text.contains("title"),
        "the refusal must name the station, the edit path and the field: {text}"
    );
    assert_eq!(
        declarations(&registry.list_active().await.expect("list_active")),
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
    let registry = PgStations::new(db.pool.clone());
    let live = registry
        .get_active("repair")
        .await
        .expect("repair is active");

    let mut specs = bundle();
    let bumped = specs
        .iter_mut()
        .find(|s| s.name == "repair")
        .expect("the bundle declares the repair station");
    bumped.version = live.version + 1;
    bumped.wip_limit = Some(3);

    let report = seed_stations(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect("a version bump publishes");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Published { .. })),
        1,
        "{report}"
    );

    let now_active = registry
        .get_active("repair")
        .await
        .expect("repair is active");
    assert_eq!(now_active.version, live.version + 1);
    assert_eq!(now_active.wip_limit, Some(3));
    let old = registry
        .get_version("repair", live.version)
        .await
        .expect("the prior version is history");
    assert_eq!(old.status, WorkflowStatus::Retired);
}
