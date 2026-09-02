//! `GET /api/jobs/queue-age` — the queue-age lens (packet 2a0b034e):
//! how long has every outstanding obligation waited?
//!
//! WHY IT EXISTS. 70 live steps sat on open IT packets the day this
//! was filed, and the only age the system could answer with was the
//! PACKET's `opened_on` — date-only, job-level. Nine design decisions
//! sat in the wrong actor's queue with nothing to flag them, because
//! nothing recorded when a step became an obligation. This surface
//! answers per STEP, from the projection's `became_ready_at` stamp
//! (with the `updated_at` lower bound as labelled fallback — see
//! [`crate::port::QueueAgeRow`] for the honest semantics of each).
//!
//! A LENS, NOT A FIELD: read-only, its own row shape, `Job` and
//! `Step` untouched — `terminal-report` is the precedent. `waiting`
//! is derived HERE, in one place, from the same clock the sim
//! surfaces use, so no adapter grows its own arithmetic for it.

use super::*;

pub(super) async fn list_queue_age<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // The job read gate, scoped — same shape as the station surfaces:
    // an unreadable caller gets an empty lens, not a 403, and a
    // partially-scoped one sees exactly their slice of the network.
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    let scope = job_scope_from_predicate(&user, &predicate);
    let now = boss_clock_client::now_from(&state.clock).await;
    match state.jobs.queue_age(&scope).await {
        Ok(rows) => {
            let total = rows.len();
            let data: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|r| {
                    // `.max(0)`: a stamp written between the clock
                    // read and the query must not render as a
                    // negative wait.
                    let waiting_seconds = (now - r.since).num_seconds().max(0);
                    serde_json::json!({
                        "job_id": r.job_id,
                        "job_kind": r.job_kind,
                        "job_title": r.job_title,
                        "step_id": r.step_id,
                        "spec_slug": r.spec_slug,
                        "step_title": r.step_title,
                        "status": r.status,
                        "assignee_id": r.assignee_id,
                        "simulated": r.simulated,
                        "since": r.since,
                        "exact": r.exact,
                        "waiting_seconds": waiting_seconds,
                        "waiting_days": waiting_seconds as f64 / 86_400.0,
                    })
                })
                .collect();
            Json(serde_json::json!({ "data": data, "total": total, "now": now })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
