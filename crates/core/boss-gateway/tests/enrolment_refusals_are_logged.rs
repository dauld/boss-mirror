//! A refused enrolment leaves its reason in the gateway log, and no
//! refusal the browser reads carries an internal URL or stored
//! credential material.
//!
//! Backlog 56126dc7 (2026-09-23), the two gaps the builder of f3436d99
//! left named: `register_begin`, `register_finish` and
//! `credentials_remove` refused to the browser and logged nothing —
//! the shape f3436d99 fixed for the assertion handlers — and two
//! refusal texts carried internal detail to the browser. `people
//! unreachable: {e}` printed reqwest's error, which names the URL it
//! failed on, and the consume URL carries the challenge id (the
//! removal URL carries the credential id); `authentication state
//! rebuild failed: {e}` printed a serde error, which quotes the stored
//! value it choked on. Since 2e893e27 and f3436d99 both texts render in
//! the web UI and the sign-off plugin.
//!
//! **This file holds exactly one test, and that is the point** — the
//! reasoning is `presence_refusals_are_logged.rs`'s, and
//! `boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"enrolment-refusal-test-key";
/// The employee whose people service answers.
const LIVE: &str = "emp-enrol-live-4";
/// The employee whose people service is down.
const DARK: &str = "emp-enrol-dark-8";
// Values neither the log nor a browser-facing refusal may carry.
const REGISTER_CHALLENGE: &str = "chal-register-7c1e";
const DARK_CHALLENGE: &str = "chal-dark-b20d";
const PRESENCE_CHALLENGE: &str = "chal-presence-5a91";
const CREDENTIAL_ID: &str = "ZW5yb2wtY3JlZC1pZC1zZWNyZXQ";
const STORED_CRED: &str = "stored-credential-material-must-not-leak";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The live people service: one stored row whose public key does not
/// decode to a credential, a challenge mint that fails, every consumed
/// challenge a presence row for `LIVE`, and a removal it refuses.
async fn people() -> String {
    let stored = URL_SAFE_NO_PAD.encode(json!({ "cred": STORED_CRED }).to_string());
    let app = Router::new()
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(move || {
                let stored = stored.clone();
                async move {
                    axum::Json(json!([{
                        "credential_id": CREDENTIAL_ID,
                        "public_key": stored,
                    }]))
                }
            }),
        )
        .route(
            "/api/people/presence-challenges",
            post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "mint down") }),
        )
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(|| async {
                axum::Json(json!({
                    "flow": "presence",
                    "employee_id": LIVE,
                    "challenge": "c3RvcmVkLWNoYWxsZW5nZQ",
                    "step_id": "step-1",
                    "shape_hash": "shape-1",
                    "nonce": "nonce-1",
                }))
                .into_response()
            }),
        )
        .route(
            "/api/people/{id}/webauthn-credentials/{cid}",
            delete(|| async { (StatusCode::CONFLICT, "the last passkey stays") }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// An address nothing listens on: bound, then released.
async fn dark() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

/// The ceremony's routes, as the gateway's `main.rs` mounts them —
/// `passkey_router` carries the removal since backlog 3bddce66.
fn router(people_base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        jobs_base: people_base.clone(),
        people_base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
    }))
}

async fn call(
    router: &Router,
    employee: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let mut sess = Session::new("enrol", 600);
    sess.employee_id = Some(employee.to_string());
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", format!("{COOKIE_NAME}={}", sess.encode(KEY)))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The one log line a refusal wrote, found by its ceremony and the
/// employee it names.
fn line_for<'a>(log: &'a str, ceremony: &str, employee: &str) -> &'a str {
    log.lines()
        .find(|l| l.contains(ceremony) && l.contains(employee))
        .unwrap_or_else(|| {
            panic!("no log line names {ceremony} for {employee}; the log was:\n{log}")
        })
}

