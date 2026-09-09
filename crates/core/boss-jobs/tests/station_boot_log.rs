//! The ERROR line the boot station check prints — asserted in a
//! process of its own.
//!
//! **This file holds exactly one test, and that is the point.**
//! `tracing` keeps its callsite-interest cache and its dynamic
//! max-level in process-global state, while
//! `tracing::subscriber::set_default` installs a subscriber for one
//! THREAD. `cargo test` runs a binary's tests on many threads at
//! once, so a sibling test that reaches the same `error!` callsite
//! with no subscriber on its thread can leave that callsite resolved
//! against no subscriber for everyone — and the capturing test then
//! reads a log that is missing lines, or empty.
//!
//! That is not hypothetical. This assertion lived alongside four
//! sibling tests in `station_boot_quarantine` and failed roughly one
//! run in twenty-five: measured 1/25 there, 0/60 alone. It reddened
//! train #281 on 2026-09-09, striking six cars that had nothing to do
//! with it, and it had passed every one of their gates.
//!
//! Two things keep it deterministic, and a new test in this file
//! would cost both: the file is a binary of its own, so nothing runs
//! beside it; and the subscriber is installed GLOBALLY before the
//! first event, so no thread can miss it. Assert the boot log
//! somewhere else and you are back to one-in-twenty-five.

mod common;

use std::sync::Arc;

use boss_jobs::events::{STATION_QUARANTINED, STATION_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_quarantine::check_active_stations_viable;
use boss_jobs::{InMemoryStations, StationRegistry};

use common::{Captured, always_empty, seed_active};

/// The log is the whole value of this pass, so it is asserted like
/// the report: the ERROR line names the station, its version, and
/// says in words that nothing was retired.
#[tokio::test]
async fn the_error_line_names_the_station_and_says_it_was_not_retired() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::ERROR)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let stations = Arc::new(InMemoryStations::new());
    seed_active(&stations, always_empty("broken"));

    check_active_stations_viable(stations.as_ref() as &dyn StationRegistry)
        .await
        .expect("boot check completes");

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

    // Boot logged and touched nothing — the same contract the
    // report-side tests pin, restated here because this binary is
    // the only place the log is read.
    assert_eq!(
        stations.get_active("broken").await.unwrap().status,
        WorkflowStatus::Active,
        "`broken` must still be active after boot"
    );
    let kinds: Vec<String> = stations
        .recorded_events()
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert!(
        !kinds
            .iter()
            .any(|k| k == STATION_RETIRED || k == STATION_QUARANTINED),
        "boot must not retire or quarantine anything: {kinds:?}"
    );
}
