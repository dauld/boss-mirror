//! `recent_events_by_kind` against real Postgres (d471a8ce).
//!
//! The write path stages events on the outbox and the relay drain
//! moves them into `audit_log` — so this test exercises the same two
//! hops production does: record through the repository, drain once,
//! read back through the port. A read that skipped the drain would
//! assert against rows that production never has at that point.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::port::EventBus;
use boss_jobs::PgJobs;
use boss_jobs::port::{EventWindow, JobsRepository};
use boss_testing::{RecordingEventBus, TestDb};
use sqlx::PgPool;

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 200)
        .await
        .expect("relay drain");
}

fn event(kind: &str, marker: &str) -> Event {
    Event::new(
        "jobs",
        kind,
        serde_json::json!({"marker": marker}),
        chrono::Utc::now(),
    )
}

fn scoped_event(kind: &str, scope: &str, marker: &str) -> Event {
    Event::new(
        "jobs",
        kind,
        serde_json::json!({"marker": marker, "scope": scope}),
        chrono::Utc::now(),
    )
}

fn in_scope(scope: &str) -> EventWindow {
    EventWindow {
        scope: Some(scope.to_string()),
        ..EventWindow::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_one_exact_kind_newest_first_with_limit() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    repo.record_events(&[
        event("jobs.estate.observed", "obs-1"),
        event("jobs.estate.compared", "cmp-1"),
        event("jobs.estate.observed", "obs-2"),
    ])
    .await
    .expect("events record");
    drain_outbox(&db.pool).await;

    let rows = repo
        .recent_events_by_kind("jobs.estate.observed", &EventWindow::default(), 1)
        .await
        .expect("read back")
        .rows;
    assert_eq!(rows.len(), 1, "limit respected");
    assert_eq!(
        rows[0]["payload"]["marker"], "obs-2",
        "newest of the exact kind, never the neighbour kind"
    );

    let all = repo
        .recent_events_by_kind("jobs.estate.observed", &EventWindow::default(), 50)
        .await
        .expect("read back")
        .rows;
    assert_eq!(all.len(), 2, "only the observed kind counts");
}

/// The SQL half of the scope filter, pinned against the same property
/// the in-memory reader asserts in `estate_readers_http.rs`: two
/// implementations of one rule, so `payload->>'scope' = $2` is held to
/// the numbers the HTTP tests already assert.
///
/// The load-bearing case is the last one — the filter must be applied
/// WHERE THE LIMIT IS. A reader that fetched N rows and filtered them
/// in Rust would pass every assertion above and still fail this, which
/// is exactly the shape of the defect measured on 2026-09-02.
#[tokio::test(flavor = "multi_thread")]
async fn scope_filters_before_the_limit() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    let mut events = vec![scoped_event("jobs.estate.observed", "codebase", "nightly")];
    for i in 0..10 {
        events.push(scoped_event(
            "jobs.estate.observed",
            "kubernetes-nodes",
            &format!("k8s-{i}"),
        ));
    }
    repo.record_events(&events).await.expect("events record");
    drain_outbox(&db.pool).await;

    let scoped = repo
        .recent_events_by_kind("jobs.estate.observed", &in_scope("codebase"), 50)
        .await
        .expect("read back")
        .rows;
    assert_eq!(scoped.len(), 1, "one codebase observation");
    assert_eq!(scoped[0]["payload"]["marker"], "nightly");

    let k8s = repo
        .recent_events_by_kind("jobs.estate.observed", &in_scope("kubernetes-nodes"), 50)
        .await
        .expect("read back")
        .rows;
    assert_eq!(k8s.len(), 10, "the fast scope, all of it");

    let unknown = repo
        .recent_events_by_kind("jobs.estate.observed", &in_scope("nonesuch"), 50)
        .await
        .expect("read back")
        .rows;
    assert!(unknown.is_empty(), "an unrecorded scope is empty, not all");

    // A limit smaller than the number of BURIED rows: only a filter
    // that reaches the WHERE clause can still find the nightly row.
    let buried = repo
        .recent_events_by_kind("jobs.estate.observed", &in_scope("codebase"), 2)
        .await
        .expect("read back")
        .rows;
    assert_eq!(
        buried.len(),
        1,
        "the slow scope survives a limit its neighbours would have filled"
    );
    assert_eq!(buried[0]["payload"]["marker"], "nightly");
}

