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
    sent: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Stub {
    pub(crate) fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    /// Every ACCEPTED write as `("METHOD /path", body)` — what was
    /// written, for a test that asserts on the body and not only on
    /// the path (`ops_queue_alarm`, a45b38c1). A body that is not JSON
    /// is `null`; a write refused 409 is not here, it is in `writes`.
    pub(crate) fn sent(&self) -> Vec<(String, Value)> {
        self.sent.lock().unwrap().clone()
    }
}

/// Serve `answers` as `(route, body)`. A route is a path plus optional
/// `k=v` query pairs that must ALL be present on the request, so two
/// reads of one path (open cars vs closed cars) answer differently. The
/// first matching route wins; an unmatched GET is a 404. Every other
/// method is recorded as a write and answered with a created id.
pub(crate) async fn serve(answers: Vec<(&'static str, Value)>) -> Stub {
    let writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sent: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = writes.clone();
    let bodies = sent.clone();
    let answers = Arc::new(answers);
    let app = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let answers = answers.clone();
        let log = log.clone();
        let bodies = bodies.clone();
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
                // And a step PUT carrying metadata is refused as the
                // decided end state refuses it (e39a9d2a) — recorded, so
                // a test sees the attempt and not a silent success.
                let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if method == Method::PUT
                    && let Some((id, sid)) = step_path(uri.path())
                    && let Some(refused) = end_state_step_put(id, sid, &parsed)
                {
                    log.lock()
                        .unwrap()
                        .push(format!("{method} {} (409)", uri.path()));
                    return refused;
                }
                // So is a job PUT carrying metadata: the stale whole-row
                // write (9e7000f6), routed to the job merge door.
                if method == Method::PUT
                    && let Some(id) = job_path(uri.path())
                    && let Some(refused) = whole_job_row_put(id, &parsed)
                {
                    log.lock()
                        .unwrap()
                        .push(format!("{method} {} (409)", uri.path()));
                    return refused;
                }
                log.lock().unwrap().push(format!("{method} {}", uri.path()));
                bodies
                    .lock()
                    .unwrap()
                    .push((format!("{method} {}", uri.path()), parsed));
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
        sent,
    }
}

/// `/api/jobs/{id}/steps/{sid}` split into its two ids.
fn step_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/jobs/")?;
    let (id, sid) = rest.split_once("/steps/")?;
    (!id.is_empty() && !sid.is_empty() && !sid.contains('/')).then_some((id, sid))
}

/// `/api/jobs/{id}` — the job row itself, nothing under it.
fn job_path(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/api/jobs/")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
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
            // The real API's own constant, not a copy of its sentence.
            "hint": boss_jobs::corrections::TERMINAL_STEP_HINT,
        })),
    )
        .into_response()
}

/// The step PUT as the decided end state of design 93d2bddb has it
/// (backlog e39a9d2a): a body carrying `metadata` is REFUSED 409 and
/// routed to the step merge door, `PATCH .../steps/{sid}/metadata`.
/// `None` for a body the PUT accepts (status, assignee, no metadata).
///
/// WHY EVERY STUB IN THIS CRATE ANSWERS THIS WAY. The live rule (stage
/// 1) refuses only a body that OMITS a stored key, so a handler that
/// read the step and PUT every key back passes it — until a concurrent
/// writer adds a key between that read and the PUT, or until the rule
/// tightens to any metadata body, which is one block in the step PUT
/// once the writers have moved. A stub that accepted the read-merge-
/// write would pin the handlers to the form the tighten breaks; this
/// one pins them to the two writes that survive it. Every stub that
/// stands in for a step PUT calls this — one definition, not a copy per
/// handler.
pub(crate) fn end_state_step_put(id: &str, sid: &str, body: &Value) -> Option<Response> {
    body.get("metadata")?;
    Some(
        (
            StatusCode::CONFLICT,
            axum::Json(json!({
                "error": "a step PUT carries no metadata — write the keys through the step \
                          merge door, then PUT the status alone",
                "step_id": sid,
                "merge_door": format!("/api/jobs/{id}/steps/{sid}/metadata"),
            })),
        )
            .into_response(),
    )
}

