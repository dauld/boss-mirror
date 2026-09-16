//! Axum routes for the cadence registry.
//!
//! This surface exists so the train conductor can run OUTSIDE the
//! cluster without a database connection. It reaches the same
//! `boss-jobs-internal` door it already uses for the dock probe, so
//! the conductor needs exactly one address to do its whole job.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::CurrentUser;

use crate::trust::{can_read, is_trusted};

use super::port::{CadenceError, CadenceRepository};
use super::types::{ClaimResult, FiringOutcome, NewFiring};

pub struct CadenceApiState {
    pub repo: Arc<dyn CadenceRepository>,
}

// Who this door admits lives in `crate::trust`, with the four other
// operator doors (839335b7): reads admit the auditor tier the
// recorded-probe reader carries; writes are operator machinery only.

pub fn router(state: CadenceApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/cadence/rules", get(list_rules))
        .route("/api/cadence/rules/{name}/last-firing", get(last_firing))
        .route("/api/cadence/firings", post(claim_firing))
        .route("/api/cadence/firings/{id}/outcome", post(record_outcome))
        .with_state(shared)
}

fn err_response(e: CadenceError) -> Response {
    match e {
        CadenceError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CadenceError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list_rules(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.active_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => err_response(e),
    }
}

async fn last_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.last_firing(&name).await {
        // `null` for "never fired" — the conductor treats it as
        // "every window is a candidate", so a 404 would be wrong.
        Ok(f) => Json(f).into_response(),
        Err(e) => err_response(e),
    }
}

async fn claim_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewFiring>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.firing_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "firing_id is required").into_response();
    }
    match state.repo.claim_firing(&body).await {
        // A lost claim is a normal, expected outcome — not an error.
        // It is reported as 200 + `{"claimed": false}` rather than 409
        // so the conductor's retry policy (which retries 5xx and
        // treats 4xx as fatal) never sees a race as a failure.
        Ok(claimed) => Json(ClaimResult { claimed }).into_response(),
        Err(e) => err_response(e),
    }
}

async fn record_outcome(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<FiringOutcome>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .repo
        .record_outcome(&id, body.rc, body.runtime_secs)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadence::InMemoryCadence;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use tower::ServiceExt;

    fn app() -> Router {
        router(CadenceApiState {
            repo: Arc::new(InMemoryCadence::default()),
        })
    }

    fn header(role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: "x".into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("platform".into()),
        })
        .unwrap()
    }

    async fn status(req: Request<Body>) -> StatusCode {
        app().oneshot(req).await.unwrap().status()
    }

    /// Measured 2026-09-16 (839335b7): `/api/cadence/rules` and
    /// `/api/cadence/rules/{name}/last-firing` answered the
    /// recorded-probe reader (`audit-readonly` at auditor) 403 on the
    /// live system of record, so no car about a cadence could be
    /// proved. The reads admit the auditor tier; the two writes stay
    /// operator machinery.
    #[tokio::test]
    async fn the_probe_reader_reads_the_cadence_and_cannot_claim_a_firing() {
        let reader = header("audit-readonly", AccessTier::Auditor);
        for path in ["/api/cadence/rules", "/api/cadence/rules/board/last-firing"] {
            let st = status(
                Request::get(path)
                    .header("x-boss-user", reader.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{path}");
        }
        let st = status(
            Request::post("/api/cadence/firings")
                .header("x-boss-user", reader.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"firing_id":"f1","rule_name":"board","verb":"board","basis":"queue-depth","fired_at":"2026-09-16T00:00:00Z"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "a read-only reader claims nothing"
        );
        // The gateway's guest session (audit-readonly at USER tier)
        // stays refused on the reads too.
        let st = status(
            Request::get("/api/cadence/rules")
                .header("x-boss-user", header("audit-readonly", AccessTier::User))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }
}
