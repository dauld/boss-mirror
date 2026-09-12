//! The presence ceremony's storage half, end to end against Postgres.
//!
//! 10-people.sql shipped webauthn_credentials + webauthn_challenges as
//! dormant intent on 2026-08-10; 151-presence-challenge-binding.sql
//! wakes them. These tests pin the storage contract the gateway's
//! ceremony depends on: credential round-trip, single-use challenge
//! consumption (atomic, evidence-leaving), and the presence flow's
//! step binding riding the row.

use axum::http::StatusCode;
use boss_people::webauthn::webauthn_router;
use boss_testing::{TestDb, TestRequest};
use serde_json::json;
use std::sync::Arc;

async fn app() -> (TestDb, axum::Router) {
    let db = TestDb::new().await;
    let router = webauthn_router(
        db.pool.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    (db, router)
}

// b64url("test-credential-id") and a fake COSE key, stable across tests.
const CRED_ID: &str = "dGVzdC1jcmVkZW50aWFsLWlk";
const PUB_KEY: &str = "cHVibGljLWtleS1ieXRlcw";

async fn seed_employee(db: &TestDb, id: &str) {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role) VALUES ($1, $1, $2, 'platform-admin')",
    )
    .bind(id)
    .bind(format!("{id}@test.invalid"))
    .execute(&db.pool)
    .await
    .expect("seed employee");
}

#[tokio::test]
async fn credential_round_trip_and_duplicate_conflict() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-1").await;

    TestRequest::post("/api/people/emp-wa-1/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY, "label": "yubikey-a"}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    // Same credential_id again is a conflict, not an upsert: a
    // credential is bound to one authenticator forever.
    TestRequest::post("/api/people/emp-wa-1/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CONFLICT);

    let resp = TestRequest::get("/api/people/emp-wa-1/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let creds: serde_json::Value = resp.assert_json();
    let list = creds.as_array().expect("array of credentials");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["credential_id"], CRED_ID);
    assert_eq!(list[0]["public_key"], PUB_KEY);
    assert_eq!(list[0]["label"], "yubikey-a");
    assert_eq!(list[0]["access_tier"], "user");
    assert_eq!(list[0]["sign_count"], 0);
    assert!(list[0]["last_used_at"].is_null());
}

#[tokio::test]
async fn recording_a_use_advances_sign_count_and_last_used() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-2").await;

    TestRequest::post("/api/people/emp-wa-2/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/webauthn-credentials/used")
        .json(&json!({"credential_id": CRED_ID, "sign_count": 7}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::get("/api/people/emp-wa-2/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(creds[0]["sign_count"], 7);
    assert!(!creds[0]["last_used_at"].is_null());
}

#[tokio::test]
async fn presence_challenge_consumes_exactly_once_with_binding() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-3").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-1",
            "employee_id": "emp-wa-3",
            "challenge": PUB_KEY,
            "flow": "presence",
            "step_id": "step-42",
            "shape_hash": "abc123",
            "nonce": "6e6f6e6365"
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::post("/api/people/presence-challenges/ch-1/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let row: serde_json::Value = resp.assert_json();
    assert_eq!(row["employee_id"], "emp-wa-3");
    assert_eq!(row["challenge"], PUB_KEY);
    assert_eq!(row["flow"], "presence");
    assert_eq!(row["step_id"], "step-42");
    assert_eq!(row["shape_hash"], "abc123");
    assert_eq!(row["nonce"], "6e6f6e6365");

    // Second consumption: the row exists but is spent — 410, not 404.
    TestRequest::post("/api/people/presence-challenges/ch-1/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn expired_challenge_is_gone_and_unknown_is_not_found() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-4").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-exp",
            "employee_id": "emp-wa-4",
            "challenge": PUB_KEY,
            "flow": "authenticate",
            "ttl_seconds": 0
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/presence-challenges/ch-exp/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::GONE);

    TestRequest::post("/api/people/presence-challenges/ch-nope/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let _ = db; // TestDb teardown on drop
}

#[tokio::test]
async fn bad_flow_and_bad_base64_are_rejected_before_the_db() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-5").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-bad",
            "employee_id": "emp-wa-5",
            "challenge": PUB_KEY,
            "flow": "vibes"
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    TestRequest::post("/api/people/emp-wa-5/webauthn-credentials")
        .json(&json!({"credential_id": "not!!base64", "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let _ = db;
}

#[tokio::test]
async fn an_ordinary_session_is_refused_at_the_storage_door() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-6").await;

    // The gateway proxies /api/people/{*rest} for every session, so
    // these paths are browser-reachable — and a credential row planted
    // for someone else is an account takeover. Ordinary roles stop
    // here; the ceremony's session-to-employee binding lives at the
    // gateway's /api/auth/passkey surface.
    TestRequest::post("/api/people/emp-wa-6/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("emp-wa-6", "qa-lead")
        .send(&router)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    TestRequest::get("/api/people/emp-wa-6/webauthn-credentials")
        .as_user("emp-wa-6", "qa-lead")
        .send(&router)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

// David, feedback 16414d99 (2026-09-10): "Let me manage the passkeys on
// my account… Important so users can add a backup key, but don't let
// them delete all their keys." Removal exists so a lost or retired
// authenticator can go; the LAST one stays, because a person with no
// passkey cannot pass a presence-gated step and the surface that let
// them do that would be the one that locked them out.
#[tokio::test]
async fn a_passkey_can_be_removed_but_never_the_last_one() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-5").await;
    const BACKUP_ID: &str = "YmFja3VwLWNyZWRlbnRpYWwtaWQ";

    for (id, label) in [(CRED_ID, "yubikey-a"), (BACKUP_ID, "backup")] {
        TestRequest::post("/api/people/emp-wa-5/webauthn-credentials")
            .json(&json!({"credential_id": id, "public_key": PUB_KEY, "label": label}))
            .as_user("automation:gateway", "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // Two keys: one may go.
    TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{CRED_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    // One key: it stays, and the refusal says why in the user's terms.
    let resp = TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{BACKUP_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await;
    resp.assert_status(StatusCode::CONFLICT);
    let text = resp.body_text();
    assert!(
        text.contains("last passkey") && text.contains("backup"),
        "the refusal names the rule and the way out: {text}"
    );

    let resp = TestRequest::get("/api/people/emp-wa-5/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(creds.as_array().map(Vec::len), Some(1));
    assert_eq!(creds[0]["credential_id"], BACKUP_ID);

    // A credential that is not there, or is somebody else's, is 404 —
    // never a silent 204 that reads as "removed".
    TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{CRED_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await
    .assert_status(StatusCode::NOT_FOUND);
}
