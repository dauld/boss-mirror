//! The platform cadence bundle — `infra/platform/cadence/`, one
//! `<name>.toml` per rule — is well-formed and complete, read with no
//! database.
//!
//! WHY (backlog 393d3234, consolidation H4, car 3 of 4). Until
//! 2026-09-18 nine migrations were the only place a cadence rule was
//! declared. The row now lives here: the seed publishes it
//! insert-if-missing by (name, version) at every start, and
//! `infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
//! cadence_rules` in any migration newer than the cutover. The equality
//! pin against the migrations' own rows is
//! `the_cadence_rules_bundle_is_the_migrations_pg.rs`; this file holds
//! the rules that need no database and range over every file.
//!
//! The names below are the migrations' active rules less
//! `protocol-retro-daily`, retired by decision the same day (the pin
//! names why), plus the rules born in the bundle since; a rule added
//! later is a file dropped in and a line here (`train-dock-refresh`,
//! design 42279fb2, was the first).

use boss_jobs::cadence::{CadenceRegistry, CadenceRepository, CadenceRuleSpec, InMemoryCadence};
use boss_jobs::cadence_seed::platform_cadence_path;
use boss_jobs::seed_loader::{bundle_files, load_cadence_rules, parse_cadence_rules};
use std::path::Path;

fn bundle() -> Vec<CadenceRuleSpec> {
    load_cadence_rules(platform_cadence_path()).expect("the platform cadence bundle parses")
}

/// One rule per file, and the file is named for its rule, so `ls`
/// answers "which rules does a deployment seed".
#[test]
fn the_bundle_is_one_file_per_rule() {
    let dir = Path::new(platform_cadence_path());
    let files = bundle_files(dir).expect("the bundle directory lists its rule files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a rule file has a UTF-8 stem");
        let rows = load_cadence_rules(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let names: Vec<&str> = rows.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [stem],
            "{} holds exactly its own rule",
            file.display()
        );
    }
    let mut names: Vec<String> = bundle().into_iter().map(|s| s.name().to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "train-board-on-dock-depth",
            "train-dock-refresh",
            "train-reconcile",
            "train-window"
        ],
        "the three platform cadence rules the migrations seeded and nobody retired, plus \
         train-dock-refresh, the first rule born in the bundle (design 42279fb2), and no other"
    );
}

/// Every bundled row publishes as-is into an empty registry at its
/// declared version, and is then what the conductor's read serves —
/// `train-board-on-dock-depth` lands at v6 with no v1–v5 history
/// nobody wrote.
#[tokio::test]
async fn every_bundled_rule_is_publishable_at_its_declared_version() {
    let registry = InMemoryCadence::default();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    for spec in bundle() {
        let declared = spec.version;
        let name = spec.name().to_string();
        let published = registry
            .publish_declared(spec, &actor, now)
            .await
            .expect("a bundle row publishes");
        assert_eq!(
            published.version, declared,
            "{name}: the declared version lands"
        );
        assert_eq!(
            registry.live_versions(&name).await.expect("versions").len(),
            1,
            "{name}: no synthetic history below the declared version"
        );
    }
    let served = registry.active_rules().await.expect("active rules");
    assert_eq!(
        served.len(),
        bundle().len(),
        "every published rule is served active"
    );
    // EVERY SERVED ROW CARRIES WHAT ITS FILE DECLARED, checked against
    // the bundle rather than against literals.
    //
    // This spot-checked the boarding rule against `(Some(1), Some(45))`
    // until 2026-09-21. That is a second copy of two integers an
    // operator changes — the boarding rule alone has been through seven
    // versions, six of them moving one number — and every one of those
    // changes had to remember to edit this literal too. §9a: a fact that
    // lives twice gets one definition, and here the definition is the
    // bundle file.
    //
    // NOT VACUOUS. The comparison is not file-against-itself: the
    // values go file -> publish_declared -> active_rules, so this
    // asserts the PUBLISH PATH preserves them. A publish that dropped
    // `cooldown_minutes` would still fail, and would now fail for every
    // rule rather than for the one that was spelled out.
    for spec in bundle() {
        let declared = spec.clone();
        let row = served
            .iter()
            .find(|r| r.name == declared.name())
            .unwrap_or_else(|| panic!("{} is served", declared.name()));
        assert_eq!(
            (
                row.min_dock_depth,
                row.cooldown_minutes,
                row.every_minutes,
                row.regate_hold_minutes,
                row.verb.as_str()
            ),
            (
                declared.row.min_dock_depth,
                declared.row.cooldown_minutes,
                declared.row.every_minutes,
                declared.row.regate_hold_minutes,
                declared.row.verb.as_str()
            ),
            "{}: the served row must carry what its bundle file declares",
            declared.name()
        );
    }
}

