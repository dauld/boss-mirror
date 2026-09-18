//! The business-calendar batch against the real tables (design
//! e187198f, David 2026-09-18: THE INSTANCE IS THE TRUTH).
//!
//! Measured that day: `upsert_business_calendars` was `ON CONFLICT
//! (code) DO UPDATE` plus a DELETE-and-reinsert of the closed-day set
//! (postgres.rs:397-417), and `boss tenant publish` runs at every
//! services-container start — so an operator's edit to a calendar's
//! closed days or weekend lived exactly until the next boot. The door
//! is insert-if-absent by default now: a held code is KEPT and the
//! outcome names the fields the declaration differs on; only
//! `PublishMode::Take` replaces the row (header and closed set
//! wholesale), naming each change from → to.

use std::collections::BTreeSet;

use boss_calendar::{CalendarClient, PgCalendar};
use boss_core::calendar::BusinessCalendar;
use boss_core::publish::PublishMode;
use boss_testing::TestDb;
use chrono::NaiveDate;

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn calendar(name: &str, weekend: &[u8], closed: &[&str]) -> BusinessCalendar {
    BusinessCalendar {
        code: "acme-founder".into(),
        name: name.into(),
        weekend: weekend.iter().copied().collect(),
        closed: closed.iter().map(|d| day(d)).collect(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_calendar_is_kept_by_default_and_replaced_only_under_take() {
    let db = TestDb::new().await;
    let cal = PgCalendar::new(db.pool.clone());

    // First publish: the code is absent, so it lands whole.
    let out = cal
        .publish_business_calendars(
            &[calendar("founder hours", &[5, 6], &["2026-12-25"])],
            PublishMode::InsertIfAbsent,
        )
        .await
        .expect("lands");
    assert_eq!((out.received, out.inserted), (1, 1));
    assert!(out.kept.is_empty() && out.updated.is_empty(), "{out:?}");

    // An operator's edit through the database: a longer closed set
    // and a Friday weekend.
    sqlx::query("UPDATE business_calendars SET weekend = '{4,5,6}' WHERE code = 'acme-founder'")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO business_calendar_closed_days (calendar_code, day) VALUES ('acme-founder', '2026-12-26')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // The same file again, by default: kept, and the outcome names the
    // two fields the declaration differs on.
    let declared = calendar("founder hours", &[5, 6], &["2026-12-25"]);
    let out = cal
        .publish_business_calendars(std::slice::from_ref(&declared), PublishMode::InsertIfAbsent)
        .await
        .expect("answers");
    assert_eq!((out.received, out.inserted), (1, 0));
    assert!(out.updated.is_empty(), "{out:?}");
    assert_eq!(out.kept.len(), 1, "{out:?}");
    assert_eq!(
        out.kept[0].render(),
        "acme-founder differs on weekend, closed"
    );
    let live = cal
        .get_business_calendar("acme-founder")
        .await
        .unwrap()
        .expect("still there");
    assert_eq!(
        live.weekend,
        [4u8, 5, 6].into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        live.closed,
        [day("2026-12-25"), day("2026-12-26")]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the operator's closed day survives the republish"
    );

    // Identical declaration: neither kept-differing nor updated.
    let same = calendar("founder hours", &[4, 5, 6], &["2026-12-25", "2026-12-26"]);
    let out = cal
        .publish_business_calendars(&[same], PublishMode::InsertIfAbsent)
        .await
        .unwrap();
    assert_eq!((out.inserted, out.kept.len(), out.updated.len()), (0, 0, 0));
    assert_eq!(out.unchanged, 1);

    // Under take the declaration replaces header and closed set
    // wholesale, and each change is named from → to.
    let out = cal
        .publish_business_calendars(std::slice::from_ref(&declared), PublishMode::Take)
        .await
        .expect("takes");
    assert_eq!((out.inserted, out.kept.len()), (0, 0));
    assert_eq!(out.updated.len(), 1, "{out:?}");
    assert_eq!(out.updated[0].id, "acme-founder");
    assert_eq!(
        out.updated[0].render(),
        "acme-founder (weekend [4,5,6] → [5,6], closed [\"2026-12-25\",\"2026-12-26\"] → [\"2026-12-25\"])"
    );
    let live = cal
        .get_business_calendar("acme-founder")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.weekend, [5u8, 6].into_iter().collect::<BTreeSet<_>>());
    assert_eq!(
        live.closed,
        [day("2026-12-25")].into_iter().collect::<BTreeSet<_>>()
    );
}
