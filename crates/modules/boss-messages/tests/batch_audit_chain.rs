//! Audit-chain test for the batch-messages path.
//!
//! `POST /api/messages/batch` must emit MESSAGE_SENT for each row,
//! the same as the single-message POST handler — otherwise the
//! batch path used by the sim and bulk imports is a silent bypass
//! and every batch-imported row vanishes on `boss-rebuild-all`.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_messages::PgMessages;
use boss_messages::http::{MessageApiState, router};
use boss_messages::rebuild_messages;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;
use sqlx::PgPool;

fn build_pg_app(pool: PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox -> relay drain.
    router(MessageApiState {
        messages: Arc::new(PgMessages::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    })
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain")
        .delivered
}

async fn count(pool: &PgPool) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await
        .unwrap();
    row.0
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_messages_survive_rebuild() {
    let db = TestDb::new().await;
    let app = build_pg_app(db.pool.clone());

    let batch = json!([
        {
            "id": "msg-batch-001",
            "sender_id": "emp-sim",
            "recipient_id": "emp-recipient-1",
            "subject": "Wholesale dispatch — 2026-05-04",
            "body": "Driver loaded 84 kegs.",
            "kind": "signal",
            "sent_at": "2026-05-04T09:15:00Z",
        },
        {
            "id": "msg-batch-002",
            "sender_id": "emp-sim",
            "recipient_id": "emp-recipient-2",
            "subject": "Hop shipment received",
            "body": "Pallet from Cascade landed.",
            "kind": "signal",
            "sent_at": "2026-05-04T11:30:00Z",
        },
    ]);

    // The batch door is trusted-only (backlog 4dd10336): the sim and a
    // bulk import sign as an operator-tier automation.
    TestRequest::post("/api/messages/batch")
        .header(
            "x-boss-user",
            r#"{"id":"automation:messages-test","role":"platform-admin","access_tier":"operator"}"#,
        )
        .json(&batch)
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 2, "both batch sends arrive via the outbox");

    let pre = count(&db.pool).await;
    assert_eq!(pre, 2, "two batch rows landed in projection");

    // Wipe + rebuild. With the audit-chain fix, the rebuilder
    // replays MESSAGE_SENT and reproduces both rows. Pre-fix this
    // would leave count=0.
    sqlx::query("DELETE FROM messages")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_messages(&db.pool).await.expect("rebuild");
    assert!(
        report.events_processed >= 2,
        "rebuild should replay both batch events, got {report:?}"
    );

    let post = count(&db.pool).await;
    assert_eq!(post, pre, "batch rows must round-trip through audit_log");
}
