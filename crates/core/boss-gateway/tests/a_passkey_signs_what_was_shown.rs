//! A PASSKEY SIGNS WHAT THE APPROVER WAS SHOWN (backlog fd7090cc, the
//! security re-review of 2026-09-25).
//!
//! `POST /api/auth/passkey/assert/begin` mints the challenge over the
//! step's shape hash as the gateway reads it AT BEGIN. The browser used
//! to send only `{job_id, step_id}`, so the binding was to whatever the
//! step said at that instant — not to what the approver had on screen.
//! An actor who can write the step could swap the plan, verb or args
//! between the render and the key press, and the passkey would sign the
//! swap (on `merge-tenant-main`, another branch). Now the ceremony names
//! the step content it SHOWED, and the gateway hashes that with the one
//! definition (`boss_core::job::step_shape_hash`) and refuses a begin
//! whose shown content is not the step as it stands — before anything
//! is minted. What this guards is an HONEST page from a swap between
//! its render and the key press. It is not proof of what was on screen:
//! the page supplies the content it names, so a page that displayed one
//! thing and named another is outside what this check can see (the
//! adversarial re-review's correction, 2026-09-25).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, Uri};
use axum::routing::get;
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"presence-shown-session-test-key";
const EMPLOYEE: &str = "emp-presence-shown";
const JOB: &str = "5a2c1d7e-8b41-4e0c-9e5b-6a2f0c9d1e48";
const STEP: &str = "c9e1f3b5-2d4a-4f6b-8c8e-1a3f5d7b9c2e";
const TITLE: &str = "Approve the plan: merge-tenant-main on forge";

/// The approve step as it stands in the system of record.
fn current_metadata() -> Value {
    json!({
        "authority_role": "platform-admin",
        "plan": "MERGE tenant/main <- feature-a\n",
        "verb": "merge-tenant-main",
        "host": "forge",
        "args": ["feature-a"],
    })
}

/// Every path the stub was asked for, in order.
type Seen = Arc<Mutex<Vec<String>>>;

/// jobs + people in one stub. The job holds the approve step; the
/// employee holds no stored passkey, so a begin that PASSES the binding
/// stops at "no passkey enrolled" (409) and mints nothing either way.
async fn upstream() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let on_job = seen.clone();
    let app = Router::new()
        .route(
            "/api/jobs/{id}",
            get(move |uri: Uri| {
                on_job.lock().unwrap().push(uri.to_string());
                async {
                    axum::Json(json!({
                        "id": JOB,
                        "steps": [{ "id": STEP, "title": TITLE, "metadata": current_metadata() }],
                    }))
                }
            }),
        )
        .fallback(move |uri: Uri| {
            log.lock().unwrap().push(uri.to_string());
            async { axum::Json(json!([])) }
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
    let mut sess = Session::new("presence-shown", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
    sess.role = Some("platform-admin".to_string());
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

/// Nothing past the step read: no credential lookup, no mint.
fn nothing_past_the_step_read(seen: &Seen) {
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().all(|p| p.starts_with("/api/jobs/")),
        "a refused binding reads no passkey and mints no challenge: {seen:?}"
    );
}

#[tokio::test]
async fn a_begin_for_the_content_that_was_shown_proceeds() {
    let (base, _) = upstream().await;
    let router = router(base);
    let (status, body) = begin(
        &router,
        json!({"job_id": JOB, "step_id": STEP,
               "shown": {"title": TITLE, "metadata": current_metadata()}}),
    )
    .await;
    // Past the binding: the stub's employee holds no passkey.
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("no passkey enrolled"), "{body}");
}

#[tokio::test]
async fn a_step_that_changed_after_it_was_shown_is_refused_before_any_mint() {
    // The plan, the verb, the args, or the title moved between the render
    // and the key press.
    let mut other_plan = current_metadata();
    other_plan["plan"] = json!("MERGE tenant/main <- feature-b\n");
    let mut other_args = current_metadata();
    other_args["args"] = json!(["feature-b"]);
    let mut missing_key = current_metadata();
    missing_key.as_object_mut().unwrap().remove("host");
    let cases = [
        (TITLE, other_plan),
        (TITLE, other_args),
        (TITLE, missing_key),
        ("Approve the plan: something else", current_metadata()),
    ];
    for (title, shown) in cases {
        let (base, seen) = upstream().await;
        let router = router(base);
        let (status, body) = begin(
            &router,
            json!({"job_id": JOB, "step_id": STEP,
                   "shown": {"title": title, "metadata": shown}}),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{shown}: {body}");
        assert!(
            body.contains("changed since it was shown"),
            "the refusal says why, so the approver reloads and reads it again: {body}"
        );
        nothing_past_the_step_read(&seen);
    }
}

#[tokio::test]
async fn a_begin_that_names_nothing_it_showed_is_refused() {
    let (base, seen) = upstream().await;
    let router = router(base);
    let (status, body) = begin(&router, json!({"job_id": JOB, "step_id": STEP})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("shown"), "{body}");
    nothing_past_the_step_read(&seen);
}
