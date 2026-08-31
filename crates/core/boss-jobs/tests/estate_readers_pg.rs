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
        .recent_events_by_kind("jobs.estate.observed", 1)
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1, "limit respected");
    assert_eq!(
        rows[0]["payload"]["marker"], "obs-2",
        "newest of the exact kind, never the neighbour kind"
    );

    let all = repo
        .recent_events_by_kind("jobs.estate.observed", 50)
        .await
        .expect("read back");
    assert_eq!(all.len(), 2, "only the observed kind counts");
}
