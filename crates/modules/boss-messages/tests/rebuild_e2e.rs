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

/// The machinery that sends, reads and expires on others' behalf signs
/// as an operator-tier automation: a request with no `x-boss-user` is
/// refused by every scoped door since backlog e84de48e (2026-09-25).
const OPERATOR: &str = r#"{"id":"automation:messages-test","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct MessageRow {
    id: String,
    sender_id: String,
    recipient_id: String,
    subject: String,
    body: String,
    entity_type: Option<String>,
    entity_id: Option<String>,
    entity_path: Option<String>,
    kind: String,
    sent_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
    reply_to: Option<String>,
    archived_at: Option<DateTime<Utc>>,
}

async fn snapshot_messages(pool: &sqlx::PgPool) -> Vec<MessageRow> {
    sqlx::query_as::<_, MessageRow>(
        "SELECT id, sender_id, recipient_id, subject, body, entity_type, entity_id, \
                entity_path, kind, sent_at, read_at, reply_to, archived_at \
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
        .header("x-boss-user", OPERATOR)
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

/// The `x-boss-user` of the person a message was sent to: marking read,
/// archiving and deleting are the recipient's to do (backlog 4dd10336).
fn signed_in(id: &str) -> String {
    serde_json::json!({ "id": id, "role": "employee", "access_tier": "user" }).to_string()
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
        .header("x-boss-user", signed_in("emp-b"))
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post(format!("/api/messages/{m3}/read"))
        .header("x-boss-user", signed_in("emp-b"))
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post(format!("/api/messages/{m2}/archive"))
        .header("x-boss-user", signed_in("emp-b"))
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    TestRequest::delete(format!("/api/messages/{m4}"))
        .header("x-boss-user", signed_in("emp-c"))
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 2. Drain the outbox into audit_log, then snapshot the
    //    projection. m4 should be gone (deleted), m2 archived — and
    //    still a direct (backlog 9bda9726) — m1+m3 read.
    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 8, "4 sent + 2 read + 1 archived + 1 deleted");
    let before = snapshot_messages(&db.pool).await;
    assert_eq!(before.len(), 3, "post-delete count");
    let archived: Vec<_> = before.iter().filter(|r| r.archived_at.is_some()).collect();
    assert_eq!(archived.len(), 1, "exactly one archived");
    assert_eq!(archived[0].id, m2);
    assert_eq!(archived[0].kind, "direct", "archiving kept the kind");
    let read: Vec<_> = before.iter().filter(|r| r.read_at.is_some()).collect();
    assert_eq!(read.len(), 2, "two messages marked read");

    // The archived row has left emp-b's inbox read, and is still there
    // for a reader that asks for it (backlog 8578b91e / 5963a322).
    async fn inbox_ids(router: &Router, uri: &str) -> Vec<String> {
        let resp = TestRequest::get(uri)
            .header("x-boss-user", OPERATOR)
            .send(router)
            .await;
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
        .header("x-boss-user", signed_in("emp-b"))
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
            .header("x-boss-user", OPERATOR)
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
            .header("x-boss-user", OPERATOR)
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
        .header("x-boss-user", OPERATOR)
        .json(&serde_json::json!({ "entity_path_prefix": step, "id_prefix": "notify_" }))
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["expired"], 0, "the id prefix is literal, not a pattern");

    let resp = TestRequest::post("/api/messages/expire")
        .header("x-boss-user", OPERATOR)
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
    let archived: Vec<_> = before.iter().filter(|r| r.archived_at.is_some()).collect();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, "notify:step-1:emp_d");
    assert_eq!(
        archived[0].kind, "direct",
        "a retired notice is still a direct"
    );

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

/// Backlog 9bda9726, against the real adapter. Two defects of one
/// shape — archive wrote `kind` — measured on the PgMessages door:
///
/// - IDEMPOTENCE. The single archive's UPDATE had no guard, so a second
///   archive of an archived message updated the row again and recorded
///   a second `messages.message.archived`. It is now a no-op: 204, and
///   one event in the log.
/// - CONSERVATION. Archive overwrote `kind` with `archived`, so the
///   projection lost direct-vs-signal. Both archive doors now set
///   `archived_at` and leave `kind` as sent.
///
/// And the rebuild lands on the same projection from the log alone.
#[tokio::test(flavor = "multi_thread")]
async fn archiving_twice_records_one_event_and_keeps_the_kind_through_a_rebuild() {
    let db = TestDb::new().await;
    let router = build_app(db.pool.clone());

    let direct = send_message(&router, "emp-a", "emp-b", "a question").await;
    let resp = TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&serde_json::json!({
            "sender_id": "automation:dispatcher",
            "recipient_id": "emp-b",
            "subject": "a step is ready",
            "body": "b",
            "kind": "signal",
            "entity_ref": {
                "entity_type": "job",
                "entity_id": "job-9",
                "entity_path": "/jobs/job-9",
            },
        }))
        .send(&router)
        .await;
    resp.assert_status(StatusCode::CREATED);
    let signal = resp.assert_json::<serde_json::Value>()["id"]
        .as_str()
        .unwrap()
        .to_string();

    for _ in 0..2 {
        TestRequest::post(format!("/api/messages/{direct}/archive"))
            .header("x-boss-user", signed_in("emp-b"))
            .send(&router)
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    for expected in [1, 0] {
        let resp = TestRequest::post("/api/messages/expire")
            .header("x-boss-user", OPERATOR)
            .json(&serde_json::json!({ "entity_path_prefix": "/jobs/job-9" }))
            .send(&router)
            .await;
        resp.assert_status(StatusCode::OK);
        let v: serde_json::Value = resp.assert_json();
        assert_eq!(v["expired"], expected, "the second expire moves nothing");
    }

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 4, "2 sent + ONE archived each");
    let per_id: Vec<(String, i64)> = sqlx::query_as(
        "SELECT payload->>'id', COUNT(*) FROM audit_log \
         WHERE kind = 'messages.message.archived' GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let mut expected = vec![(direct.clone(), 1), (signal.clone(), 1)];
    expected.sort_unstable();
    assert_eq!(per_id, expected, "archiving twice records one event");

    let before = snapshot_messages(&db.pool).await;
    let row = |id: &str| before.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(
        row(&direct).kind,
        "direct",
        "an archived direct reads as direct"
    );
    assert_eq!(
        row(&signal).kind,
        "signal",
        "an expired signal reads as signal"
    );
    assert!(row(&direct).archived_at.is_some());
    assert!(row(&signal).archived_at.is_some());

    let resp = TestRequest::get("/api/messages/inbox/emp-b")
        .header("x-boss-user", OPERATOR)
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let rows: Vec<serde_json::Value> = resp.assert_json();
    assert!(rows.is_empty(), "both left the inbox: {rows:?}");
    let resp = TestRequest::get("/api/messages/unread/emp-b?kind=direct")
        .header("x-boss-user", OPERATOR)
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["count"], 0, "the archived direct left the badge");

    sqlx::query("DELETE FROM messages")
        .execute(&db.pool)
        .await
        .unwrap();
    let report = rebuild_messages(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.rows_archived, 2);
    assert_eq!(
        before,
        snapshot_messages(&db.pool).await,
        "the rebuild reproduces archived_at AND the kind"
    );
}