#[tokio::test]
async fn a_refused_enrolment_logs_its_reason_and_the_browser_reads_no_internal_detail() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let live = router(people().await);
    let dark_base = dark().await;
    let dark = router(dark_base.clone());
    let begin = "/api/auth/passkey/register/begin";
    let finish = "/api/auth/passkey/register/finish";
    let remove = format!("/api/auth/passkey/credentials/{CREDENTIAL_ID}");

    // Enrolment refusals, people answering: each keeps its text.
    let (status, body) = call(&live, LIVE, "POST", begin, json!({})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.starts_with("challenge mint failed"), "{body}");
    let (status, body) = call(
        &live,
        LIVE,
        "POST",
        finish,
        json!({"challenge_id": REGISTER_CHALLENGE, "credential": attestation()}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body, "challenge was minted for someone else");
    let (status, body) = call(&live, LIVE, "DELETE", &remove, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body, "the last passkey stays",
        "the rule's own words pass through"
    );

    // People unreachable: same status, and a text with no URL in it.
    let (status, body) = call(&dark, DARK, "POST", begin, json!({})).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_GATEWAY, "people unreachable")
    );
    let (status, body) = call(
        &dark,
        DARK,
        "POST",
        finish,
        json!({"challenge_id": DARK_CHALLENGE, "credential": attestation()}),
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_GATEWAY, "people unreachable")
    );
    let (status, body) = call(&dark, DARK, "DELETE", &remove, json!({})).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_GATEWAY, "people unreachable")
    );

    // A stored row that is no credential: same status, no quoted value.
    let (status, body) = call(
        &live,
        LIVE,
        "POST",
        "/api/auth/passkey/assert/finish",
        json!({"challenge_id": PRESENCE_CHALLENGE, "credential": assertion()}),
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication state rebuild failed"
        )
    );

    let out = log.text();
    for (ceremony, employee, status) in [
        ("register_begin", LIVE, "status=502"),
        ("register_finish", LIVE, "status=403"),
        ("credentials_remove", LIVE, "status=409"),
        ("register_begin", DARK, "status=502"),
        ("register_finish", DARK, "status=502"),
        ("credentials_remove", DARK, "status=502"),
    ] {
        let line = line_for(&out, ceremony, employee);
        assert!(line.contains("WARN"), "a refusal logs at WARN: {line}");
        assert!(line.contains(status), "it names the status: {line}");
    }
    // The reason names the stage, in fixed words — never the refusal's
    // own text, so a helper's failure is named by where it was called.
    for (ceremony, employee, stage) in [
        ("register_begin", LIVE, "challenge mint: 500"),
        ("register_finish", LIVE, "challenge minted for someone else"),
        ("credentials_remove", LIVE, "credential removal: 409"),
        ("register_begin", DARK, "stored passkeys"),
        ("register_finish", DARK, "challenge consume"),
        (
            "credentials_remove",
            DARK,
            "credential removal: people unreachable",
        ),
    ] {
        let line = line_for(&out, ceremony, employee);
        assert!(line.contains(stage), "it names the stage {stage:?}: {line}");
    }

    let dark_host = dark_base.trim_start_matches("http://");
    for secret in [
        REGISTER_CHALLENGE,
        DARK_CHALLENGE,
        PRESENCE_CHALLENGE,
        CREDENTIAL_ID,
        STORED_CRED,
        dark_host,
    ] {
        assert!(
            !out.contains(secret),
            "the log carries {secret:?}, which it must never hold:\n{out}"
        );
    }
}

/// A well-formed attestation envelope — enough to pass the body
/// extractor and reach the challenge consume.
fn attestation() -> Value {
    json!({
        "id": CREDENTIAL_ID,
        "rawId": CREDENTIAL_ID,
        "type": "public-key",
        "response": {
            "attestationObject": "AAAA",
            "clientDataJSON": "e30",
        },
        "extensions": {},
    })
}

/// A well-formed assertion envelope — enough to reach the rebuild of
/// the authentication state.
fn assertion() -> Value {
    json!({
        "id": CREDENTIAL_ID,
        "rawId": CREDENTIAL_ID,
        "type": "public-key",
        "response": {
            "authenticatorData": "AAAA",
            "clientDataJSON": "e30",
            "signature": "AAAA",
            "userHandle": null,
        },
        "extensions": {},
    })
}
