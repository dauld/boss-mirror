//! THE PRESENCE CEREMONY READS THE STEP AS THE SESSION, AND ONLY FOR A
//! JOB AND STEP ID (backlog 18b9e09d, a residual of a0dd9387).
//!
//! `POST /api/auth/passkey/assert/begin` reads the step the passkey is
//! about to approve from the jobs service. Until this car it formatted
//! the caller's `job_id` into that request's path unchecked, and sent
//! the request signed as the gateway's own platform-admin actor with
//! the session's identity added as a second `x-boss-user` — and the
//! policy extractor downstream reads the first. So the read ran with
//! the gateway's scope rather than the employee's, on a path the
//! caller chose. Now a `job_id` or `step_id` that is not a UUID (every
//! job and step id is one — `define_id!` over a Uuid in boss-core) is
//! refused with 400 before any request, and the step read carries one
//! identity: the session's.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use axum::routing::get;
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"presence-read-session-test-key";
const EMPLOYEE: &str = "emp-presence-read-2";
const JOB: &str = "3f1c2b7e-9a41-4d0c-8e5b-6a2f0c9d1e47";
const STEP: &str = "b8d0e2a4-1c3f-4e5a-9b7d-0f2e4c6a8b1d";

/// Every request the stub was asked for: its path (with any query) and
/// every `x-boss-user` value it carried, in order.
type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// jobs + people in one stub. The job read answers a job holding STEP;
/// the employee holds no stored passkeys, so a begin that got as far as
/// the step read stops at "no passkey enrolled" and mints nothing.
async fn recording_upstream() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let record = move |uri: Uri, headers: HeaderMap| {
        let identities = headers
            .get_all("x-boss-user")
            .iter()
            .map(|v| v.to_str().unwrap_or("?").to_string())
            .collect();
        log.lock().unwrap().push((uri.to_string(), identities));
    };
    let on_job = record.clone();
    let app = Router::new()
        .route(
            "/api/jobs/{id}",
            get(move |uri: Uri, headers: HeaderMap| {
                on_job(uri, headers);
                async {
                    axum::Json(json!({
                        "id": JOB,
                        "steps": [{ "id": STEP, "title": "Approve", "metadata": {} }],
                    }))
                }
            }),
        )
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(|| async { axum::Json(json!([])) }),
        )
        .fallback(move |uri: Uri, headers: HeaderMap| {
            record(uri, headers);
            async { StatusCode::NO_CONTENT }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn router(base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        people_base: base.clone(),
        jobs_base: base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
    }))
}

async fn begin(router: &Router, body: Value) -> (StatusCode, String) {
    let mut sess = Session::new("presence-read", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
    sess.role = Some("brewmaster".to_string());
    let req = Request::post("/api/auth/passkey/assert/begin")
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
async fn a_job_or_step_id_that_is_not_a_uuid_is_refused_before_any_request() {
    let (base, seen) = recording_upstream().await;
    let router = router(base);
    let bad = [
        "../../api/stations/x/queue?",
        "..",
        "job-1",
        "",
        "3f1c2b7e-9a41-4d0c-8e5b-6a2f0c9d1e47/../x",
    ];
    for id in bad {
        let (status, body) = begin(&router, json!({ "job_id": id, "step_id": STEP })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "job_id {id:?}: {body}");
        let (status, body) = begin(&router, json!({ "job_id": JOB, "step_id": id })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "step_id {id:?}: {body}");
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "no request left the gateway for a refused id: {:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn the_step_read_carries_exactly_one_identity_and_it_is_the_sessions() {
    let (base, seen) = recording_upstream().await;
    let router = router(base);
    let (status, body) = begin(&router, json!({ "job_id": JOB, "step_id": STEP })).await;
    // The read succeeded and found the step; the stub's employee holds
    // no passkey, so the ceremony stops there.
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let seen = seen.lock().unwrap();
    let reads: Vec<&(String, Vec<String>)> = seen
        .iter()
        .filter(|(path, _)| path.starts_with("/api/jobs/"))
        .collect();
    assert_eq!(reads.len(), 1, "one step read: {seen:?}");
    let (path, identities) = reads[0];
    assert_eq!(path, &format!("/api/jobs/{JOB}"));
    assert_eq!(
        identities.len(),
        1,
        "exactly one x-boss-user reaches jobs: {identities:?}"
    );
    let user: Value = serde_json::from_str(&identities[0]).unwrap();
    assert_eq!(user["id"], EMPLOYEE, "the session's identity: {user}");
    assert_eq!(user["role"], "brewmaster", "the session's role: {user}");
}