const MIGRATION: &str =
    "infra/postgres/schema/20260926061106-an-archived-message-keeps-its-kind.sql";

fn migration_sql() -> String {
    std::fs::read_to_string(boss_testing::repo_root().join(MIGRATION))
        .unwrap_or_else(|e| panic!("reading {MIGRATION}: {e}"))
}

/// Backlog 9bda9726: the rows archived before this change sit in the
/// projection as `kind = 'archived'` with no `archived_at`. The
/// migration backfills them from the log — the kind from the message's
/// sent event, the time from its FIRST archived event (the old door
/// could record several) — and must land exactly where a rebuild from
/// the same log lands, or the next rebuild moves every one of them.
///
/// The shapes planted are the ones the old doors left: a direct
/// archived twice by the unguarded single door, a signal expired with
/// a nanosecond `archived_at` (the wall clock's precision; the column
/// holds microseconds, and Postgres ROUNDS a text cast where sqlx
/// TRUNCATES a bind), and an archive recorded with a bare `{id}`
/// payload, which both paths date from the log row's own timestamp.
#[tokio::test(flavor = "multi_thread")]
async fn the_migration_backfills_a_legacy_archived_row_the_way_the_rebuild_does() {
    let db = TestDb::new().await;
    let sent_at = "2026-09-20T10:00:00.123456Z";
    let sent = |id: &str, kind: &str| {
        serde_json::json!({
            "id": id, "sender_id": "emp-a", "recipient_id": "emp-b",
            "subject": format!("s {id}"), "body": "b", "kind": kind,
            "sent_at": sent_at, "read_at": null, "reply_to": null,
        })
    };
    let events = [
        ("messages.message.sent", sent("legacy-direct", "direct")),
        ("messages.message.sent", sent("legacy-signal", "signal")),
        ("messages.message.sent", sent("legacy-bare", "direct")),
        ("messages.message.sent", sent("never-archived", "direct")),
        (
            "messages.message.archived",
            serde_json::json!({ "id": "legacy-direct", "archived_at": "2026-09-21T09:00:00.000001Z" }),
        ),
        (
            "messages.message.archived",
            serde_json::json!({ "id": "legacy-direct", "archived_at": "2026-09-21T09:30:00Z" }),
        ),
        (
            "messages.message.archived",
            serde_json::json!({ "id": "legacy-signal", "archived_at": "2026-09-21T11:00:00.123456789Z", "reason": "entity-past-relevancy" }),
        ),
        (
            "messages.message.archived",
            serde_json::json!({ "id": "legacy-bare" }),
        ),
    ];
    for (kind, payload) in &events {
        sqlx::query(
            "INSERT INTO audit_log (event_id, source, kind, payload) \
             VALUES (gen_random_uuid(), 'messages', $1, $2)",
        )
        .bind(kind)
        .bind(payload)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    // The projection the OLD code left: every archived row's kind
    // overwritten, no archived_at. Built by a rebuild, then set back
    // to the legacy shape by the one statement the old doors ran.
    rebuild_messages(&db.pool).await.expect("rebuild succeeds");
    let rebuilt = snapshot_messages(&db.pool).await;
    sqlx::query(
        "UPDATE messages SET kind = 'archived', archived_at = NULL \
         WHERE id IN ('legacy-direct', 'legacy-signal', 'legacy-bare')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::raw_sql(&migration_sql())
        .execute(&db.pool)
        .await
        .expect("the migration applies to a legacy projection");
    let backfilled = snapshot_messages(&db.pool).await;

    let row = |id: &str| backfilled.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(row("legacy-direct").kind, "direct");
    assert_eq!(row("legacy-signal").kind, "signal");
    assert_eq!(row("legacy-bare").kind, "direct");
    assert_eq!(
        row("legacy-direct").archived_at,
        Some("2026-09-21T09:00:00.000001Z".parse().unwrap()),
        "the FIRST archive stands"
    );
    assert_eq!(
        row("legacy-signal").archived_at,
        Some("2026-09-21T11:00:00.123456Z".parse().unwrap()),
        "truncated to the microsecond, as a bind is"
    );
    assert!(row("legacy-bare").archived_at.is_some());
    assert!(row("never-archived").archived_at.is_none());
    assert_eq!(
        backfilled, rebuilt,
        "the backfill lands where the rebuild lands"
    );

    // Re-applied, it moves nothing: it only touches a legacy row.
    sqlx::raw_sql(&migration_sql())
        .execute(&db.pool)
        .await
        .expect("the migration re-applies");
    assert_eq!(snapshot_messages(&db.pool).await, backfilled);

    // `archived` is no longer a kind a sender may choose: the Class
    // row retires once no row carries it.
    let retired: bool = sqlx::query_scalar(
        "SELECT retired_at IS NOT NULL FROM classes \
         WHERE subject_kind = 'message' AND code = 'archived'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(retired, "the `archived` message kind is retired");
}
