//! What the browser reads when a passkey ceremony refuses: a fixed
//! text and an unchanged status, never an upstream error.
//!
//! Backlog 3bddce66 (2026-09-23), four texts 56126dc7's builder found
//! still interpolating: `jobs unreachable: {e}` printed reqwest's
//! error, which names the internal jobs URL; `job malformed: {e}`,
//! `credential list malformed: {e}` and `challenge malformed: {e}`
//! printed a decode error, which quotes the value it choked on — and
//! the consume URL a reqwest error names carries the challenge id.
//! Since 2e893e27 and f3436d99 the web UI and the sign-off plugin
//! render the text. The reason a refusal was made goes to the gateway
//! log through `presence_refused`, whose log shape the two
//! `*_refusals_are_logged.rs` binaries pin; this file captures no log,
//! so it may hold more than one test.
//!
//! It also mounts the ceremony exactly as production does:
//! `passkey_router` is the one route list, and the gateway's `main.rs`
//! merges it — until 3bddce66 `main.rs` spelled the routes out itself,
//! including the credential removal `passkey_router` did not carry,
//! so the router the tests drove was not the router that served.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post};
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"passkey-refusal-text-test-key";
const EMPLOYEE: &str = "emp-refusal-text-3";
/// A stored value a decode error would quote back.
const STORED: &str = "stored-value-the-browser-must-not-read";
const CHALLENGE: &str = "chal-text-61d0";
const CREDENTIAL_ID: &str = "dGV4dC1jcmVkLWlk";
/// Job and step ids are UUIDs, and the begin refuses anything else
/// before it reads (backlog 18b9e09d).
const JOB: &str = "0c4e1f8a-2b6d-4a9e-8f3c-5d7b9e1a3c5f";
const STEP: &str = "7a9c1e3b-5d8f-4b2a-9c6e-1f3a5b7d9e2c";

/// jobs + people in one stub, every read answering 200 with a body
/// that is not the shape asked for, and the removal succeeding.
async fn malformed_upstream() -> String {
    let app = Router::new()
        .route("/api/jobs/{id}", get(|| async { STORED }))
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(|| async { axum::Json(json!(STORED)) }),
        )
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(|| async { STORED }),
        )
        .route(
            "/api/people/{id}/webauthn-credentials/{cid}",
            delete(|| async { StatusCode::NO_CONTENT }),
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

fn router(people_base: String, jobs_base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        people_base,
        jobs_base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
    }))
}

async fn call(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, String) {
    let mut sess = Session::new("refusal-text", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
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

#[tokio::test]
async fn an_unreachable_jobs_service_is_named_without_its_url() {
    let upstream = malformed_upstream().await;
    let jobs = dark().await;
    let router = router(upstream, jobs.clone());
    let (status, body) = call(
        &router,
        "POST",
        "/api/auth/passkey/assert/begin",
        json!({"job_id": JOB, "step_id": STEP}),
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_GATEWAY, "jobs unreachable"),
        "the refusal must not carry {jobs}"
    );
}

#[tokio::test]
async fn a_malformed_upstream_answer_is_named_without_quoting_it() {
    let upstream = malformed_upstream().await;
    let router = router(upstream.clone(), upstream);
    let cases = [
        (
            "/api/auth/passkey/assert/begin",
            json!({"job_id": JOB, "step_id": STEP}),
            "job malformed",
        ),
        (
            "/api/auth/passkey/register/begin",
            json!({}),
            "credential list malformed",
        ),
        (
            "/api/auth/passkey/assert/finish",
            json!({"challenge_id": CHALLENGE, "credential": assertion()}),
            "challenge malformed",
        ),
    ];
    for (path, request, text) in cases {
        let (status, body) = call(&router, "POST", path, request).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::BAD_GATEWAY, text),
            "{path}: the refusal must not carry {STORED:?} or {CHALLENGE:?}"
        );
    }
}

/// The router the tests drive is the one the gateway serves, so it
/// carries the removal too.
#[tokio::test]
async fn the_one_passkey_router_carries_the_credential_removal() {
    let upstream = malformed_upstream().await;
    let router = router(upstream.clone(), upstream);
    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/api/auth/passkey/credentials/{CREDENTIAL_ID}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// A well-formed assertion envelope — enough to reach the challenge
/// consume.
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
