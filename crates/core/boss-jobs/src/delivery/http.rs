//! Axum routes for the delivery-policy registry.
//!
//! Same door, same reason as `cadence::http`: the train conductor runs
//! OUTSIDE the cluster without a database connection, and reaches the
//! `boss-jobs-internal` address it already uses for every other call.
//! One address does its whole job.
//!
//! Both routes answer `null` rather than 404 for "no such policy". The
//! conductor's retry policy treats 4xx as fatal and 5xx as a blip, so a
//! missing row must not arrive as either: it is an ANSWER, and the
//! answer means "fall back to the compiled values and say so".

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use boss_policy_client::CurrentUser;

use crate::trust::can_read;

use super::port::{DeliveryPolicyError, DeliveryPolicyRepository};

pub struct DeliveryPolicyApiState {
    pub repo: Arc<dyn DeliveryPolicyRepository>,
}

// Both routes are reads, so both admit what `crate::trust::can_read`
// admits — the operator machinery that always read here, and the
// auditor tier the recorded-probe reader carries (839335b7).

pub fn router(state: DeliveryPolicyApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/delivery/policy/{name}", get(active_policy))
        .route(
            "/api/delivery/policy/{name}/versions/{version}",
            get(policy_version),
        )
        .with_state(shared)
}

fn err_response(e: DeliveryPolicyError) -> Response {
    match e {
        DeliveryPolicyError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        DeliveryPolicyError::Conflict(m) => (StatusCode::CONFLICT, m).into_response(),
        DeliveryPolicyError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn active_policy(
    State(state): State<Arc<DeliveryPolicyApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.active_policy(&name).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => err_response(e),
    }
}

async fn policy_version(
    State(state): State<Arc<DeliveryPolicyApiState>>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.policy_version(&name, version).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::InMemoryDeliveryPolicy;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use tower::ServiceExt;

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

    async fn status(path: &str, user: String) -> StatusCode {
        router(DeliveryPolicyApiState {
            repo: Arc::new(InMemoryDeliveryPolicy::default()),
        })
        .oneshot(
            Request::get(path)
                .header("x-boss-user", user)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    /// Measured 2026-09-16 (839335b7): `/api/delivery/policy/default`
    /// answered the recorded-probe reader 403 on the live system of
    /// record. Both routes are reads; both admit the auditor tier, and
    /// neither admits the gateway's guest session.
    #[tokio::test]
    async fn the_probe_reader_reads_the_policy_and_the_guest_session_does_not() {
        let reader = header("audit-readonly", AccessTier::Auditor);
        for path in [
            "/api/delivery/policy/default",
            "/api/delivery/policy/default/versions/1",
        ] {
            let st = status(path, reader.clone()).await;
            assert_ne!(st, StatusCode::FORBIDDEN, "{path}: {st}");
        }
        let st = status(
            "/api/delivery/policy/default",
            header("audit-readonly", AccessTier::User),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }
}
