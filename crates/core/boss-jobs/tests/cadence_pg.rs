//! Postgres-backed coverage for the cadence registry surface.
//!
//! This file inherits a specific scar. Until 2026-08-14 the train
//! conductor opened its own `PgPool` and read `cadence_rules`
//! directly. It ran on a host whose local Postgres was NOT the
//! cluster's, so the registry an operator inspected said
//! `min_dock_depth = 4` while the loop actually enforcing boarding
//! read 8 — and a confident, sourced, wrong answer came out of that
//! gap ("the dock is at 4, it will board"). It did not board.
//!
//! The seed assertion below is the pin from the SERVING side: the
//! number the API hands the conductor is the number in the schema. The
//! conductor no longer has a second opinion available to it, because it
//! no longer has a second database.

use boss_jobs::cadence::{CadenceRepository, NewFiring, PgCadence};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};

fn firing(id: &str, rule: &str) -> NewFiring {
    NewFiring {
        firing_id: id.into(),
        rule_name: rule.into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        fired_at: Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap(),
        detail: serde_json::json!({ "dock_depth": 9 }),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_rules_serve_the_thresholds_the_schema_declares() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    let rules = repo.active_rules().await.unwrap();

    let by_name = |n: &str| {
        rules
            .iter()
            .find(|r| r.name == n)
            .unwrap_or_else(|| panic!("seed rule {n} missing"))
    };

    let reconcile = by_name("train-reconcile");
    assert_eq!(reconcile.verb, "reconcile");
    assert_eq!(reconcile.basis, "wall");
    assert_eq!(reconcile.every_minutes, Some(10));

    let window = by_name("train-window");
    assert_eq!(window.verb, "run");
    assert_eq!(window.basis, "clock");
    // :05, not :00. 134-cadence-window-off-grid.sql moved the window off
    // the wall grid the 10-minute reconcile fires on: 06:00 and 18:00 are
    // both multiples of ten, so the two rules fired in the same tick every
    // time and the window lost the conductor's flock — having already
    // claimed its firing row. In the system's whole history the
    // twice-daily window never boarded a train (filed 4ed0e791). Five past
    // is off that grid for any interval the reconcile is likely to use.
    assert_eq!(
        window.at_times,
        Some(serde_json::json!(["06:05", "18:05"])),
        "the clock rule's windows are registry data, not a constant"
    );

    // The number from the scar. Migration 123 reconciled the seed to the
    // value the running conductor already enforced; 131 raised it to 12
    // so one CI run carries more work (David: "load up trains as fast as
    // we can and have CI be the blocker"); 147 put it back to 4.
    //
    // Why it came back down: 12 was unreachable. This project's dock
    // does not exceed 3-5, so on 2026-08-17 four consecutive trains
    // opened and cancelled "nothing to board" while three mergeable cars
    // sat parked, and the depth rule had not fired for a day. Combined
    // with the window above never having boarded anything, the pipeline
    // had no working automatic trigger at all.
    //
    // THIS NUMBER LIVES IN FOUR PLACES and they do not move together:
    // the migration, boss-gcp's LOCAL cadence_rules (which is what the
    // boarding loop actually reads — see 131 and 147), this assertion,
    // and the sibling in boss-cli's cadence::db_tests. `--auto` gates a
    // schema-only change with "fixture + lints only" and SKIPS tests, so
    // editing the migration alone leaves both assertions red and reports
    // green. That is how this one was found: not by the change that
    // broke it, but by an unrelated merge dragging the crate into scope.
    let depth = by_name("train-board-on-dock-depth");
    assert_eq!(depth.verb, "board");
    assert_eq!(depth.basis, "queue-depth");
    assert_eq!(
        depth.min_dock_depth,
        // 3 since 202609032030-cadence-supersede-by-name.sql. It was 4;
        // board-on-three (202609031515) tried 3 and SILENTLY NO-OP'd —
        // its version-keyed retire missed the real active row against a
        // diverged version history, so the live value stayed 4 and both
        // this pin and its boss-cli sibling kept asserting 4, documenting
        // the breakage. The supersede-by-name migration retires the
        // active row BY NAME (correct from any version history) so 3
        // actually takes; both pins now assert 3. This one is the FOURTH
        // place the number lives — the gate found it after the sibling
        // was moved, exactly as this comment warned.
        Some(3),
        "boarding threshold drifted from the schema — this is the \
         2026-08-13 split-brain, and it made the operator's answer wrong"
    );
    assert_eq!(depth.cooldown_minutes, Some(120));
}

/// A calendar rule must be served WHOLE — cadence, anchor_date and
/// business_calendar included.
///
/// The conductor's cutover onto `/api/cadence/*` (protocol-cadence.md
/// sequencing step 3, backlog a516f1f1) makes this adapter the loop's
/// only source of rules. The loop already lived through the shape of
/// this failure once on the SQL side: `load_rules`' SELECT was not
/// widened when the calendar basis landed, and protocol-retro-daily
/// was skipped loudly on every tick while the registry showed it
/// active. Serving the row without its calendar columns would replay
/// that scar one door over.
#[tokio::test(flavor = "multi_thread")]
async fn a_calendar_rule_is_served_whole() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    let rules = repo.active_rules().await.unwrap();

    let retro = rules
        .iter()
        .find(|r| r.name == "protocol-retro-daily")
        .expect("seed rule protocol-retro-daily missing");
    assert_eq!(retro.basis, "calendar");
    assert_eq!(
        retro.cadence.as_deref(),
        Some("daily"),
        "a calendar rule served without its cadence is unreadable to the loop"
    );
    assert_eq!(
        retro.anchor_date,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 28),
        "the anchor is the recurrence's whole identity"
    );
    assert_eq!(retro.business_calendar, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn active_rules_excludes_retired_ones() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    sqlx::query("UPDATE cadence_rules SET status = 'retired' WHERE name = $1")
        .bind("train-reconcile")
        .execute(&db.pool)
        .await
        .unwrap();

    let rules = repo.active_rules().await.unwrap();
    assert!(
        !rules.iter().any(|r| r.name == "train-reconcile"),
        "a retired rule must stop firing — retirement is how a cadence \
         is turned off without deleting its firing history"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_is_exactly_once_against_the_primary_key() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    let f = firing(
        "cadence:board:2026-08-14T12:00Z",
        "train-board-on-dock-depth",
    );

    assert!(repo.claim_firing(&f).await.unwrap());
    assert!(
        !repo.claim_firing(&f).await.unwrap(),
        "ON CONFLICT (firing_id) DO NOTHING is the exactly-once gate: a \
         conductor that crashed mid-verb must not re-run the verb on restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_records_the_callers_clock_not_the_database_wallclock() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    // A sim-dated tick, far from real time.
    let simulated = Utc.with_ymd_and_hms(2031, 1, 2, 3, 4, 0).unwrap();
    let mut f = firing(
        "cadence:board:2031-01-02T03:04Z",
        "train-board-on-dock-depth",
    );
    f.fired_at = simulated;
    repo.claim_firing(&f).await.unwrap();

    let last = repo
        .last_firing("train-board-on-dock-depth")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        last.fired_at, simulated,
        "fired_at is bound from boss-clock by the caller; a server-side \
         NOW() would silently rewrite sim runs to real time"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn outcome_merges_and_preserves_why_the_rule_fired() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    let f = firing(
        "cadence:board:2026-08-14T12:00Z",
        "train-board-on-dock-depth",
    );
    repo.claim_firing(&f).await.unwrap();
    repo.record_outcome(&f.firing_id, 0, 37).await.unwrap();

    let row: (serde_json::Value,) =
        sqlx::query_as("SELECT detail FROM cadence_firings WHERE firing_id = $1")
            .bind(&f.firing_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(row.0["rc"], 0);
    assert_eq!(row.0["runtime_secs"], 37);
    assert_eq!(
        row.0["dock_depth"], 9,
        "`detail || $2` merges; replacing would discard the dock depth \
         that triggered the firing, which is the measurement"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn last_firing_is_none_before_a_rule_has_ever_fired() {
    let db = TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    assert!(repo.last_firing("train-window").await.unwrap().is_none());
}
