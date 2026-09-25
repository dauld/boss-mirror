//! HTTP-level write path tests for the messages service.
//!
//! Each test verifies one business contract via the actual HTTP router.

mod common;

use axum::http::StatusCode;
use boss_messages::types::{Message, MessageKind};
use boss_testing::TestRequest;
use common::{MessageTestApp, message_fixture};
use serde_json::json;

/// The machinery that sends, reads and expires on others' behalf signs
/// as an operator-tier automation: a request with no `x-boss-user` is
/// refused by every scoped door since backlog e84de48e (2026-09-25).
const OPERATOR: &str = r#"{"id":"automation:messages-test","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

// ---------------------------------------------------------------------------
// POST /api/messages/send — compose
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_send_returns_201_created() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hello",
        "body": "Test body",
    });

    let resp = TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&body)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_send_emits_message_sent_event() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hello",
        "body": "Test body",
    });

    TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&body)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    app.assert_recorded("messages.message.sent");
}

#[tokio::test]
async fn post_send_returns_new_message_id() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hi",
        "body": "Body",
    });

    let resp = TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&body)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::CREATED);

    let parsed: serde_json::Value = resp.assert_json();
    let id = parsed["id"].as_str().expect("response should include id");
    assert!(id.starts_with("msg-"), "expected msg- prefix, got {id}");
}

// ---------------------------------------------------------------------------
// POST /api/messages/{id}/read — mark as read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_read_returns_200_ok() {
    let msg = message_fixture("msg-read-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::post("/api/messages/msg-read-1/read")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn post_read_emits_message_read_event() {
    let msg = message_fixture("msg-read-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::post("/api/messages/msg-read-2/read")
        .send(&app.router)
        .await
        .assert_status(StatusCode::OK);

    let event = app.assert_recorded("messages.message.read");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-read-2"),
    );
}

// ---------------------------------------------------------------------------
// DELETE /api/messages/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_message_returns_204() {
    let msg = message_fixture("msg-del-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::delete("/api/messages/msg-del-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_message_emits_deleted_event() {
    let msg = message_fixture("msg-del-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::delete("/api/messages/msg-del-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.assert_recorded("messages.message.deleted");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-del-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_message_returns_404() {
    let app = MessageTestApp::new();

    let resp = TestRequest::delete("/api/messages/msg-missing")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_nonexistent_message_does_not_emit_event() {
    let app = MessageTestApp::new();

    TestRequest::delete("/api/messages/msg-missing")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.assert_not_recorded("messages.message.deleted");
}

// ---------------------------------------------------------------------------
// POST /api/messages/{id}/archive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn archive_existing_message_returns_204() {
    let msg = message_fixture("msg-arch-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::post("/api/messages/msg-arch-1/archive")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn archive_existing_message_emits_archived_event() {
    let msg = message_fixture("msg-arch-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::post("/api/messages/msg-arch-2/archive")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.assert_recorded("messages.message.archived");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-arch-2"),
    );
}

#[tokio::test]
async fn archive_nonexistent_message_returns_404() {
    let app = MessageTestApp::new();

    let resp = TestRequest::post("/api/messages/msg-missing/archive")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archive_nonexistent_message_does_not_emit_event() {
    let app = MessageTestApp::new();

    TestRequest::post("/api/messages/msg-missing/archive")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.assert_not_recorded("messages.message.archived");
}

// ---------------------------------------------------------------------------
// GET /api/messages/{id}/thread — conversation thread
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_thread_returns_reply_chain() {
    // Build a thread: root -> reply1 -> reply2
    let mut root = message_fixture("msg-root");
    root.reply_to = None;
    let mut reply1 = message_fixture("msg-reply-1");
    reply1.reply_to = Some("msg-root".to_string());
    let mut reply2 = message_fixture("msg-reply-2");
    reply2.reply_to = Some("msg-reply-1".to_string());

    let app = MessageTestApp::with_messages(vec![root, reply1, reply2]);

    let resp = TestRequest::get("/api/messages/msg-root/thread")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert!(
        msgs.len() >= 2,
        "expected thread to contain multiple messages, got {}",
        msgs.len()
    );
    // Root message must be in the thread.
    assert!(msgs.iter().any(|m| m.id == "msg-root"));
}

#[tokio::test]
async fn compose_with_reply_to_creates_reply() {
    let root = message_fixture("msg-parent");
    let app = MessageTestApp::with_messages(vec![root]);

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Re: Hi",
        "body": "Replying",
        "reply_to": "msg-parent",
    });

    let resp = TestRequest::post("/api/messages/send")
        .header("x-boss-user", OPERATOR)
        .json(&body)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::CREATED);

    let parsed: serde_json::Value = resp.assert_json();
    let new_id = parsed["id"].as_str().unwrap();

    // Fetch the new message and verify reply_to is set.
    let fetch = TestRequest::get(format!("/api/messages/{new_id}"))
        .send(&app.router)
        .await;
    fetch.assert_status(StatusCode::OK);
    let fetched: Message = fetch.assert_json();
    assert_eq!(fetched.reply_to.as_deref(), Some("msg-parent"));
}

// ---------------------------------------------------------------------------
// GET /api/messages/inbox/{employee_id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_inbox_returns_messages_for_recipient() {
    let mut m1 = message_fixture("msg-inbox-1");
    m1.recipient_id = "emp-42".to_string();
    let mut m2 = message_fixture("msg-inbox-2");
    m2.recipient_id = "emp-42".to_string();
    let mut m3 = message_fixture("msg-inbox-3");
    m3.recipient_id = "emp-other".to_string();

    let app = MessageTestApp::with_messages(vec![m1, m2, m3]);

    let resp = TestRequest::get("/api/messages/inbox/emp-42")
        .header("x-boss-user", OPERATOR)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().all(|m| m.recipient_id == "emp-42"));
}

