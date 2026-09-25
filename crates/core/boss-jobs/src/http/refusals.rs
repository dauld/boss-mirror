//! The middleware that records refused step writes, and the door to
//! read them back.
//!
//! WHY A LAYER AND NOT A CALL AT EACH `return`. `http/steps.rs` refuses
//! from roughly fifteen sites and gains more as the protocol tightens.
//! Recording at each one is a fact that lives fifteen times: the next
//! refusal added would silently not be counted, and the metric would
//! quietly under-report exactly where the protocol is newest and least
//! understood. One layer over the router cannot drift — a refusal that
//! reaches the client is recorded whether or not anyone remembered.
//!
//! The cost is that the layer sees an HTTP response rather than a typed
//! error, which is why `crate::refusals::classify` exists and is pure.

use super::*;

use axum::body::Body;
use axum::extract::{Query, Request};
use axum::middleware::Next;
use boss_policy_client::User;

use crate::refusals::{StepWriteRefusal, classify, is_refusal, is_step_write};

/// Cap on the refusal detail we keep. Long enough for every message the
/// handlers produce today, short enough that a pathological body cannot
/// turn a measurement table into a storage problem.
const DETAIL_LIMIT: usize = 2000;

/// The path carries the ids: `/api/jobs/{job}/steps/{step}/...`.
/// Parsed rather than threaded through the handlers, because the layer
/// runs after the handler has already returned.
fn ids_from_path(path: &str) -> (Option<uuid::Uuid>, Option<uuid::Uuid>) {
    let Some(rest) = path.strip_prefix("/api/jobs/") else {
        return (None, None);
    };
    let mut parts = rest.split('/');
    let job = parts.next().and_then(|s| uuid::Uuid::parse_str(s).ok());
    // `steps`, then the step id if this is a per-step route.
    let step = match parts.next() {
        Some("steps") => parts.next().and_then(|s| uuid::Uuid::parse_str(s).ok()),
        _ => None,
    };
    (job, step)
}

pub(super) async fn record_step_write_refusals<
    R: JobsRepository + 'static,
    B: EventBus + 'static,
>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    // Not a step write: hand it straight through, untouched. In
    // particular the response body is NOT buffered, so the read paths
    // and the SSE stream keep streaming.
    if !is_step_write(&method, &path) {
        return next.run(req).await;
    }

    let actor_id = req
        .headers()
        .get("x-boss-user")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str::<User>(s).ok())
        .map(|u| u.id)
        .unwrap_or_else(|| "anonymous".to_string());

    let res = next.run(req).await;
    let status = res.status().as_u16();
    if !is_refusal(status) {
        return res;
    }

    // Only refusals get buffered, and a refusal body is an error
    // string. `usize::MAX` is safe here for that reason, and the value
    // we KEEP is capped at DETAIL_LIMIT regardless.
    let (parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        // Reading the body failed: the caller still gets a response,
        // just without a recorded detail. Never let the side-channel
        // damage the answer.
        Err(e) => {
            tracing::warn!(error = %e, "could not read refusal body to record it");
            return Response::from_parts(parts, Body::empty());
        }
    };
    let detail: String = String::from_utf8_lossy(&bytes)
        .chars()
        .take(DETAIL_LIMIT)
        .collect();

    let (job_id, step_id) = ids_from_path(&path);
    let refusal = StepWriteRefusal {
        job_id,
        step_id,
        actor_id,
        method,
        path,
        status_code: status,
        error_class: classify(status, &detail),
        detail,
    };

    // Side-channel: a failure to RECORD must never turn a refusal the
    // caller can act on into a 500 it cannot.
    if let Err(e) = state.jobs.record_step_write_refusal(&refusal).await {
        tracing::warn!(error = %e, "could not record refused step write");
    }

    Response::from_parts(parts, Body::from(bytes))
}

#[derive(serde::Deserialize)]
pub(super) struct RefusalQuery {
    limit: Option<i64>,
}

/// The read door. Without it the table is a black hole and "let's try
/// it for a while and see how it goes" has nothing to look at.
///
/// A row carries the refused caller's id, the path and the refusal's
/// words about the packet, so it is read under the packet's scope
/// (backlog 046832d3): refused to a caller denied Read on job, and cut
/// to the rows on packets inside the caller's scope. A row whose id
/// never parsed names no packet, so only a caller who reads every
/// packet sees it.
pub(super) async fn list_step_write_refusals<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<RefusalQuery>,
) -> Response {
    let scope = match job_read_scope(&state, &user).await {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows = match state.jobs.step_write_refusals(limit).await {
        Ok(rows) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let packet = |r: &crate::refusals::RecordedRefusal| {
        r.refusal.job_id.map(boss_core::job::JobId::from_uuid)
    };
    let rows = if matches!(scope, boss_policy_client::Scope::All) {
        rows
    } else {
        let readable =
            match readable_job_ids(&state, &user, &scope, rows.iter().filter_map(packet)).await {
                Ok(ids) => ids,
                Err(refusal) => return refusal,
            };
        rows.into_iter()
            .filter(|r| packet(r).is_some_and(|id| readable.contains(&id)))
            .collect()
    };
    Json(serde_json::json!({ "total": rows.len(), "data": rows })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_come_off_the_path_including_the_sub_routes() {
        let job = uuid::Uuid::parse_str("9fa67fb9-ba93-4383-a46e-542983c3bc54").expect("job");
        let step = uuid::Uuid::parse_str("5d404e64-338e-4145-8ce0-82171a130b2c").expect("step");

        assert_eq!(
            ids_from_path("/api/jobs/9fa67fb9-ba93-4383-a46e-542983c3bc54/steps"),
            (Some(job), None)
        );
        assert_eq!(
            ids_from_path(
                "/api/jobs/9fa67fb9-ba93-4383-a46e-542983c3bc54/steps/5d404e64-338e-4145-8ce0-82171a130b2c"
            ),
            (Some(job), Some(step))
        );
        assert_eq!(
            ids_from_path(
                "/api/jobs/9fa67fb9-ba93-4383-a46e-542983c3bc54/steps/5d404e64-338e-4145-8ce0-82171a130b2c/claim"
            ),
            (Some(job), Some(step))
        );
    }

    #[test]
    fn an_unparseable_id_still_yields_a_recordable_refusal() {
        // The refusal whose CAUSE is the bad id must still be recorded —
        // that is why both columns are nullable. Returning None here is
        // the correct answer, not a failure to parse.
        assert_eq!(ids_from_path("/api/jobs/not-a-uuid/steps"), (None, None));
    }
}
