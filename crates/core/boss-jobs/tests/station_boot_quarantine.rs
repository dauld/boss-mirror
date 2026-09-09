//! Boot-time viability check of ACTIVE stations — what boot owes the
//! operator when a seeded queue can never match, and what it may NOT
//! do about it.
//!
//! `station_lint::gate_active` closes the publish edge, but the rows
//! that actually exist in a deployed cluster got there by SQL seed
//! (`116-stations.sql`, `118-watchlist-station.sql`) and never passed
//! through publish at all. This pass is what covers them.
//!
//! Until 2026-09-08 this pass RETIRED each unviable row at boot — a
//! persisted, non-idempotent write from a boot path, the same defect
//! class that took the system of record down twice on 2026-09-07
//! through the Workflow pass (outage packet 7752e636). The argument
//! for it was that nothing is pinned to a station version, so
//! retiring strands nothing. That is true and beside the point: a
//! boot check that ACTS on a data condition has taken an action
//! nobody asked for, and it takes it again on every roll.
//!
//! The contract now, the same one the Workflow pass keeps: boot
//! CHECKS and LOGS. It never writes. Quarantine is a deliberate act
//! (retire through the registry), not a boot side-effect. These tests
//! pin that the two passes read the same.

mod common;

use std::sync::Arc;

use boss_jobs::events::{STATION_QUARANTINED, STATION_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_quarantine::check_active_stations_viable;
use boss_jobs::station_queue::{SELF, StationPredicate};
use boss_jobs::{InMemoryStations, StationKind, StationRegistry, StationSpec};
use std::collections::BTreeMap;

use common::{always_empty, now, seed_active, viable};

/// The row is still active afterwards, and no retirement or
/// quarantine marker was recorded anywhere — boot touched nothing.
async fn assert_untouched(stations: &InMemoryStations, name: &str) {
    assert_eq!(
        stations
            .get_active(name)
            .await
            .unwrap_or_else(|e| panic!("`{name}` must still be active after boot: {e}"))
            .status,
        WorkflowStatus::Active,
        "`{name}` must still be active after boot"
    );
    let recorded = stations.recorded_events();
    let kinds: Vec<&str> = recorded.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        !kinds.contains(&STATION_RETIRED),
        "boot must not retire anything: {kinds:?}"
    );
    assert!(
        !kinds.contains(&STATION_QUARANTINED),
        "boot must not write a quarantine marker: {kinds:?}"
    );
}

#[tokio::test]
async fn a_clean_registry_reports_nothing_and_is_left_alone() {
    let stations = Arc::new(InMemoryStations::new());
    seed_active(&stations, viable("dock"));
    seed_active(&stations, viable("review"));

    let report = check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("boot check completes");

    assert_eq!(report.checked, 2);
    assert!(report.unviable.is_empty());
    assert_eq!(stations.list_active().await.unwrap().len(), 2);
    assert!(
        stations.recorded_events().is_empty(),
        "boot wrote nothing at all"
    );
}

#[tokio::test]
async fn a_seeded_row_that_can_never_match_is_reported_and_left_untouched() {
    let stations = Arc::new(InMemoryStations::new());
    seed_active(&stations, viable("dock"));
    seed_active(&stations, always_empty("broken"));

    let report = check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("an unviable row is a report, not a refusal");

    assert_eq!(report.checked, 2);
    assert_eq!(report.unviable.len(), 1);
    assert_eq!(report.unviable[0].name, "broken");
    assert_eq!(report.unviable[0].version, 1);
    assert!(
        report.unviable[0]
            .problems
            .iter()
            .any(|p| p.reason.contains("train")),
        "the report carries the problems the log printed: {:?}",
        report.unviable[0].problems
    );

    // Until 2026-09-08 this is the row boot retired. It must not.
    assert_untouched(&stations, "broken").await;
    let live = stations.list_active().await.unwrap();
    assert_eq!(
        live.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["broken", "dock"],
        "both rows are still serving; the finding is in the log, not the registry"
    );
    assert!(
        stations.recorded_events().is_empty(),
        "boot wrote nothing at all"
    );
}

#[tokio::test]
async fn a_station_that_lies_about_being_personal_is_reported_not_retired() {
    // The shape that made the census misreport orphaned packets: an
    // `actor` row every executor sees identically.
    let stations = Arc::new(InMemoryStations::new());
    let mut impostor = viable("my-queue");
    impostor.kind = StationKind::Actor;
    seed_active(&stations, impostor);

    let report = check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("boot check completes");
    assert_eq!(report.unviable.len(), 1);
    assert_eq!(report.unviable[0].name, "my-queue");
    assert_untouched(&stations, "my-queue").await;
}

#[tokio::test]
async fn the_real_watchlist_row_survives_the_pass() {
    // 118-watchlist-station.sql, as deployed. A regression here would
    // flag a live queue at ERROR on every pod roll.
    let stations = Arc::new(InMemoryStations::new());
    seed_active(
        &stations,
        StationSpec::draft(
            "my-watchlist",
            "My watchlist",
            StationKind::Actor,
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
                ..Default::default()
            },
            now(),
        ),
    );

    let report = check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("boot check completes");
    assert!(
        report.unviable.is_empty(),
        "the deployed watchlist row must pass: {:?}",
        report.unviable
    );
    assert_eq!(stations.list_active().await.unwrap().len(), 1);
}
