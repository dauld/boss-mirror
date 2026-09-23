//! A refused presence ceremony leaves its reason in the gateway log —
//! and nothing a credential is made of.
//!
//! Until backlog f3436d99 (2026-09-23) `assert_begin` and
//! `assert_finish` returned their refusal to the browser and wrote
//! nothing on the server, so a failed presence approval could be
//! diagnosed only from whatever the browser chose to show — and both
//! browser copies of the ceremony dropped the text (2e893e27 fixed the
//! app's; the sign-off plugin's rides with this test's car).
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the gateway LOGGED, and a captured log is
//! deterministic only when the capturing test is alone in its process
//! with the subscriber installed globally before the first event
//! (`boss-core/tests/common/mod.rs` carries the mechanism and the
//! measurement; `boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape).

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"presence-refusal-test-key";
const EMPLOYEE: &str = "emp-refused-7";
// Values the log must never carry: a challenge id, a credential id,
// and the stored challenge and nonce a verifier compares against.
const SPENT_CHALLENGE: &str = "chal-spent-4f1d";
const LIVE_CHALLENGE: &str = "chal-live-9a2c";
const CREDENTIAL_ID: &str = "Y3JlZC1zZWNyZXQtaWQ";
const STORED_CHALLENGE: &str = "c3RvcmVkLWNoYWxsZW5nZS1ieXRlcy0zMi1sb25nLi4";
const NONCE: &str = "nonce-must-not-log-5e";

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

/// jobs + people in one stub: the job read is refused, one challenge
/// is spent, the other consumes to a well-formed presence row, and the
/// employee holds no stored passkeys.
async fn upstream() -> String {
    let app = Router::new()
        .route(
            "/api/jobs/{id}",
            get(|| async { (StatusCode::FORBIDDEN, "denied") }),
        )
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    if id == SPENT_CHALLENGE {
                        return (StatusCode::GONE, axum::Json(json!({}))).into_response();
                    }
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "flow": "presence",
                            "employee_id": EMPLOYEE,
                            "challenge": STORED_CHALLENGE,
                            "step_id": "step-1",
                            "shape_hash": "shape-1",
                            "nonce": NONCE,
                        })),
                    )
                        .into_response()
                },
            ),
        )
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(|| async { axum::Json(json!([])) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn call(router: &Router, path: &str, body: Value) -> (StatusCode, String) {
    let mut sess = Session::new("refused", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
    let req = Request::post(path)
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

/// The one log line a refusal wrote, found by the employee it names.
fn line_for<'a>(log: &'a str, needle: &str) -> &'a str {
    log.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no log line carries {needle:?}; the log was:\n{log}"))
}

#[tokio::test]
async fn a_refused_presence_ceremony_logs_its_reason_and_no_credential_material() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let base = upstream().await;
    let origin = Url::parse("https://boss.test").unwrap();
    let state = Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        people_base: base.clone(),
        jobs_base: base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
    });
    let router = passkey_router(state);

    // 1. begin: the job read is refused upstream.
    let (status, body) = call(
        &router,
        "/api/auth/passkey/assert/begin",
        json!({"job_id": "job-1", "step_id": "step-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    // 2. finish: the challenge was already spent.
    let (status, body) = call(
        &router,
        "/api/auth/passkey/assert/finish",
        json!({"challenge_id": SPENT_CHALLENGE, "credential": credential()}),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    // 3. finish: webauthn itself rejects the assertion.
    let (status, rejected) = call(
        &router,
        "/api/auth/passkey/assert/finish",
        json!({"challenge_id": LIVE_CHALLENGE, "credential": credential()}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{rejected}");
    let webauthn_text = rejected
        .strip_prefix("assertion rejected: ")
        .unwrap_or_else(|| panic!("the refusal the browser reads changed: {rejected}"));

    let out = log.text();
    let begin = line_for(&out, "job fetch");
    assert!(begin.contains("WARN"), "a refusal logs at WARN: {begin}");
    assert!(begin.contains(EMPLOYEE), "it names the employee: {begin}");
    assert!(
        begin.contains("403"),
        "it names the upstream status: {begin}"
    );

    let spent = line_for(&out, "status=410");
    assert!(spent.contains(EMPLOYEE), "it names the employee: {spent}");
    assert!(
        spent.contains("challenge consume"),
        "it names the stage that refused: {spent}"
    );

    let verifier = line_for(&out, webauthn_text);
    assert!(
        verifier.contains(EMPLOYEE),
        "it names the employee: {verifier}"
    );
    assert!(verifier.contains("status=401"), "{verifier}");

    for secret in [
        SPENT_CHALLENGE,
        LIVE_CHALLENGE,
        CREDENTIAL_ID,
        STORED_CHALLENGE,
        NONCE,
    ] {
        assert!(
            !out.contains(secret),
            "the log carries {secret:?}, which it must never hold:\n{out}"
        );
    }
}

/// A well-formed credential envelope whose contents no verifier
/// accepts — enough to reach `finish_passkey_authentication`.
fn credential() -> Value {
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
