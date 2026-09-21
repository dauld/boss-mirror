//! Postgres-backed coverage for the sensor registry and its readings
//! (design 14c9b2ad, backlog 2d33e111): the tables the migration
//! creates are the ones the adapter writes; both writes are
//! insert-if-absent; the first stamp wins; the cursor is monotonic on
//! the SENSOR row; the sweep keeps a reading still owed a packet — the
//! same answers the in-memory twin gives.

use boss_jobs::sensors::{NewReading, PgSensors, PollStamp, SensorInput, Sensors, SensorsError};
use boss_testing::TestDb;
use chrono::{DateTime, Duration, TimeZone, Utc};

fn t(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 17, 10, 0, 0).unwrap() + Duration::minutes(minutes)
}

fn input(id: &str) -> SensorInput {
    SensorInput {
        id: id.into(),
        source: "stripe".into(),
        credential: "stripe-restricted-read".into(),
        every_minutes: 15,
        opens: "receive-a-sponsorship".into(),
        subject_kind: "custom".into(),
        enabled: true,
        selector: None,
    }
}

fn reading(id: &str, at: DateTime<Utc>) -> NewReading {
    NewReading {
        external_id: id.into(),
        observed_at: at,
        payload: serde_json::json!({"id": id, "amount": 100}),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_is_insert_if_absent_and_keeps_its_cursor() {
    let db = TestDb::new().await;
    let repo = PgSensors::new(db.pool.clone());
    let out = repo
        .publish("acme", &[input("stripe-sponsorships")])
        .await
        .unwrap();
    assert_eq!((out.received, out.inserted), (1, 1));
    repo.mark_polled(
        "stripe-sponsorships",
        &PollStamp {
            polled_at: t(0),
            cursor_at: Some(t(-5)),
        },
    )
    .await
    .unwrap();
    // A second publish — the same row plus a new one — inserts one and
    // leaves the first row's cursor where the poller put it.
    let mut second = input("stripe-sponsorships");
    second.every_minutes = 1;
    let out = repo
        .publish("acme", &[second, input("a-second-sensor")])
        .await
        .unwrap();
    assert_eq!((out.received, out.inserted), (2, 1));
    let rows = repo.list().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "a-second-sensor", "ordered by id");
    let first = &rows[1];
    assert_eq!(
        first.every_minutes, 15,
        "the row already there is kept as it was"
    );
    assert_eq!(first.tenant_id, "acme");
    assert_eq!(first.opens_kind, "receive-a-sponsorship");
    assert_eq!(first.cursor_at, Some(t(-5)));
    assert_eq!(first.last_polled_at, Some(t(0)));
    assert!(first.due_at(t(15)));
    assert!(!first.due_at(t(14)));
}

#[tokio::test(flavor = "multi_thread")]
async fn readings_dedup_by_external_id_and_the_first_stamp_wins() {
    let db = TestDb::new().await;
    let repo = PgSensors::new(db.pool.clone());
    repo.publish("acme", &[input("s")]).await.unwrap();
    let out = repo
        .record("s", &[reading("ch_1", t(0)), reading("ch_2", t(1))])
        .await
        .unwrap();
    assert_eq!(out.inserted, 2);
    let out = repo
        .record("s", &[reading("ch_1", t(0)), reading("ch_3", t(2))])
        .await
        .unwrap();
    assert_eq!(
        (out.received, out.inserted),
        (2, 1),
        "a redelivery inserts nothing"
    );

    let owed = repo.unstamped("s").await.unwrap();
    assert_eq!(
        owed.iter()
            .map(|r| r.external_id.as_str())
            .collect::<Vec<_>>(),
        ["ch_1", "ch_2", "ch_3"],
        "oldest observation first"
    );
    assert_eq!(owed[0].payload["amount"], 100);

    repo.stamp("s", "ch_1", "job-1").await.unwrap();
    repo.stamp("s", "ch_1", "job-2").await.unwrap();
    let owed = repo.unstamped("s").await.unwrap();
    assert_eq!(owed.len(), 2);
    let stamped: Option<String> = sqlx::query_scalar(
        "SELECT packet_id FROM sensor_readings WHERE sensor_id = 's' AND external_id = 'ch_1'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(stamped.as_deref(), Some("job-1"));

    assert!(matches!(
        repo.record("nobody", &[reading("x", t(0))]).await,
        Err(SensorsError::UnknownSensor(_))
    ));
    assert!(matches!(
        repo.stamp("s", "never-read", "job").await,
        Err(SensorsError::UnknownSensor(_))
    ));
    assert!(matches!(
        repo.mark_polled(
            "nobody",
            &PollStamp {
                polled_at: t(0),
                cursor_at: None
            }
        )
        .await,
        Err(SensorsError::UnknownSensor(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_never_moves_backwards_and_the_sweep_keeps_what_is_owed() {
    let db = TestDb::new().await;
    let repo = PgSensors::new(db.pool.clone());
    repo.publish("acme", &[input("s")]).await.unwrap();
    for (polled, cursor) in [(t(0), Some(t(0))), (t(15), Some(t(-60))), (t(30), None)] {
        repo.mark_polled(
            "s",
            &PollStamp {
                polled_at: polled,
                cursor_at: cursor,
            },
        )
        .await
        .unwrap();
    }
    let row = &repo.list().await.unwrap()[0];
    assert_eq!(row.cursor_at, Some(t(0)));
    assert_eq!(row.last_polled_at, Some(t(30)));

    let old = t(0) - Duration::days(100);
    repo.record(
        "s",
        &[
            reading("done", old),
            reading("owed", old),
            reading("new", t(0)),
        ],
    )
    .await
    .unwrap();
    repo.stamp("s", "done", "job").await.unwrap();
    repo.stamp("s", "new", "job").await.unwrap();
    let deleted = repo.sweep(t(0) - Duration::days(90)).await.unwrap();
    assert_eq!(deleted, 1, "only the old, stamped reading");
    let left: Vec<String> =
        sqlx::query_scalar("SELECT external_id FROM sensor_readings ORDER BY external_id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(left, ["new", "owed"]);
}

/// A push-only sensor (backlog 0b5c5081): the row lands with no
/// credential and no period (the schema admits `every_minutes = 0`
/// since 20260917-a-push-only-sensor-has-no-period), it is never due,
/// and its readings — which no stamp will ever reach — are swept by
/// age alone while a polled sensor's owed reading of the same age is
/// ONE MAILBOX, MANY DEPARTMENTS (design bffc0aba). The selector is the
/// field that lets two sensors read the same source and each take only
/// its own stream, so it has to survive the table — a column that
/// silently dropped it would give both sensors everything.
#[tokio::test(flavor = "multi_thread")]
async fn a_selector_survives_the_round_trip_and_null_stays_null() {
    let db = TestDb::new().await;
    let repo = PgSensors::new(db.pool.clone());
    let mut support = input("support-inbox");
    support.source = "mail".into();
    support.opens = "receive-a-message".into();
    support.selector = Some("support@algedonic.dev".into());
    let mut finance = input("finance-inbox");
    finance.source = "mail".into();
    finance.opens = "receive-an-invoice".into();
    finance.selector = Some("finance@algedonic.dev".into());
    repo.publish(
        "algedonic",
        &[support, finance, input("stripe-sponsorships")],
    )
    .await
    .expect("publish");

    let rows = repo.list().await.expect("list");
    let by = |id: &str| {
        rows.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} published"))
            .clone()
    };
    assert_eq!(
        by("support-inbox").selector.as_deref(),
        Some("support@algedonic.dev")
    );
    assert_eq!(
        by("finance-inbox").selector.as_deref(),
        Some("finance@algedonic.dev"),
        "two sensors on one source keep DIFFERENT selectors"
    );
    assert_eq!(
        by("stripe-sponsorships").selector,
        None,
        "a source with one stream selects nothing, and null is not the empty string"
    );
}

/// kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_only_sensor_lands_without_a_period_and_its_readings_are_swept_by_age() {
    let db = TestDb::new().await;
    let repo = PgSensors::new(db.pool.clone());
    let site = SensorInput {
        id: "www-visits".into(),
        source: "site".into(),
        credential: String::new(),
        every_minutes: 0,
        opens: "marketing-weekly".into(),
        subject_kind: "custom".into(),
        enabled: true,
        selector: None,
    };
    let out = repo.publish("acme", &[input("s"), site]).await.unwrap();
    assert_eq!((out.received, out.inserted), (2, 2));
    let rows = repo.list().await.unwrap();
    let www = rows.iter().find(|r| r.id == "www-visits").unwrap();
    assert!(www.is_push_only());
    assert_eq!(www.every_minutes, 0);
    assert!(!www.due_at(t(0)), "never due: nothing polls it");

    let old = t(0) - Duration::days(100);
    repo.record("s", &[reading("owed", old)]).await.unwrap();
    repo.record(
        "www-visits",
        &[reading("view-old", old), reading("view-new", t(0))],
    )
    .await
    .unwrap();
    let deleted = repo.sweep(t(0) - Duration::days(90)).await.unwrap();
    assert_eq!(deleted, 1, "the old page view and nothing else");
    let left: Vec<String> =
        sqlx::query_scalar("SELECT external_id FROM sensor_readings ORDER BY external_id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(left, ["owed", "view-new"]);
}
