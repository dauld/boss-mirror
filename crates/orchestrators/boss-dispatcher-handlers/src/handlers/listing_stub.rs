//! A stand-in for the far side of a listing read, for the tests that
//! prove a handler REFUSES a body with no `data` array (backlog
//! 37fc5837) rather than reading it as an empty list.
//!
//! One definition rather than a copy per handler: seven sites in five
//! handlers take the same test, and each needs only "answer these
//! paths with these bodies, and tell me whether anything was written".
//! The judgement itself lives in `common::rows_or_refuse`; this only
//! lets a test drive the handler to it end to end, so the test fails
//! on the READ SITE and not on the helper.
//!
//! Test-only: declared `#[cfg(test)]` in `handlers/mod.rs`.

use axum::Router;
use axum::body::Bytes;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use boss_dispatcher::rules::handler::HandlerError;

/// The body a dark, narrowed or error-answering far side hands back: a
/// well-formed 200 with a count and no rows array. Before 37fc5837 every
/// site this module tests read it as "nothing there".
pub(crate) fn no_data_array() -> Value {
    json!({ "total": 3 })
}

/// An honest empty listing — the control beside the refusal: zero rows
/// is an answer, and `rows_or_refuse` puts no floor on the count.
pub(crate) fn empty_listing() -> Value {
    json!({ "data": [], "total": 0 })
}

/// A running stub: its base URL, and every non-GET request it saw as
/// `"METHOD /path"` — the writes a refusal must NOT have made.
pub(crate) struct Stub {
    pub base: String,
    writes: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    pub(crate) fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
}

/// Serve `answers` as `(route, body)`. A route is a path plus optional
/// `k=v` query pairs that must ALL be present on the request, so two
/// reads of one path (open cars vs closed cars) answer differently. The
/// first matching route wins; an unmatched GET is a 404. Every other
/// method is recorded as a write and answered with a created id.
pub(crate) async fn serve(answers: Vec<(&'static str, Value)>) -> Stub {
    let writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = writes.clone();
    let answers = Arc::new(answers);
    let app = Router::new().fallback(move |method: Method, uri: Uri, _body: Bytes| {
        let answers = answers.clone();
        let log = log.clone();
        async move {
            if method != Method::GET {
                // A write to a step the fixtures hold as finished is
                // refused, as the real step API refuses it (backlog
                // d4698bc2) — recorded, so a test can see the attempt.
                if method == Method::PUT
                    && let Some((id, sid)) = step_path(uri.path())
                    && step_is_terminal(&fixture_rows(&answers), id, sid)
                {
                    log.lock()
                        .unwrap()
                        .push(format!("{method} {} (409)", uri.path()));
                    return terminal_step_refusal(sid);
                }
                log.lock().unwrap().push(format!("{method} {}", uri.path()));
                return axum::Json(json!({ "id": "stub-created" })).into_response();
            }
            answer(&answers, &uri)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Stub {
        base: format!("http://{addr}"),
        writes,
    }
}

/// `/api/jobs/{id}/steps/{sid}` split into its two ids.
fn step_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/jobs/")?;
    let (id, sid) = rest.split_once("/steps/")?;
    (!id.is_empty() && !sid.is_empty() && !sid.contains('/')).then_some((id, sid))
}

/// Every packet row the stub's fixtures answer with. A fixture with no
/// `data` array holds no packets — deliberately empty here, because the
/// only question is which steps this stub knows to be finished, and a
/// refusal fixture holds none.
fn fixture_rows(answers: &[(&'static str, Value)]) -> Vec<Value> {
    answers
        .iter()
        .flat_map(|(_, body)| {
            super::common::rows_or_refuse::<Value>(body, "a stub fixture").unwrap_or_default()
        })
        .collect()
}

/// Does `rows` hold step `sid` of packet `id` at a terminal status?
///
/// WHY A STUB MUST ASK (backlog d4698bc2). The real step API refuses a
/// metadata write to a completed or skipped step with 409 (`boss-jobs`
/// `http/steps.rs`: "step is terminal — these fields are immutable").
/// The estate stub answered every PUT with success and the cadence
/// sweep had no stub at all, so a handler completing a triage step a
/// person had already answered passed its tests while the live API
/// refused it on every pass — which is how the retraction 409 went
/// unseen until a2d8bad3. `sensor_poll`'s stub learned this first
/// (6072ff60); this is the one definition the others share.
pub(crate) fn step_is_terminal(rows: &[Value], id: &str, sid: &str) -> bool {
    rows.iter()
        .filter(|r| r.get("id").and_then(Value::as_str) == Some(id))
        .flat_map(|r| {
            r.get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .any(|s| {
            s.get("id").and_then(Value::as_str) == Some(sid)
                && matches!(
                    s.get("status").and_then(Value::as_str),
                    Some("completed") | Some("skipped")
                )
        })
}

/// The 409 the real step API answers a write to a terminal step with —
/// its error and its hint, so a handler's error text reads the same
/// under test as in the dead-letter.
pub(crate) fn terminal_step_refusal(sid: &str) -> Response {
    (
        StatusCode::CONFLICT,
        axum::Json(json!({
            "error": "step is terminal — these fields are immutable",
            "step_id": sid,
            "hint": "a completed step is a record of what happened. To correct or \
                     annotate it, write to the parent job's metadata \
                     (PATCH /api/jobs/{id}/metadata) instead.",
        })),
    )
        .into_response()
}

fn answer(answers: &[(&'static str, Value)], uri: &Uri) -> Response {
    let asked: Vec<&str> = uri.query().unwrap_or_default().split('&').collect();
    answers
        .iter()
        .find(|(route, _)| {
            let (path, query) = route.split_once('?').unwrap_or((route, ""));
            path == uri.path()
                && query
                    .split('&')
                    .filter(|p| !p.is_empty())
                    .all(|p| asked.contains(&p))
        })
        .map(|(_, body)| axum::Json(body.clone()).into_response())
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

/// The shape every one of these tests asserts: a Downstream refusal
/// that says there was no `data` array AND names the read, so the
/// operator reading the dead-letter knows which listing lied.
pub(crate) fn assert_refused_by_name(res: Result<(), HandlerError>, read: &str) {
    match res {
        Err(HandlerError::Downstream(why)) => {
            assert!(why.contains("no `data` array"), "{why}");
            assert!(why.contains(read), "the refusal names {read:?}: {why}");
        }
        other => panic!("a body with no `data` array must refuse, naming {read:?}; got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stub is only as honest as the API it stands in for: a PUT to
    /// a step its fixtures hold as completed is a 409, recorded as
    /// refused, while a PUT to a ready step still succeeds.
    #[tokio::test]
    async fn a_put_to_a_finished_step_is_refused_409_as_the_step_api_refuses_it() {
        let stub = serve(vec![(
            "/api/jobs",
            json!({ "data": [{ "id": "j1", "steps": [
                { "id": "j1-triage", "status": "completed" },
                { "id": "j1-build", "status": "ready" },
            ]}], "total": 1 }),
        )])
        .await;
        let client = reqwest::Client::new();
        let put = |sid: &str| {
            client
                .put(format!("{}/api/jobs/j1/steps/{sid}", stub.base))
                .json(&json!({ "status": "completed" }))
                .send()
        };
        assert_eq!(put("j1-triage").await.unwrap().status(), 409);
        assert!(put("j1-build").await.unwrap().status().is_success());
        assert_eq!(
            stub.writes(),
            vec![
                "PUT /api/jobs/j1/steps/j1-triage (409)".to_string(),
                "PUT /api/jobs/j1/steps/j1-build".to_string(),
            ]
        );
    }
}
