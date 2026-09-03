//! Axum routes for the credentials registry.
//!
//! `GET /api/credentials` and `GET /api/credentials/{id}` — the door
//! that turns a scope question into a lookup. The one rule from the
//! module doc holds hardest here: **a row carries locations, never
//! contents** — no secret value can pass through this API because no
//! secret value exists anywhere behind it. The registry knows
//! *about* credentials; possession stays in Secrets.
//!
//! Read access mirrors `delivery::http`: operator machinery. Trusted
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
//!
//! `POST /api/credentials/{id}/rotation/{phase}` is the rotation
//! path's write — the census-door precedent. The broker
//! (`credential.rotate.forgejo`, a dispatcher handler) owns no
//! database, so the service that owns the registry provides the one
//! write: validate trust and shape, record exactly one
//! `credential.<phase>` event, and on the `installed` phase stamp
//! the row's `rotated_at` in the same transaction. DELIBERATELY A
//! DUMB DOOR: the evidence payload is owned by the handler that
//! observed the effects — identifiers (token name/id, Secret path,
//! value length) and observed effects only, NEVER a value — and a
//! door that second-guesses its instrument is a second instrument.
//! Operator tier only, and no guest trust here: this is a WRITE, and
//! the headerless-internal-caller allowance exists for reads.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::{AccessTier, CurrentUser, User};

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::RotationPhase;

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
        .route(
            "/api/credentials/{id}/rotation/{phase}",
            post(record_rotation_phase),
        )
        .with_state(shared)
}

fn err_response(e: CredentialsError) -> Response {
    match e {
        CredentialsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
        CredentialsError::UnknownCredential(id) => (
            StatusCode::NOT_FOUND,
            format!("no credential {id:?} in the registry"),
        )
            .into_response(),
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

/// The rotation door. Trust (operator tier — the dispatcher stamps
/// `access_tier: operator` on every rule-as-actor header), phase (one
/// of the four the registry's `RotationPhase` names), shape (a JSON
/// object), then record verbatim: the event's `credential_id` is
/// injected from the path so every rotation event self-identifies,
/// and everything else in the payload is the instrument's own report.
async fn record_rotation_phase(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    Path((id, phase)): Path<(String, String)>,
    Json(evidence): Json<serde_json::Value>,
) -> Response {
    if user.access_tier != AccessTier::Operator {
        return (
            StatusCode::FORBIDDEN,
            "the rotation door is operator machinery — operator tier required",
        )
            .into_response();
    }
    let Some(phase) = RotationPhase::parse(&phase) else {
        let valid: Vec<&str> = RotationPhase::ALL.iter().map(|p| p.as_str()).collect();
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown rotation phase {phase:?}; valid phases: {}",
                valid.join(", ")
            ),
        )
            .into_response();
    };
    let Some(mut payload) = evidence.as_object().cloned() else {
        return (
            StatusCode::BAD_REQUEST,
            "rotation evidence must be a JSON object",
        )
            .into_response();
    };
    payload.insert(
        "credential_id".to_string(),
        serde_json::Value::String(id.clone()),
    );

    // Same envelope construction as the census door: the actor rides
    // as `_actor` exactly as EventStamp injects it, the stamp is
    // wall-clock (sim time is retired from the record), and the
    // source is `jobs` — this service is the one recording.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);

    match state
        .registry
        .record_rotation(&id, phase, serde_json::Value::Object(payload), &stamp)
        .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "recorded": true,
                "kind": phase.event_kind(),
            })),
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

    // ----- the rotation door -----

    fn rotation_app(rows: Vec<CredentialRow>) -> (Router, Arc<InMemoryCredentials>) {
        let registry = Arc::new(InMemoryCredentials::new(rows));
        let app = router(CredentialsApiState {
            registry: registry.clone(),
        });
        (app, registry)
    }

    fn post_phase(id: &str, phase: &str, body: serde_json::Value, header: &str) -> Request<Body> {
        Request::post(format!("/api/credentials/{id}/rotation/{phase}"))
            .header("x-boss-user", header)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_recorded_phase_lands_as_an_event_naming_the_credential() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!({
                    "job_id": "7ee101aa-3267-4745-8096-06d07df7e144",
                    "token_name": "boss-dev-forge-token-7ee101aa",
                    "token_id": 101,
                }),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        assert_eq!(body["kind"], "credential.minted");

        let events = registry.recorded_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "credential.minted");
        assert_eq!(events[0].source, "jobs");
        assert_eq!(
            events[0].payload["credential_id"], "boss-dev-forge-token",
            "the door injects the path id so every rotation event self-identifies"
        );
        assert_eq!(
            events[0].payload["token_name"],
            "boss-dev-forge-token-7ee101aa"
        );
    }

    #[tokio::test]
    async fn the_install_phase_stamps_rotated_at_and_the_others_do_not() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        for phase in ["minted", "verified", "revoked"] {
            let resp = app
                .clone()
                .oneshot(post_phase(
                    "boss-dev-forge-token",
                    phase,
                    json!({}),
                    &operator_header(),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::ACCEPTED);
        }
        assert!(
            registry
                .get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_none(),
            "rotated_at records when the VALUE last changed — the install moment"
        );
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "installed",
                json!({ "value_length": 40 }),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(
            registry
                .get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_unknown_phase_is_a_400_naming_the_valid_ones() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "misplaced",
                json!({}),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        for phase in ["minted", "installed", "verified", "revoked"] {
            assert!(
                text.contains(phase),
                "400 must name the valid phases: {text}"
            );
        }
        assert!(registry.recorded_events().is_empty());
    }

    #[tokio::test]
    async fn a_rotation_against_an_unknown_credential_is_a_404_naming_it() {
        let (app, registry) = rotation_app(vec![]);
        let resp = app
            .oneshot(post_phase(
                "ghost-credential",
                "minted",
                json!({}),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an absent row is a finding — a rotation event may not detach from its row"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("ghost-credential"));
        assert!(registry.recorded_events().is_empty());
    }

    #[tokio::test]
    async fn non_object_evidence_is_refused() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!("a bare string"),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(registry.recorded_events().is_empty());
    }

    #[tokio::test]
    async fn the_rotation_door_refuses_below_operator_tier_including_guest() {
        // The READ doors trust a headerless internal caller (guest);
        // the WRITE door must not — that allowance exists for the
        // forge-host audit's reads, and a write with no presented
        // identity would record an unattributable rotation.
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let user_resp = app
            .clone()
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!({}),
                &user_tier_header(),
            ))
            .await
            .unwrap();
        assert_eq!(user_resp.status(), StatusCode::FORBIDDEN);
        let guest_resp = app
            .oneshot(
                Request::post("/api/credentials/boss-dev-forge-token/rotation/minted")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(guest_resp.status(), StatusCode::FORBIDDEN);
        assert!(registry.recorded_events().is_empty());
    }
}
