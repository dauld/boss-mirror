//! A CALLER-SUPPLIED ID CANNOT STEER A GATEWAY REQUEST OFF ITS PATH
//! (backlog a0dd9387, CodeQL rust/request-forgery on mirror PR 242).
//!
//! The passkey door calls the people service signed as the gateway's
//! own platform-admin actor (`sign_as_gateway`), and it formatted two
//! caller-supplied values into those request paths unencoded: the
//! `credential_id` of `DELETE /api/auth/passkey/credentials/{id}` (axum
//! decodes percent-escapes, so a slash arrives INSIDE the one segment)
//! and the `challenge_id` a ceremony's finish posts back. Either could
//! carry a traversal to another people path, and the privileged request
//! would follow it. So a value outside the id alphabet is refused with
//! 400 before any request is made, and a well-formed one still reaches
//! exactly its own path.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, Uri};
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"passkey-path-segment-test-key";
const EMPLOYEE: &str = "emp-path-segment-1";
const CREDENTIAL_ID: &str = "dGV4dC1jcmVkLWlk";

type Seen = Arc<Mutex<Vec<String>>>;

/// A people stub that answers 204 to anything and records every path
/// (with its query) it was asked for.
async fn recording_people() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = Router::new().fallback(move |uri: Uri| {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(uri.to_string());
            StatusCode::NO_CONTENT
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn router(people_base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        people_base: people_base.clone(),
        jobs_base: people_base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
    }))
}

async fn call(router: &Router, method: &str, path: &str, body: Value) -> StatusCode {
    let mut sess = Session::new("path-segment", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", format!("{COOKIE_NAME}={}", sess.encode(KEY)))
        .body(Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn a_traversal_credential_id_is_refused_before_any_request() {
    let (people, seen) = recording_people().await;
    let router = router(people);
    for id in ["..%2F..%2Femp-other", "..", "x%3Fall%3Dtrue", "x%23frag"] {
        let status = call(
            &router,
            "DELETE",
            &format!("/api/auth/passkey/credentials/{id}"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{id}");
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "no request left the gateway for a refused id: {:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn a_traversal_challenge_id_is_refused_before_any_request() {
    let (people, seen) = recording_people().await;
    let router = router(people);
    let assertion = json!({
        "id": CREDENTIAL_ID, "rawId": CREDENTIAL_ID, "type": "public-key",
        "response": { "authenticatorData": "AAAA", "clientDataJSON": "e30",
                      "signature": "AAAA", "userHandle": null },
        "extensions": {},
    });
    let status = call(
        &router,
        "POST",
        "/api/auth/passkey/assert/finish",
        json!({ "challenge_id": "../../emp-other", "credential": assertion }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        seen.lock().unwrap().is_empty(),
        "no request left the gateway for a refused id: {:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn a_well_formed_credential_id_reaches_exactly_its_own_path() {
    let (people, seen) = recording_people().await;
    let router = router(people);
    let status = call(
        &router,
        "DELETE",
        &format!("/api/auth/passkey/credentials/{CREDENTIAL_ID}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![format!(
            "/api/people/{EMPLOYEE}/webauthn-credentials/{CREDENTIAL_ID}"
        )]
    );
}