#[tokio::test]
async fn get_inbox_for_unknown_employee_returns_empty_list() {
    let app = MessageTestApp::new();

    let resp = TestRequest::get("/api/messages/inbox/emp-ghost")
        .header("x-boss-user", OPERATOR)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert!(msgs.is_empty());
}

/// Backlog 8578b91e (page audit 5477d9eb, GAP 2): the inbox read was
/// `SELECT *` by recipient, so an archived row came back to a page
/// whose MessageKind knows only `direct` and `signal` — counted in
/// All, drawn with the direct glyph, counted Unread when the expire
/// rule had archived an unread signal, and offered Mark read. Archived
/// is the inbox's exit (backlog 5963a322 puts the Archive control on
/// the page), so the read leaves it out unless it is asked for.
fn archived_and_kept(recipient: &str) -> Vec<Message> {
    let mut kept = message_fixture("msg-kept");
    kept.recipient_id = recipient.to_string();
    let mut archived = message_fixture("msg-archived");
    archived.recipient_id = recipient.to_string();
    archived.kind = MessageKind::ARCHIVED.into();
    vec![kept, archived]
}

#[tokio::test]
async fn get_inbox_leaves_out_archived_rows() {
    let app = MessageTestApp::with_messages(archived_and_kept("emp-42"));

    let resp = TestRequest::get("/api/messages/inbox/emp-42")
        .header("x-boss-user", OPERATOR)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["msg-kept"]);
}

#[tokio::test]
async fn get_inbox_with_include_archived_returns_them_too() {
    let app = MessageTestApp::with_messages(archived_and_kept("emp-42"));

    let resp = TestRequest::get("/api/messages/inbox/emp-42?include_archived=true")
        .header("x-boss-user", OPERATOR)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    let mut ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["msg-archived", "msg-kept"]);
}

/// The page's Archive control is this round trip: the write records
/// its event, and the next inbox read no longer carries the row.
#[tokio::test]
async fn an_archived_message_leaves_the_inbox_read_and_records_its_event() {
    let mut m = message_fixture("msg-done");
    m.recipient_id = "emp-42".to_string();
    let app = MessageTestApp::with_messages(vec![m]);

    TestRequest::post("/api/messages/msg-done/archive")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    app.assert_recorded("messages.message.archived");

    let resp = TestRequest::get("/api/messages/inbox/emp-42")
        .header("x-boss-user", OPERATOR)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);
    let msgs: Vec<Message> = resp.assert_json();
    assert!(msgs.is_empty(), "archived row still in the inbox: {msgs:?}");
}

