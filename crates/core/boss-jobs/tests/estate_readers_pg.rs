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
use boss_jobs::port::JobsRepository;
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
        .recent_events_by_kind("jobs.estate.observed", None, 1)
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1, "limit respected");
    assert_eq!(
        rows[0]["payload"]["marker"], "obs-2",
        "newest of the exact kind, never the neighbour kind"
    );

    let all = repo
        .recent_events_by_kind("jobs.estate.observed", None, 50)
        .await
        .expect("read back");
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
        .recent_events_by_kind("jobs.estate.observed", Some("codebase"), 50)
        .await
        .expect("read back");
    assert_eq!(scoped.len(), 1, "one codebase observation");
    assert_eq!(scoped[0]["payload"]["marker"], "nightly");

    let k8s = repo
        .recent_events_by_kind("jobs.estate.observed", Some("kubernetes-nodes"), 50)
        .await
        .expect("read back");
    assert_eq!(k8s.len(), 10, "the fast scope, all of it");

    let unknown = repo
        .recent_events_by_kind("jobs.estate.observed", Some("nonesuch"), 50)
        .await
        .expect("read back");
    assert!(unknown.is_empty(), "an unrecorded scope is empty, not all");

    // A limit smaller than the number of BURIED rows: only a filter
    // that reaches the WHERE clause can still find the nightly row.
    let buried = repo
        .recent_events_by_kind("jobs.estate.observed", Some("codebase"), 2)
        .await
        .expect("read back");
    assert_eq!(
        buried.len(),
        1,
        "the slow scope survives a limit its neighbours would have filled"
    );
    assert_eq!(buried[0]["payload"]["marker"], "nightly");
}