/// A file that declares anything but an active row, or a version below
/// 1, is refused by the loader with the rule named — a bundle row is
/// what a fresh deployment gets, and that is an active row.
#[test]
fn a_bundle_row_must_be_active_at_a_real_version() {
    let retired = "\
[[cadence_rule]]
name = \"x\"
version = 1
status = \"retired\"
verb = \"reconcile\"
basis = \"wall\"
every_minutes = 10
";
    let err = parse_cadence_rules(retired, "x.toml").expect_err("a retired declaration is refused");
    assert!(err.to_string().contains("`x`"), "{err}");
    assert!(err.to_string().contains("retired"), "{err}");

    let err = parse_cadence_rules(
        &retired
            .replace("version = 1", "version = 0")
            .replace("retired", "active"),
        "x.toml",
    )
    .expect_err("version 0 is refused");
    assert!(err.to_string().contains("version"), "{err}");

    let err = parse_cadence_rules(
        &retired
            .replace("retired", "active")
            .replace("every_minutes", "every_minute"),
        "x.toml",
    )
    .expect_err("a column the table does not have is refused");
    assert!(err.to_string().contains("every_minute"), "{err}");
}

/// A calendar rule's `anchor_date` is a string the row's DATE column
/// reads, and `at_times` is the JSON array the conductor parses.
#[test]
fn a_calendar_rule_round_trips_its_date_and_times() {
    let text = "\
[[cadence_rule]]
name = \"retro\"
version = 1
status = \"active\"
verb = \"open:protocol-retro\"
basis = \"calendar\"
at_times = [\"06:10\"]
cadence = \"daily\"
anchor_date = \"2026-08-28\"
";
    let rows = parse_cadence_rules(text, "retro.toml").expect("parses");
    let row = &rows[0].row;
    assert_eq!(
        row.anchor_date,
        Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 28).unwrap())
    );
    assert_eq!(row.at_times, Some(serde_json::json!(["06:10"])));
    assert_eq!(row.cadence.as_deref(), Some("daily"));
    assert_eq!(row.business_calendar, None);
    assert_eq!(row.regate_hold_minutes, None, "absent means no hold");
}

/// THE DOCK'S TWO RULES (design 42279fb2, backlog 4890165b). The boarding
/// rule carries the bound a departure waits for the dock's re-gate round
/// — and ONLY a departing rule may carry one, which the table's own CHECK
/// refuses too — and the dock refreshes on a wall clock of its own, short
/// enough to relaunch re-gates between departures rather than once per
/// departure window.
#[test]
fn the_dock_refreshes_on_its_own_clock_and_a_departure_declares_its_hold() {
    let rules = bundle();
    let board = rules
        .iter()
        .find(|s| s.name() == "train-board-on-dock-depth")
        .expect("the boarding rule is declared");
    assert!(
        board.row.regate_hold_minutes.is_some_and(|m| m > 0),
        "the boarding rule declares how long a departure waits for the re-gate round: {:?}",
        board.row
    );
    for s in &rules {
        if s.row.regate_hold_minutes.is_some() {
            assert!(
                boss_jobs::cadence::departs_a_train(&s.row.verb),
                "{}: only a rule that departs a train can hold a departure",
                s.name()
            );
        }
    }
    let refresh = rules
        .iter()
        .find(|s| s.name() == "train-dock-refresh")
        .expect("the dock's refresh rule is declared");
    assert_eq!(refresh.row.verb, "refresh");
    assert_eq!(refresh.row.basis, "wall");
    assert!(
        !boss_jobs::cadence::departs_a_train(&refresh.row.verb),
        "a refresh departs nothing, so the loop never holds it for the track"
    );
    assert!(
        refresh
            .row
            .every_minutes
            .is_some_and(|m| (1..5).contains(&m)),
        "the refresh re-asks within a few minutes, not once per departure window: {:?}",
        refresh.row.every_minutes
    );
}
