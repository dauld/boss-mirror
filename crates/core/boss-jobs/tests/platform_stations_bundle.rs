//! The platform station bundle — `infra/platform/stations/`, one
//! `<name>.toml` per station — is well-formed, viable and complete, read
//! with no database.
//!
//! WHY (backlog 393d3234, consolidation H4, car 1 of 4). Until
//! 2026-09-18 seven migrations were the only place a platform station
//! was declared. The bundle is now the home: the seed publishes it
//! insert-if-missing by (name, version) at every start, and
//! `infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
//! stations` in any migration newer than the cutover. The equality pin
//! against the migrations' own rows is
//! `the_stations_bundle_is_the_migrations_pg.rs`; this file holds the
//! rules that need no database and range over every file.
//!
//! The `[[station]]` shape is authored, not generated — `StationSpec`
//! does not serialize to TOML (TOML has no null) — and the loader
//! refuses a file named for a station it does not hold, a file holding
//! two, a status other than `active`, and a row the viability lint
//! rejects. The five names below are the ones the migrations seeded;
//! a station added later is a file dropped in and a line here.

use boss_jobs::seed_loader::{bundle_files, load_stations};
use boss_jobs::station_seed::platform_stations_path;
use boss_jobs::{StationRegistry, StationSpec};
use std::path::Path;

fn bundle() -> Vec<StationSpec> {
    load_stations(platform_stations_path()).expect("the platform station bundle parses")
}

/// One station per file, and the file is named for it, so `ls` answers
/// "which stations does a deployment seed".
#[test]
fn the_bundle_is_one_file_per_station() {
    let dir = Path::new(platform_stations_path());
    let files = bundle_files(dir).expect("the bundle directory lists its station files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a station file has a UTF-8 stem");
        let rows = load_stations(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let names: Vec<&str> = rows.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [stem],
            "{} holds exactly its own station",
            file.display()
        );
    }
    let mut names: Vec<String> = bundle().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "design-review",
            "loading-dock",
            "my-watchlist",
            "q.platform-admin.task",
            "repair",
        ],
        "the five platform stations the migrations seeded, and no other"
    );
}

/// Every bundled row passes the same viability gate an API publish
/// passes (`station_lint::gate_active`) and can be published as-is
/// into an empty registry at its declared version.
#[tokio::test]
async fn every_bundled_station_is_viable_and_publishable() {
    let registry = boss_jobs::stations::InMemoryStations::new();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    for spec in bundle() {
        boss_jobs::station_lint::gate_active(&spec)
            .unwrap_or_else(|problems| panic!("{} is not viable: {problems:?}", spec.name));
        let declared = spec.version;
        let published = registry
            .publish_declared(spec, &actor, now)
            .await
            .expect("a bundle row publishes");
        assert_eq!(published.version, declared, "the declared version lands");
    }
}

/// A file that declares anything but an active row, or a version below
/// 1, is refused by the loader with the station named — a bundle row is
/// what a fresh deployment gets, and that is an active row.
#[test]
fn a_bundle_row_must_be_active_at_a_real_version() {
    let retired = "\
[[station]]
name = \"x\"
version = 1
status = \"retired\"
title = \"X\"
kind = \"batch\"
[station.predicate]
kind = \"ship-a-change\"
";
    let err = boss_jobs::seed_loader::parse_stations(retired, "x.toml")
        .expect_err("a retired declaration is refused");
    assert!(err.to_string().contains("`x`"), "{err}");
    assert!(err.to_string().contains("retired"), "{err}");

    let err = boss_jobs::seed_loader::parse_stations(
        &retired
            .replace("version = 1", "version = 0")
            .replace("retired", "active"),
        "x.toml",
    )
    .expect_err("version 0 is refused");
    assert!(err.to_string().contains("version"), "{err}");
}
