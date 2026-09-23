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
