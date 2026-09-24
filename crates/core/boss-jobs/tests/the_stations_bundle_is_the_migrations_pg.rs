//! The platform station bundle (`infra/platform/stations/`) declares
//! every ACTIVE row the migrations produce — at that version, column for
//! column, or at a later version — and can be the registry's only home.
//! Since 2026-09-24 it LEADS them: `design-review` v2 and the new
//! `design-decided` exist in the bundle alone (backlog 08372fdb), the
//! edit path migrations-declare-schema-only leaves.
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
//!      emptied, the seed's publish function recreates the bundle
//!      exactly — same versions, same columns, active.
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

/// The bundle rows that sit AHEAD of the live registry: `(ahead, new)`
/// — rows at a higher version than the live active row of their name,
/// which a seed PUBLISHES, and names with no live row at all, which it
/// INSERTS.
///
/// Derived, never assumed. The pins below took both as zero until
/// 2026-09-24, which was true only while no station had changed since
/// the cutover; the first bundle-only change (design-review v2 and the
/// new design-decided, backlog 08372fdb) made them one each. The cadence
/// pin met the same thing on its first bump (backlog cab50f4c) and this
/// follows the rule it settled: the bundle may lead, never trail.
fn ahead_of(live: &[StationSpec]) -> (usize, usize) {
    let live: BTreeMap<&str, i32> = live.iter().map(|s| (s.name.as_str(), s.version)).collect();
    bundle()
        .iter()
        .fold((0, 0), |(ahead, new), s| match live.get(s.name.as_str()) {
            Some(v) if s.version > *v => (ahead + 1, new),
            Some(_) => (ahead, new),
            None => (ahead, new + 1),
        })
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, at its version or AHEAD of it, and where the versions match,
/// column for column.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let live = registry.list_active().await.expect("list_active");
    assert!(!live.is_empty(), "the migrations seed at least one station");

    let from_migrations = declarations(&live);
    let from_bundle = declarations(&bundle());

    // EVERY MIGRATION NAME HAS A FILE; THE BUNDLE MAY HOLD MORE. This
    // asserted the two name lists equal until 2026-09-24, which forbade
    // the edit path the cutover created: once migrations declare schema
    // only, a station added later exists in the bundle and nowhere else
    // (design-decided, backlog 08372fdb). What the check was FOR still
    // holds — a station the migrations seed and the bundle does not
    // declare has no home — and that is the direction kept.
    let homeless: Vec<&String> = from_migrations
        .keys()
        .filter(|name| !from_bundle.contains_key(*name))
        .collect();
    assert!(
        homeless.is_empty(),
        "these stations are seeded by the migrations and declared by no file under \
         infra/platform/stations/ — they have no home once migrations declare schema \
         only: {homeless:?}"
    );
    // THE BUNDLE MAY BE AHEAD; IT MAY NEVER BE BEHIND; AND WHERE THE
    // VERSIONS MATCH, EVERY COLUMN MUST AGREE — the rule the cadence pin
    // settled on its own first bump (backlog cab50f4c, David chose the
    // lint over plain equality).
    for (name, migrated) in &from_migrations {
        let declared = &from_bundle[name];
        let dv = declared["version"].as_i64().expect("a bundle version");
        let mv = migrated["version"].as_i64().expect("a migration version");
        assert!(
            dv >= mv,
            "infra/platform/stations/{name}.toml declares v{dv}, BEHIND the v{mv} the \
             migrations produce — a fresh database would seed the older row and history \
             would silently win"
        );
        if dv == mv {
            assert_eq!(
                declared, migrated,
                "infra/platform/stations/{name}.toml is at the migrations' version but \
                 differs from it — a column changed without the version bump that says so \
                 (left = bundle, right = migrations)"
            );
        }
    }
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// bundle: same versions, same columns, all active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    // WHAT THE SEED MUST REBUILD IS THE BUNDLE, because the bundle is the
    // home. Reading the expectation off the migrations would assert that
    // an emptied registry comes back as HISTORY rather than as the
    // current declaration (backlog cab50f4c, the cadence pin's same fix).
    let expected = declarations(&bundle());

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
        "the seed must recreate the BUNDLE exactly — the bundle can be the only home"
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

/// On a registry the migrations already filled, the seed publishes
/// exactly the rows the bundle moved ahead, inserts exactly the
/// stations only the bundle declares, and leaves every other row alone;
/// a second boot then finds every row present and writes nothing — the
/// insert-if-missing posture every boot relies on.
///
/// Until 2026-09-24 this asserted that the FIRST seed found everything
/// present, which assumed no station had changed since the cutover. The
/// first bundle-only change (backlog 08372fdb) moved one row ahead and
/// added one name, so the property is now stated on the boot AFTER the
/// one that delivers the change — which is the boot every later restart
/// is.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let migrated = registry.list_active().await.expect("list_active");
    let before = declarations(&migrated);
    // BEFORE the seed, because the seed is what changes it.
    let (ahead, new) = ahead_of(&migrated);

    let first = seed_stations(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        first.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len() - ahead - new,
        "every bundle row at the live version is already present: {first}"
    );
    assert_eq!(
        first.count(|o| matches!(o, SeedOutcome::Published { .. })),
        ahead,
        "exactly the rows the bundle moved ahead are published: {first}"
    );
    assert_eq!(
        first.count(|o| matches!(o, SeedOutcome::Inserted)),
        new,
        "exactly the stations only the bundle declares are inserted: {first}"
    );
    // The rows that did NOT move are untouched, declaration for
    // declaration — which is what this pin is named for.
    let after_first = declarations(&registry.list_active().await.expect("list_active"));
    for (name, was) in &before {
        let moved = bundle()
            .iter()
            .any(|s| &s.name == name && Some(i64::from(s.version)) > was["version"].as_i64());
        if !moved {
            assert_eq!(
                after_first.get(name),
                Some(was),
                "{name} did not move in the bundle and must be untouched by the seed"
            );
        }
    }

    let events = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM event_outbox WHERE kind LIKE 'jobs.station.%'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("count station events")
    };
    let events_before = events().await;
    let second = seed_stations(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        second.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len(),
        "on the next boot every bundle row is present: {second}"
    );
    assert_eq!(
        events().await,
        events_before,
        "a no-op seed records no station event"
    );
    assert_eq!(
        declarations(&registry.list_active().await.expect("list_active")),
        after_first
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
        StationSeedError::Refused { rows: refusals, .. } => {
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
    // Bring the registry to the bundle first, so the ONE bump this test
    // makes is the only thing the seed below can publish — the bundle
    // itself may lead the migrations (see `ahead_of`).
    seed_stations(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the bundle seeds over the migrations");
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
