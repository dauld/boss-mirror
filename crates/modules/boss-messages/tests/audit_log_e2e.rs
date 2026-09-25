//! End-to-end test for the messages → audit_log chain, on the REAL
//! pipeline (outbox phase 2): the POST records the event on the
//! transactional outbox inside the adapter's tx; the relay drain
//! moves it to audit_log. Deliberately NO direct audit writer — this
//! test only passes through the real outbox → relay → audit_log path.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_messages::PgMessages;
use boss_messages::http::{MessageApiState, router};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use sqlx::PgPool;

/// The machinery that sends, reads and expires on others' behalf signs
/// as an operator-tier automation: a request with no `x-boss-user` is
/// refused by every scoped door since backlog e84de48e (2026-09-25).
const OPERATOR: &str = r#"{"id":"automation:messages-test","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

fn build_app(pool: PgPool) -> Router {
    // No publisher: the handler stamp falls back to source="messages"
    // and the event records on the outbox.
    router(MessageApiState {
        messages: Arc::new(PgMessages::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn send_message_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let body = serde_json::json!({
        "sender_id": "emp-sender",
        "recipient_id": "emp-recipient",
        "subject": "audit log test",
        "body": "hello",
        "kind": "direct",
    });
    TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert_eq!(stats.delivered, 1, "the send arrives via the outbox");

    let row: (String, String) = sqlx::query_as(
        "SELECT source, kind FROM audit_log \
         WHERE kind = 'messages.message.sent' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST + drain");

    assert_eq!(row.0, "messages");
    assert_eq!(row.1, "messages.message.sent");
}
