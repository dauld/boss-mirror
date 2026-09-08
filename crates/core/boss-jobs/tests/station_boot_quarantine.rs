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

use std::io::Write;
use std::sync::{Arc, Mutex};

use boss_core::job::JobStatus;
use boss_jobs::events::{STATION_QUARANTINED, STATION_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_quarantine::check_active_stations_viable;
use boss_jobs::station_queue::{SELF, StationPredicate};
use boss_jobs::{InMemoryStations, StationKind, StationRegistry, StationSpec};
use std::collections::BTreeMap;

fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
}

/// Seeded straight to ACTIVE, exactly as the SQL migrations do —
/// bypassing publish, which is the whole point of this pass.
fn seed_active(stations: &InMemoryStations, mut spec: StationSpec) {
    spec.status = WorkflowStatus::Active;
    stations.seed(spec).unwrap();
}

fn viable(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "A real queue",
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        },
        now(),
    )
}

/// A contradiction: the same key demanded present and absent.
fn always_empty(name: &str) -> StationSpec {
    let mut s = viable(name);
    s.predicate.metadata_present = vec!["train".into()];
    s.predicate.metadata_absent = vec!["train".into()];
    s
}

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

/// A `MakeWriter` that collects everything `tracing_subscriber::fmt`
/// prints, so a test can read the boot log the way an operator would.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
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

/// The log is the whole value of this pass, so it is asserted like
/// the report: the ERROR line names the station, its version, and
/// says in words that nothing was retired.
#[tokio::test]
async fn the_error_line_names_the_station_and_says_it_was_not_retired() {
    let stations = Arc::new(InMemoryStations::new());
    seed_active(&stations, always_empty("broken"));

    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("boot check completes");
    drop(_guard);

    let text = log.text();
    assert!(
        text.contains("ERROR"),
        "an unviable active station is logged at ERROR: {text}"
    );
    assert!(
        text.contains("name=broken") && text.contains("version=1"),
        "the ERROR line names the station and its version: {text}"
    );
    assert!(
        text.contains("NOT retired at boot") && text.contains("quarantine is a deliberate act"),
        "the ERROR line says what boot did not do and who must: {text}"
    );
    assert!(
        text.contains("train"),
        "the problems that condemned the row are in the log, one line each: {text}"
    );

    assert_untouched(&stations, "broken").await;
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