/// The SQL half of the time window (backlog bf362f25), pinned against
/// the numbers `estate_readers_http.rs` asserts of the in-memory
/// reader: `since` inclusive, `until` exclusive, both in the WHERE
/// clause beside the scope, and `total` the count of the WINDOW rather
/// than the page. The load-bearing leg is the cursor walk — a page
/// capped below the window, then the next page read with the oldest
/// timestamp on the first as its `until`, reaching the oldest row.
#[tokio::test(flavor = "multi_thread")]
async fn a_time_window_pages_back_and_counts_itself() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    let t0: chrono::DateTime<chrono::Utc> = "2026-09-22T00:00:00Z".parse().unwrap();
    let mut events: Vec<Event> = (0..12)
        .map(|i| {
            Event::new(
                "jobs",
                "jobs.estate.observed",
                serde_json::json!({"scope": "host-units", "marker": format!("u-{i}")}),
                t0 + chrono::Duration::minutes(i),
            )
        })
        .collect();
    // A neighbour scope inside the same window must not be counted.
    events.push(Event::new(
        "jobs",
        "jobs.estate.observed",
        serde_json::json!({"scope": "host", "marker": "h-0"}),
        t0 + chrono::Duration::minutes(5),
    ));
    repo.record_events(&events).await.expect("events record");
    drain_outbox(&db.pool).await;

    let window = EventWindow {
        scope: Some("host-units".to_string()),
        since: Some(t0 + chrono::Duration::minutes(2)),
        until: Some(t0 + chrono::Duration::minutes(10)),
        ..EventWindow::default()
    };
    let first = repo
        .recent_events_by_kind("jobs.estate.observed", &window, 3)
        .await
        .expect("read back");
    let markers = |rows: &[serde_json::Value]| -> Vec<String> {
        rows.iter()
            .map(|r| r["payload"]["marker"].as_str().unwrap_or("?").to_string())
            .collect()
    };
    assert_eq!(markers(&first.rows), vec!["u-9", "u-8", "u-7"]);
    assert_eq!(
        first.total, 8,
        "minutes 2..10, one scope: the window, not the page"
    );

    let cursor: chrono::DateTime<chrono::Utc> = first.rows[2]["timestamp"]
        .as_str()
        .expect("rows carry their timestamp")
        .parse()
        .expect("an RFC 3339 instant");
    let next = repo
        .recent_events_by_kind(
            "jobs.estate.observed",
            &EventWindow {
                until: Some(cursor),
                ..window.clone()
            },
            50,
        )
        .await
        .expect("read back");
    assert_eq!(
        markers(&next.rows),
        vec!["u-6", "u-5", "u-4", "u-3", "u-2"],
        "the rows before the cursor, down to the inclusive since"
    );
    assert_eq!(next.total, 5, "a whole answer: rows == total");
}

/// The SQL half of `latest_per` (backlog 725532ab), pinned against the
/// numbers `estate_readers_http.rs` asserts of the in-memory reader:
/// one row per distinct `payload->>'host'`, each that host's newest,
/// grouped INSIDE the window and BEFORE the limit, newest first, and
/// `total` counting the groups. The load-bearing leg is the daily host
/// behind a page's worth of the fast one — measured 2026-09-25, 50 of
/// 768 host rows, all forge's, and boss-gcp's row not among them.
#[tokio::test(flavor = "multi_thread")]
async fn latest_per_host_groups_before_the_limit_and_counts_hosts() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    let t0: chrono::DateTime<chrono::Utc> = "2026-09-24T10:25:00Z".parse().unwrap();
    let cmp = |host: &str, i: i64, at: chrono::DateTime<chrono::Utc>| {
        Event::new(
            "jobs",
            "jobs.estate.compared",
            serde_json::json!({"scope": "host", "host": host, "marker": format!("{host}-{i}")}),
            at,
        )
    };
    let mut events = vec![cmp("boss-gcp", 0, t0)];
    for i in 0..12 {
        events.push(cmp("forge", i, t0 + chrono::Duration::minutes(35 + i)));
    }
    // Another scope naming a host must not join the host groups.
    events.push(Event::new(
        "jobs",
        "jobs.estate.compared",
        serde_json::json!({"scope": "host-units", "host": "w-1", "marker": "units"}),
        t0 + chrono::Duration::minutes(50),
    ));
    repo.record_events(&events).await.expect("events record");
    drain_outbox(&db.pool).await;

    let markers = |rows: &[serde_json::Value]| -> Vec<String> {
        rows.iter()
            .map(|r| r["payload"]["marker"].as_str().unwrap_or("?").to_string())
            .collect()
    };
    let per_host = EventWindow {
        latest_per: Some("host".to_string()),
        ..in_scope("host")
    };

    // Precondition: a plain scoped page smaller than forge's run has no
    // boss-gcp row.
    let plain = repo
        .recent_events_by_kind("jobs.estate.compared", &in_scope("host"), 10)
        .await
        .expect("read back");
    assert!(!markers(&plain.rows).contains(&"boss-gcp-0".to_string()));

    let latest = repo
        .recent_events_by_kind("jobs.estate.compared", &per_host, 10)
        .await
        .expect("read back");
    assert_eq!(markers(&latest.rows), vec!["forge-11", "boss-gcp-0"]);
    assert_eq!(latest.total, 2, "total counts hosts, not rows");

    let one = repo
        .recent_events_by_kind("jobs.estate.compared", &per_host, 1)
        .await
        .expect("read back");
    assert_eq!(markers(&one.rows), vec!["forge-11"]);
    assert_eq!(
        one.total, 2,
        "a limit below the host count shows as rows < total"
    );

    let before = repo
        .recent_events_by_kind(
            "jobs.estate.compared",
            &EventWindow {
                until: Some(t0 + chrono::Duration::minutes(35)),
                ..per_host.clone()
            },
            10,
        )
        .await
        .expect("read back");
    assert_eq!(
        markers(&before.rows),
        vec!["boss-gcp-0"],
        "grouped inside the window"
    );
    assert_eq!(before.total, 1);
}
