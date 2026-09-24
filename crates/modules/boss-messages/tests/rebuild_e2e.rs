//! End-to-end: send messages via the API (which writes to both the
//! `messages` projection AND the `audit_log` event log), snapshot the
//! resulting projection, drop every row, run `rebuild_messages`,
//! and assert the projection matches the snapshot.
//!
//! Pilot validation for the "events are canonical, projections are
//! derived" architecture.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_messages::PgMessages;
use boss_messages::http::{MessageApiState, router};
use boss_messages::rebuild_messages;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct MessageRow {
    id: String,
    sender_id: String,
    recipient_id: String,
    subject: String,
    body: String,
    entity_type: Option<String>,
    entity_id: Option<String>,
    kind: String,
    sent_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
    reply_to: Option<String>,
}

async fn snapshot_messages(pool: &sqlx::PgPool) -> Vec<MessageRow> {
    sqlx::query_as::<_, MessageRow>(
        "SELECT id, sender_id, recipient_id, subject, body, entity_type, entity_id, \
                kind, sent_at, read_at, reply_to \
         FROM messages ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: sqlx::PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox -> relay drain.
    let state = MessageApiState {
        messages: Arc::new(PgMessages::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    router(state)
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &sqlx::PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain")
        .delivered
}

async fn send_message(router: &Router, sender: &str, recipient: &str, subject: &str) -> String {
    let resp = TestRequest::post("/api/messages/send")
        .json(&serde_json::json!({
            "sender_id": sender,
            "recipient_id": recipient,
            "subject": subject,
            "body": format!("body of {subject}"),
            "kind": "direct",
        }))
        .send(router)
        .await;
    resp.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = resp.assert_json();
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_projection_after_drop() {
    let db = TestDb::new().await;
    let router = build_app(db.pool.clone());

    // 1. Drive a realistic mix through the API: 4 messages sent, 2
    //    read, 1 archived, 1 deleted. Each call lands a row in
    //    messages AND an event in audit_log.
    let m1 = send_message(&router, "emp-a", "emp-b", "first").await;
    let m2 = send_message(&router, "emp-a", "emp-b", "second").await;
    let m3 = send_message(&router, "emp-c", "emp-b", "third").await;
    let m4 = send_message(&router, "emp-a", "emp-c", "fourth").await;

    TestRequest::post(format!("/api/messages/{m1}/read"))
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post(format!("/api/messages/{m3}/read"))
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post(format!("/api/messages/{m2}/archive"))
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    TestRequest::delete(format!("/api/messages/{m4}"))
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 2. Drain the outbox into audit_log, then snapshot the
    //    projection. m4 should be gone (deleted), m2's kind should
    //    be 'archived', m1+m3 should have read_at set.
    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 8, "4 sent + 2 read + 1 archived + 1 deleted");
    let before = snapshot_messages(&db.pool).await;
    assert_eq!(before.len(), 3, "post-delete count");
    let archived: Vec<_> = before.iter().filter(|r| r.kind == "archived").collect();
    assert_eq!(archived.len(), 1, "exactly one archived");
    assert_eq!(archived[0].id, m2);
    let read: Vec<_> = before.iter().filter(|r| r.read_at.is_some()).collect();
    assert_eq!(read.len(), 2, "two messages marked read");

    // The archived row has left emp-b's inbox read, and is still there
    // for a reader that asks for it (backlog 8578b91e / 5963a322).
    async fn inbox_ids(router: &Router, uri: &str) -> Vec<String> {
        let resp = TestRequest::get(uri).send(router).await;
        resp.assert_status(StatusCode::OK);
        let rows: Vec<serde_json::Value> = resp.assert_json();
        let mut ids: Vec<String> = rows
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort_unstable();
        ids
    }
    let mut kept = vec![m1.clone(), m3.clone()];
    kept.sort_unstable();
    assert_eq!(inbox_ids(&router, "/api/messages/inbox/emp-b").await, kept);
    let mut all = vec![m1.clone(), m2.clone(), m3.clone()];
    all.sort_unstable();
    assert_eq!(
        inbox_ids(&router, "/api/messages/inbox/emp-b?include_archived=true").await,
        all
    );

    // 3. Verify audit_log has the full event sequence — 4 sent + 2
    //    read + 1 archived + 1 deleted = 8 events.
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'messages.message.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_count.0, 8, "8 events emitted");

    // 4. Blow away the projection. (We don't drop audit_log rows —
    //    that's the point: events are canonical, projections derive.)
    sqlx::query("DELETE FROM messages")
        .execute(&db.pool)
        .await
        .unwrap();
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "messages projection wiped");

    // 5. Rebuild from audit_log alone.
    let report = rebuild_messages(&db.pool).await.expect("rebuild succeeds");

    assert_eq!(report.events_processed, 8);
    assert_eq!(report.rows_inserted, 4);
    assert_eq!(report.rows_marked_read, 2);
    assert_eq!(report.rows_archived, 1);
    assert_eq!(report.rows_deleted, 1);

    // 6. The reconstructed projection should match the original
    //    bit-for-bit (same ids, same content, same read_at / kind).
    let after = snapshot_messages(&db.pool).await;
    assert_eq!(
        before, after,
        "rebuilt projection should match the pre-rebuild snapshot exactly"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_is_idempotent() {
    let db = TestDb::new().await;
    let router = build_app(db.pool.clone());

    let _m1 = send_message(&router, "emp-a", "emp-b", "alpha").await;
    let m2 = send_message(&router, "emp-a", "emp-b", "beta").await;
    TestRequest::post(format!("/api/messages/{m2}/read"))
        .send(&router)
        .await
        .assert_status(StatusCode::OK);

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 3, "2 sent + 1 read arrive via the outbox");
    let baseline = snapshot_messages(&db.pool).await;

    // Two consecutive rebuilds should both land on the same state.
    rebuild_messages(&db.pool).await.unwrap();
    let after_first = snapshot_messages(&db.pool).await;
    assert_eq!(baseline, after_first);

    rebuild_messages(&db.pool).await.unwrap();
    let after_second = snapshot_messages(&db.pool).await;
    assert_eq!(baseline, after_second);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_skips_pre_enrichment_sent_events() {
    // Mimic an audit_log slice that pre-dates the payload enrichment —
    // SENT events with only `{id}` should be skipped (and surface in
    // events_skipped) rather than abort the rebuild.
    let db = TestDb::new().await;

    // Hand-write three audit_log rows: an unenriched SENT, a fully
    // enriched SENT, and a READ that targets the enriched one.
    sqlx::query(
        "INSERT INTO audit_log (event_id, source, kind, payload) VALUES \
         (gen_random_uuid(), 'messages', 'messages.message.sent', $1::jsonb), \
         (gen_random_uuid(), 'messages', 'messages.message.sent', $2::jsonb), \
         (gen_random_uuid(), 'messages', 'messages.message.read',  $3::jsonb)",
    )
    .bind(serde_json::json!({ "id": "msg-old" }))
    .bind(serde_json::json!({
        "id": "msg-new",
        "sender_id": "emp-a",
        "recipient_id": "emp-b",
        "subject": "with full state",
        "body": "ok",
        "kind": "direct",
        "sent_at": Utc::now(),
        "read_at": null,
        "reply_to": null,
    }))
    .bind(serde_json::json!({
        "id": "msg-new",
        "read_at": Utc::now(),
    }))
    .execute(&db.pool)
    .await
    .unwrap();

    let report = rebuild_messages(&db.pool).await.unwrap();

    assert_eq!(report.events_processed, 3);
    assert_eq!(report.events_skipped, 1, "the bare-id SENT was skipped");
    assert_eq!(report.rows_inserted, 1);
    assert_eq!(report.rows_marked_read, 1);

    // Only the enriched message survives.
    let rows = snapshot_messages(&db.pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "msg-new");
    assert!(rows[0].read_at.is_some());
}

/// Backlog 0b2bac00, against the real adapter: a step's direct notice
/// retired through the expire door leaves the unread-direct count the
/// badge reads, a person's direct about the same step stays, and the
/// retirement is one `messages.message.archived` per row — so the
/// rebuilder lands on the same projection from the log alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_retired_step_notice_leaves_the_badge_and_rebuilds() {
    let db = TestDb::new().await;
    let router = build_app(db.pool.clone());
    let step = "/jobs/job-1/steps/step-1";

    for (id, sender) in [
        ("notify:step-1:emp_d", "automation:dispatcher"),
        ("msg-from-a-person", "emp-colleague"),
    ] {
        TestRequest::post("/api/messages/send")
            .json(&serde_json::json!({
                "id": id,
                "sender_id": sender,
                "recipient_id": "emp_d",
                "subject": "Ready: task",
                "body": "b",
                "kind": "direct",
                "entity_ref": {
                    "entity_type": "step",
                    "entity_id": "step-1",
                    "entity_path": step,
                },
            }))
            .send(&router)
            .await
            .assert_status(StatusCode::CREATED);
    }

    async fn unread_direct(router: &Router) -> u64 {
        let resp = TestRequest::get("/api/messages/unread/emp_d?kind=direct")
            .send(router)
            .await;
        resp.assert_status(StatusCode::OK);
        let v: serde_json::Value = resp.assert_json();
        v["count"].as_u64().unwrap()
    }
    assert_eq!(unread_direct(&router).await, 2);

    // `notify_` would match `notify:` under LIKE, where `_` is a
    // wildcard; the adapter compares the prefix literally.
    let resp = TestRequest::post("/api/messages/expire")
        .json(&serde_json::json!({ "entity_path_prefix": step, "id_prefix": "notify_" }))
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["expired"], 0, "the id prefix is literal, not a pattern");

    let resp = TestRequest::post("/api/messages/expire")
        .json(&serde_json::json!({ "entity_path_prefix": step, "id_prefix": "notify:" }))
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["expired"], 1);
    assert_eq!(
        unread_direct(&router).await,
        1,
        "the notice left the count; the person's question did not"
    );

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 3, "2 sent + 1 archived");
    let before = snapshot_messages(&db.pool).await;
    let archived: Vec<_> = before.iter().filter(|r| r.kind == "archived").collect();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, "notify:step-1:emp_d");

    sqlx::query("DELETE FROM messages")
        .execute(&db.pool)
        .await
        .unwrap();
    rebuild_messages(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(
        before,
        snapshot_messages(&db.pool).await,
        "the retirement rebuilds from the log alone"
    );
}