/// The job PUT a handler must not send: a body carrying `metadata`,
/// which is the whole row sent back from a copy the handler READ. It is
/// REFUSED 409 and routed to the job merge door, `PATCH
/// /api/jobs/{id}/metadata` (top-level keys merge, `null` deletes).
/// `None` for a body with no metadata.
///
/// WHY (backlog 9e7000f6). The live job PUT accepts the row and
/// replaces what it is sent, so a handler that listed a job, changed
/// one key and PUT the listing's copy back reverted every write that
/// landed between its read and its PUT — `jobs.clear_waiting` did
/// exactly that, and its stub-free tests could not see it. This is the
/// job-level twin of [`end_state_step_put`]: a stub that accepted the
/// row would pin handlers to the stale-copy write, so every stub here
/// refuses it.
pub(crate) fn whole_job_row_put(id: &str, body: &Value) -> Option<Response> {
    body.get("metadata")?;
    Some(
        (
            StatusCode::CONFLICT,
            axum::Json(json!({
                "error": "a handler never PUTs a job row back — merge the keys it \
                          decided through the job merge door",
                "job_id": id,
                "merge_door": format!("/api/jobs/{id}/metadata"),
            })),
        )
            .into_response(),
    )
}

/// A stub's step writes as `(step, body)` wire entries — the merge door
/// recorded as `("<step>/metadata", fields)`, the step PUT as `(step,
/// body)` — folded back into ONE entry per step write, the shape a test
/// reads: a merge followed by that step's PUT is one `(step, {status,
/// metadata: fields})`, a merge with no PUT after it is an annotation,
/// `(step, {metadata: fields})`. `metadata` is what the write SENT,
/// never the step's stored keys. A stub serving [`end_state_step_put`]
/// has already refused any PUT carrying metadata, so a folded completion
/// is always the merge first and the status alone after it (e39a9d2a).
pub(crate) fn fold_step_writes(wire: &[(String, Value)]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < wire.len() {
        let (key, body) = &wire[i];
        match key.strip_suffix("/metadata") {
            Some(sid) => match wire.get(i + 1) {
                Some((next, put)) if next == sid => {
                    let mut completion = put.clone();
                    completion["metadata"] = body.clone();
                    out.push((sid.to_string(), completion));
                    i += 2;
                }
                _ => {
                    out.push((sid.to_string(), json!({ "metadata": body })));
                    i += 1;
                }
            },
            None => {
                out.push((key.clone(), body.clone()));
                i += 1;
            }
        }
    }
    out
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

    /// And as the decided end state refuses it (e39a9d2a): a step PUT
    /// carrying metadata is a 409 naming the merge door, while the
    /// merge door itself and a status-only PUT are accepted.
    #[tokio::test]
    async fn a_step_put_carrying_metadata_is_refused_409_naming_the_merge_door() {
        let stub = serve(vec![]).await;
        let client = reqwest::Client::new();
        let url = format!("{}/api/jobs/j1/steps/j1-build", stub.base);
        let refused = client
            .put(&url)
            .json(&json!({ "status": "completed", "metadata": { "evidence": "x" } }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 409);
        let why: Value = refused.json().await.unwrap();
        assert_eq!(why["merge_door"], "/api/jobs/j1/steps/j1-build/metadata");
        let merged = client
            .patch(format!("{url}/metadata"))
            .json(&json!({ "evidence": "x" }))
            .send()
            .await
            .unwrap();
        assert!(merged.status().is_success());
        let flipped = client
            .put(&url)
            .json(&json!({ "status": "completed" }))
            .send()
            .await
            .unwrap();
        assert!(flipped.status().is_success());
        assert_eq!(
            stub.writes(),
            vec![
                "PUT /api/jobs/j1/steps/j1-build (409)".to_string(),
                "PATCH /api/jobs/j1/steps/j1-build/metadata".to_string(),
                "PUT /api/jobs/j1/steps/j1-build".to_string(),
            ]
        );
    }

    /// And a job PUT carrying metadata — the whole row sent back from a
    /// read — is a 409 naming the job merge door (9e7000f6), while the
    /// merge door itself is accepted.
    #[tokio::test]
    async fn a_job_put_carrying_metadata_is_refused_409_naming_the_job_merge_door() {
        let stub = serve(vec![]).await;
        let client = reqwest::Client::new();
        let url = format!("{}/api/jobs/j1", stub.base);
        let refused = client
            .put(&url)
            .json(&json!({ "id": "j1", "status": "open", "metadata": { "waiting_on": "" } }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 409);
        let why: Value = refused.json().await.unwrap();
        assert_eq!(why["merge_door"], "/api/jobs/j1/metadata");
        let merged = client
            .patch(format!("{url}/metadata"))
            .json(&json!({ "waiting_on": "" }))
            .send()
            .await
            .unwrap();
        assert!(merged.status().is_success());
        assert_eq!(
            stub.writes(),
            vec![
                "PUT /api/jobs/j1 (409)".to_string(),
                "PATCH /api/jobs/j1/metadata".to_string(),
            ]
        );
    }
}
