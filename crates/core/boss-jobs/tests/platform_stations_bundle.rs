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
            "design-decided",
            "design-review",
            "loading-dock",
            "my-watchlist",
            "q.platform-admin.task",
            "repair",
        ],
        "the five platform stations the migrations seeded, plus `design-decided` \
         (declared here only, 2026-09-24, backlog 08372fdb), and no other"
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

// ---------------------------------------------------------------------------
// /it/design — IN, WORKING and OUT, as the two design stations declare them
// ---------------------------------------------------------------------------
//
// WHY (backlog 08372fdb, from page audit 79db48ae, 2026-09-23). /it/design
// rendered one station, `design-review`, whose lens asked for no steps, so
// three things were invisible: a review saved but not completed looked
// exactly like one never touched (the review step stays `ready` through a
// Save — only a `pending` step is flipped — so its answers are the only
// trace, and they live on the step); a design decided and waiting at
// `fold` vanished the moment its review completed; and nothing settled
// was shown at all — 34 designs closed on 2026-09-23 alone. These pin
// what the DATA must hand the page for each of the three, evaluated the
// way `GET /api/stations/{name}/queue` evaluates it, with no database.

use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use chrono::NaiveDate;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("a real September day")
}

fn station(name: &str) -> StationSpec {
    bundle()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("the bundle declares `{name}`"))
}

/// A design-doc packet opened on `day(1)` with its review and fold steps
/// in the given states — the two steps the stations read.
fn design(review: StepStatus, fold: StepStatus) -> (Job, Vec<Step>) {
    let mut job = Job::new(
        "design-doc",
        Subject::new("custom", "boss-platform"),
        "a design",
        "agent-claude",
        Priority::Standard,
        day(1),
    );
    job.status = JobStatus::Open;
    let mut r = Step::new(job.id, "review-design", "review", 1);
    r.spec_slug = Some("review".into());
    r.status = review;
    let mut f = Step::new(job.id, "task", "fold", 2);
    f.spec_slug = Some("fold".into());
    f.status = fold;
    (job, vec![r, f])
}

fn closed_on(mut packet: (Job, Vec<Step>), on: u32) -> (Job, Vec<Step>) {
    packet.0.status = JobStatus::Closed;
    packet.0.closed_on = Some(day(on));
    packet
}

/// IN — the review queue carries its members' steps, so a row can say
/// "saved, 2 of 3 answered" from the review step's own `resolutions`
/// instead of rendering a half-made decision as an untouched one.
#[test]
fn the_review_queue_carries_the_review_step_it_is_waiting_on() {
    let spec = station("design-review");
    let lens = spec.lens.as_ref().expect("design-review declares a lens");
    assert!(
        lens.with_steps,
        "design-review must carry steps: a saved review is visible only on its step"
    );
    assert!(
        lens.panels.iter().any(|p| p == "queue") && lens.panels.iter().any(|p| p == "decided"),
        "the page renders the queue AND the decided designs: {:?}",
        lens.panels
    );

    let waiting = design(StepStatus::Ready, StepStatus::Pending);
    let id = waiting.0.id.to_string();
    let q = boss_jobs::evaluate_station(&spec, vec![waiting], day(24));
    assert_eq!(q.total, 1);
    let steps = q
        .steps
        .get(&id)
        .expect("the member's steps ride the envelope");
    assert!(
        steps
            .iter()
            .any(|s| s.spec_slug.as_deref() == Some("review")),
        "the review step is among them"
    );
}

/// WORKING and OUT — `design-decided` holds every design whose review
/// completed: open ones are at `fold` (decided, not yet landed), closed
/// ones stay for the station's terminal window and then leave. A design
/// still under review is not decided and is not held here.
#[test]
fn the_decided_station_holds_folds_in_flight_and_recently_settled_designs() {
    let spec = station("design-decided");
    let window = spec
        .terminal_window_days
        .expect("design-decided declares a terminal window — OUT is its reason to exist");
    assert!(
        spec.lens.as_ref().is_some_and(|l| l.with_steps),
        "the fold step's status and `folded_into` are what the rows show"
    );

    let under_review = design(StepStatus::Ready, StepStatus::Pending);
    let at_fold = design(StepStatus::Completed, StepStatus::Active);
    let settled = closed_on(design(StepStatus::Completed, StepStatus::Completed), 20);
    let long_gone = closed_on(
        design(StepStatus::Completed, StepStatus::Completed),
        20 - window - 1,
    );
    let (fold_id, settled_id) = (at_fold.0.id.to_string(), settled.0.id.to_string());

    let q = boss_jobs::evaluate_station(
        &spec,
        vec![under_review, at_fold, settled, long_gone],
        day(20),
    );
    let mut held: Vec<String> = q.data.iter().map(|j| j.id.to_string()).collect();
    held.sort();
    let mut want = vec![fold_id.clone(), settled_id.clone()];
    want.sort();
    assert_eq!(
        held,
        want,
        "decided = the fold in flight and the design settled inside the window; \
         not the one still under review, not the one settled {} days ago",
        window + 1
    );
    assert!(q.steps.contains_key(&fold_id) && q.steps.contains_key(&settled_id));
}
