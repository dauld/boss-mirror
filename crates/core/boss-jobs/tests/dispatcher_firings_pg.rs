//! Postgres coverage for the READ half of `dispatcher_firings`
//! (backlog b14afc48).
//!
//! The world map asks one question of a dispatcher rule — when did it
//! last fire — and the answer has to come off the row the runner wrote.
//! This test writes rows the way the writer does and reads them through
//! the adapter, so the two halves are pinned to the same columns: the
//! writer lives in `boss-dispatcher` (which depends on this crate, so
//! it cannot be exercised from here) and its own round trip is
//! `boss-dispatcher/tests/dispatcher_firings_pg.rs`.
//!
//! WHAT MUST NOT BLUR: "never fired" and "could not be read". The
//! adapter answers `Ok(None)` for the first and an `Err` for the
//! second, because a read failure rendered as never-fired is how a
//! stopped machine draws healthy.

use boss_jobs::dispatcher_firings::{DispatcherFiringsRepository, PgDispatcherFirings};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};

async fn insert(pool: &sqlx::PgPool, id: &str, rule: &str, fired_at: &str) {
    sqlx::query(
        "INSERT INTO dispatcher_firings (firing_id, rule_name, fired_on, fired_at, detail) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (firing_id) DO NOTHING",
    )
    .bind(id)
    .bind(rule)
    .bind("jobs.gate.green")
    .bind(fired_at.parse::<chrono::DateTime<Utc>>().unwrap())
    .bind(serde_json::json!({ "event_id": "evt-1" }))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_firing_of_the_named_rule_is_what_is_served() {
    let db = TestDb::new().await;
    let repo = PgDispatcherFirings::new(db.pool.clone());

    // A rule nobody has recorded a firing for: None, never an error and
    // never an invented instant.
    assert_eq!(
        repo.last_firing("auto-park-on-gate-green").await.unwrap(),
        None,
        "a rule with no row has never fired — and says so"
    );

    insert(
        &db.pool,
        "dispatcher:auto-park-on-gate-green:evt-1",
        "auto-park-on-gate-green",
        "2026-09-19T09:00:00Z",
    )
    .await;
    insert(
        &db.pool,
        "dispatcher:auto-park-on-gate-green:evt-2",
        "auto-park-on-gate-green",
        "2026-09-19T11:30:00Z",
    )
    .await;
    // Another rule's firing, newer than both: the read is per rule, so
    // it must not leak across names.
    insert(
        &db.pool,
        "dispatcher:complete-marker-on-step-ready:evt-3",
        "complete-marker-on-step-ready",
        "2026-09-19T12:00:00Z",
    )
    .await;

    let last = repo
        .last_firing("auto-park-on-gate-green")
        .await
        .unwrap()
        .expect("the rule has fired twice");
    assert_eq!(
        last.fired_at,
        Utc.with_ymd_and_hms(2026, 9, 19, 11, 30, 0).unwrap()
    );
    assert_eq!(last.firing_id, "dispatcher:auto-park-on-gate-green:evt-2");
    assert_eq!(last.fired_on, "jobs.gate.green");
}

/// The rules list's read (backlog 43c4451a): the newest firing of EVERY
/// rule, one row each, off the same columns the writer fills — an empty
/// table answering an empty list, never an error.
#[tokio::test(flavor = "multi_thread")]
async fn every_rules_newest_firing_is_one_row_per_rule() {
    let db = TestDb::new().await;
    let repo = PgDispatcherFirings::new(db.pool.clone());
    assert!(repo.last_firings().await.unwrap().is_empty());

    for (id, rule, at) in [
        ("evt-1", "auto-park-on-gate-green", "2026-09-19T09:00:00Z"),
        ("evt-2", "auto-park-on-gate-green", "2026-09-19T11:30:00Z"),
        ("evt-3", "auto-park-on-gate-green", "2026-09-19T10:00:00Z"),
        (
            "evt-4",
            "complete-marker-on-step-ready",
            "2026-09-18T12:00:00Z",
        ),
    ] {
        insert(&db.pool, &format!("dispatcher:{rule}:{id}"), rule, at).await;
    }

    let all = repo.last_firings().await.unwrap();
    assert_eq!(
        all.iter()
            .map(|f| (f.rule.as_str(), f.fired_at))
            .collect::<Vec<_>>(),
        [
            (
                "auto-park-on-gate-green",
                Utc.with_ymd_and_hms(2026, 9, 19, 11, 30, 0).unwrap()
            ),
            (
                "complete-marker-on-step-ready",
                Utc.with_ymd_and_hms(2026, 9, 18, 12, 0, 0).unwrap()
            ),
        ]
    );
    assert_eq!(all[0].fired_on, "jobs.gate.green");
}