/// The unread count is a question about the same inbox, so an unread
/// row the expire rule archived (it leaves `read_at` NULL) is not
/// counted in it either. `?kind=archived` still asks for them by name.
#[tokio::test]
async fn get_unread_leaves_out_archived_rows_unless_asked_by_kind() {
    let app = MessageTestApp::with_messages(archived_and_kept("emp-42"));

    let count = |uri: &'static str, router: axum::Router| async move {
        let resp = TestRequest::get(uri)
            .header("x-boss-user", OPERATOR)
            .send(&router)
            .await;
        resp.assert_status(StatusCode::OK);
        let v: serde_json::Value = resp.assert_json();
        v["count"].as_u64().unwrap()
    };
    assert_eq!(
        count("/api/messages/unread/emp-42", app.router.clone()).await,
        1
    );
    assert_eq!(
        count(
            "/api/messages/unread/emp-42?kind=archived",
            app.router.clone()
        )
        .await,
        1
    );
}

// Silence unused warnings for MessageKind when only used in fixture.
#[allow(dead_code)]
fn _kind_marker() -> MessageKind {
    MessageKind::direct()
}

// ---------------------------------------------------------------------------
// POST /api/messages/expire — with an id prefix, the notices about a step
// ---------------------------------------------------------------------------

/// The notice an assignee got when a step became theirs, in the shape
/// the dispatcher's notifier sends it: a `direct`, id
/// `notify:{step}:{recipient}`, linked to the step.
fn step_notice(id: &str, recipient: &str, step_path: &str) -> Message {
    let mut m = message_fixture(id);
    m.sender_id = "automation:dispatcher".to_string();
    m.recipient_id = recipient.to_string();
    m.entity_ref = Some(boss_messages::types::EntityRef {
        entity_type: "step".to_string(),
        entity_id: "s1".to_string(),
        entity_path: Some(step_path.to_string()),
    });
    m
}

/// Backlog 0b2bac00: the unread-direct count is the badge and the
/// "waiting on you" headline, and a notice about a step that has ended
/// must leave it. Before this, the expire door moved signals only, so a
/// direct notice outlived its step for good.
#[tokio::test]
async fn post_expire_with_an_id_prefix_retires_a_steps_direct_notice() {
    let step = "/jobs/job-1/steps/s1";
    let mut from_a_person = step_notice("msg-human", "emp-d", step);
    from_a_person.sender_id = "emp-colleague".to_string();
    let app = MessageTestApp::with_messages(vec![
        step_notice("notify:s1:emp-d", "emp-d", step),
        from_a_person,
    ]);

    let unread_direct = |router: axum::Router| async move {
        let resp = TestRequest::get("/api/messages/unread/emp-d?kind=direct")
            .header("x-boss-user", OPERATOR)
            .send(&router)
            .await;
        resp.assert_status(StatusCode::OK);
        let v: serde_json::Value = resp.assert_json();
        v["count"].as_u64().unwrap()
    };
    assert_eq!(unread_direct(app.router.clone()).await, 2);

    let resp = TestRequest::post("/api/messages/expire")
        .header("x-boss-user", OPERATOR)
        .json(&json!({ "entity_path_prefix": step, "id_prefix": "notify:" }))
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["expired"], 1);

    assert_eq!(
        unread_direct(app.router.clone()).await,
        1,
        "the notice left the count; the person's question did not"
    );
    let event = app.assert_recorded("messages.message.archived");
    assert_eq!(event.payload["id"], "notify:s1:emp-d");
}

/// An empty id prefix would match every id under the path — a person's
/// direct included — so it is refused, as an empty path already is.
/// 422, because the dispatcher's POST reads that as permanent: the same
/// body fails the same way on every retry.
#[tokio::test]
async fn post_expire_refuses_an_empty_id_prefix() {
    let step = "/jobs/job-1/steps/s1";
    let app = MessageTestApp::with_messages(vec![step_notice("notify:s1:emp-d", "emp-d", step)]);

    let resp = TestRequest::post("/api/messages/expire")
        .header("x-boss-user", OPERATOR)
        .json(&json!({ "entity_path_prefix": step, "id_prefix": " " }))
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.assert_not_recorded("messages.message.archived");
}

/// Without an id prefix the door is what it was: unread signals only,
/// and a direct stays — the job-close rule relies on exactly that.
#[tokio::test]
async fn post_expire_without_an_id_prefix_still_leaves_a_direct() {
    let step = "/jobs/job-1/steps/s1";
    let app = MessageTestApp::with_messages(vec![step_notice("notify:s1:emp-d", "emp-d", step)]);

    let resp = TestRequest::post("/api/messages/expire")
        .header("x-boss-user", OPERATOR)
        .json(&json!({ "entity_path_prefix": "/jobs/job-1" }))
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);
    let v: serde_json::Value = resp.assert_json();
    assert_eq!(v["expired"], 0);
}
