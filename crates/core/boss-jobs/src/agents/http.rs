//! Axum routes for the agents registry: the roster's list and its one
//! write, the tenant batch (backlog f56155f0, 2026-09-17). Until this
//! door the registry's rows arrived by migration only, so the company's
//! own agent was declared nowhere the product read.
//!
//! **Reads admit `crate::trust::can_read`** — operator machinery and
//! the auditor tier, the recorded-probe reader (`boss-sor-read
//! /api/agents` is how the car for this surface is proved). **Writes
//! admit `crate::trust::is_trusted`** — operator tier or a trusted
//! internal sibling, the tier `boss tenant publish` signs with.
//!
//! `POST /api/agents/batch` takes a bare JSON array of `AgentInput` (the
//! classes and locations batch shape), validates every row with the
//! same `validate_agent` the TOML loader ran, refuses the whole batch
//! (422, deterministic) naming the row, and answers what it did —
//! including which registered rows it KEPT and how the declaration
//! differs from them, so a disagreement between the tenant's file and
//! the registry is read in the publish line, never in silence.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::CurrentUser;

use crate::trust::{can_read, is_trusted};

use super::port::{AgentsError, AgentsRegistry};
use super::types::{AgentInput, validate_agent};

pub struct AgentsApiState {
    pub registry: Arc<dyn AgentsRegistry>,
}

pub fn router(state: AgentsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/agents", get(list))
        .route("/api/agents/batch", post(publish))
        .with_state(shared)
}

fn err_response(e: AgentsError) -> Response {
    match e {
        AgentsError::Unpriced(m) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "default_model `{m}` is not a rate-card model — the registry refuses a default \
                 it cannot price (spell it as the card does, e.g. opus-5[1m], not claude-…)"
            ),
        )
            .into_response(),
        AgentsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list().await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

async fn publish(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<AgentInput>>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(why) = rows.iter().find_map(|a| validate_agent(a).err()) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    match state.registry.publish(&rows).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::agents::InMemoryAgents;

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    fn seed() -> Option<String> {
        Some(header(
            "automation:tenant-seed",
            "platform-admin",
            AccessTier::Operator,
        ))
    }

    fn probe_reader() -> Option<String> {
        Some(header(
            "audit-readonly",
            "audit-readonly",
            AccessTier::Auditor,
        ))
    }

    async fn send(
        app: Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(path);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = app
            .oneshot(req.body(body).expect("request builds"))
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn app(registry: &Arc<InMemoryAgents>) -> Router {
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
        })
    }

    fn batch() -> Value {
        json!([{
            "id": "agent-claude", "display_name": "Claude (engineering)",
            "default_model": "opus-5[1m]", "aliases": ["claude@algedonic.dev"]
        }])
    }

    /// The tenant batch lands, the roster lists it with its alias, a
    /// second publish inserts nothing, and a declaration that disagrees
    /// with a registered row is kept as registered and NAMED.
    #[tokio::test]
    async fn a_tenant_publishes_and_the_roster_lists_the_agent() {
        let registry = Arc::new(InMemoryAgents::new());
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 1);
        assert_eq!(out["kept"], json!([]));

        let (status, body) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["total"], 1);
        assert_eq!(listing["data"][0]["id"], "agent-claude");
        assert_eq!(listing["data"][0]["default_model"], "opus-5[1m]");
        assert_eq!(
            listing["data"][0]["aliases"],
            json!(["claude@algedonic.dev"])
        );

        let mut renamed = batch();
        renamed[0]["display_name"] = json!("Claude (renamed)");
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(renamed),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0, "a second publish inserts nothing");
        assert_eq!(out["kept"][0]["id"], "agent-claude");
        assert_eq!(out["kept"][0]["differs"], json!(["display_name"]));
        let rows = registry.list().await.unwrap();
        assert_eq!(
            rows[0].display_name, "Claude (engineering)",
            "kept as registered"
        );
    }

    /// A bad declaration refuses the WHOLE batch, 422, naming the row;
    /// nothing lands.
    #[tokio::test]
    async fn a_bad_declaration_refuses_the_batch_by_name() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut b = batch();
        b.as_array_mut().unwrap().push(json!({
            "id": "claude@algedonic.dev", "display_name": "x", "default_model": "opus-5"
        }));
        let (status, body) =
            send(app(&registry), "POST", "/api/agents/batch", Some(b), seed()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("claude@algedonic.dev"), "{body}");
        assert!(registry.list().await.unwrap().is_empty());
    }

    /// The probe reader and a header-less sibling read; a user-tier
    /// session writes nothing; the auditor reads and cannot write.
    #[tokio::test]
    async fn reads_admit_the_probe_reader_and_writes_are_operator_machinery() {
        let registry = Arc::new(InMemoryAgents::new());
        for (user, want) in [(probe_reader(), StatusCode::OK), (None, StatusCode::OK)] {
            let (status, _) = send(app(&registry), "GET", "/api/agents", None, user.clone()).await;
            assert_eq!(status, want, "{user:?}");
        }
        let david = Some(header("emp-david", "platform-admin", AccessTier::User));
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            david,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            probe_reader(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
