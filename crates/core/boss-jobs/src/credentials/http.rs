//! Axum routes for the credentials registry.
//!
//! `GET /api/credentials` and `GET /api/credentials/{id}` — the door
//! that turns a scope question into a lookup. Read-only, and the one
//! rule from the module doc holds hardest here: **a row carries
//! locations, never contents** — no secret value can pass through
//! this API because no secret value exists anywhere behind it. The
//! registry knows *about* credentials; possession stays in Secrets.
//!
//! Access mirrors `delivery::http`: operator machinery. Trusted
//! internal callers (the extractor defaults to `role=guest` when no
//! `x-boss-user` header arrived — the forge-host audit reads through
//! this path) and operator-tier callers (`boss credential list`, the
//! session actor via boss-api). The gateway always injects the header
//! for external requests, so a browser session never lands here as
//! guest.
//!
//! An unknown id is a 404 that names it — unlike the delivery door,
//! there is no fallback for a missing credential row; an absent row
//! is a finding, and the caller should hear so unambiguously.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use boss_policy_client::{AccessTier, CurrentUser, User};

use super::port::{CredentialsError, CredentialsRegistry};

pub struct CredentialsApiState {
    pub registry: Arc<dyn CredentialsRegistry>,
}

fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

pub fn router(state: CredentialsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/credentials", get(list))
        .route("/api/credentials/{id}", get(get_one))
        .with_state(shared)
}

fn err_response(e: CredentialsError) -> Response {
    match e {
        CredentialsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err_response(e),
    }
}

async fn get_one(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.get(&id).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no credential {id:?} in the registry"),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialRow, InMemoryCredentials};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn row(id: &str) -> CredentialRow {
        CredentialRow {
            id: id.into(),
            kind: "forgejo-access-token".into(),
            issuer: "forgejo (10.20.0.15)".into(),
            principal: "user david".into(),
            scopes: json!(["write:repository"]),
            storage_location: "k8s Secret boss-dev/boss-dev-forge-token key token".into(),
            consumers: json!([{ "kind": "secret-mount", "location": "/etc/boss-train/forge.token" }]),
            rotation_policy: "on-demand".into(),
            rotated_at: None,
            notes: "the row knows where the value lives, never what it is".into(),
        }
    }

    fn app(rows: Vec<CredentialRow>) -> Router {
        router(CredentialsApiState {
            registry: Arc::new(InMemoryCredentials::new(rows)),
        })
    }

    fn operator_header() -> String {
        serde_json::to_string(&User {
            id: "emp-ops".into(),
            role: "platform-admin".into(),
            access_tier: AccessTier::Operator,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("it".into()),
        })
        .unwrap()
    }

    fn user_tier_header() -> String {
        serde_json::to_string(&User {
            id: "emp-someone".into(),
            role: "member".into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("sales".into()),
        })
        .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn the_list_answers_every_row_for_an_operator() {
        let app = app(vec![row("boss-dev-forge-token"), row("boss-machine-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let ids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["boss-dev-forge-token", "boss-machine-token"]);
    }

    #[tokio::test]
    async fn a_headerless_internal_caller_is_trusted() {
        // The forge-host audit reads the registry over the internal
        // address with no x-boss-user header; the extractor defaults
        // to role=guest, which this door trusts like delivery's does.
        let app = app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_user_tier_caller_is_refused() {
        let app = app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials")
                    .header("x-boss-user", user_tier_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_by_id_answers_the_row() {
        let app = app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials/boss-dev-forge-token")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["scopes"], json!(["write:repository"]));
        assert_eq!(
            body["storage_location"], "k8s Secret boss-dev/boss-dev-forge-token key token",
            "the row says where the value lives, never what it is"
        );
    }

    #[tokio::test]
    async fn an_unknown_id_is_a_404_naming_it() {
        let app = app(vec![]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials/no-such-credential")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an absent credential row is a finding, not a fallback"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("no-such-credential"));
    }
}
