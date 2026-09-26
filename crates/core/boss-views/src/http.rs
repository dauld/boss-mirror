//! HTTP surface for Views.
//!
//! `GET/POST /api/views`, `GET/PUT/DELETE /api/views/{id}`, and
//! `GET /api/views/{id}/results` — the definition CRUD plus the one
//! endpoint that runs it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use boss_policy_client::CurrentUser;
use serde::Deserialize;

use crate::error::ViewsError;
use crate::port::{ViewResolver, ViewsRepo};
use crate::types::ViewInput;

/// Rows returned by `/results` when the caller does not say.
const DEFAULT_LIMIT: usize = 100;
/// Ceiling on the caller-supplied limit. The scan itself is bounded
/// separately (`query::SCAN_CEILING`); this bounds the response.
const MAX_LIMIT: usize = 500;

#[derive(Clone)]
pub struct ViewsApiState {
    pub repo: Arc<dyn ViewsRepo>,
    pub resolver: Arc<dyn ViewResolver>,
    /// The Operating System map. Optional so a deployment without a
    /// Postgres-backed views service still serves the rest of the
    /// surface rather than failing to construct.
    pub os_map: Option<Arc<dyn crate::os_map::OsMapRepo>>,
    /// The team's flow. Optional for the same reason as `os_map`.
    pub flow: Option<Arc<dyn crate::flow::FlowRepo>>,
    /// The per-kind fleet. Optional for the same reason as `os_map`.
    pub fleet: Option<Arc<dyn crate::fleet::FleetRepo>>,
    /// Per-stage wall-clock durations. Optional like the others.
    pub stages: Option<Arc<dyn crate::stages::StageDurationsRepo>>,
}

#[derive(Deserialize)]
pub struct OsMapQuery {
    /// How many recent step completions to build the map from.
    /// Bounded by recency rather than time because `occurred_at` runs
    /// on the sim clock, which moves at warp — "the last N handoffs"
    /// means the same thing at any speed.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /api/views/os-map` — the executor network.
///
/// A read of who is moving work and where it goes, per
/// `docs/architecture-decisions.md`. Cheap enough to poll,
/// which is what makes it a live instrument rather than a snapshot.
async fn os_map(State(state): State<Arc<ViewsApiState>>, Query(q): Query<OsMapQuery>) -> Response {
    let Some(repo) = state.os_map.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "os-map needs a postgres-backed views service",
        )
            .into_response();
    };
    // Clamp rather than reject: a caller asking for everything gets a
    // large map, not an error, and the ceiling keeps one request from
    // scanning the whole log.
    let limit = q
        .limit
        .unwrap_or(crate::os_map::DEFAULT_LIMIT)
        .clamp(1, 100_000);
    match repo.os_map(limit).await {
        Ok(map) => Json(map).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Roles whose Workflows count as this team's work. Defaults to the
/// platform operator — the role the IT Workflows declare as owner.
/// Overridable so the same surface serves another team without a
/// second endpoint.
fn default_owner_roles() -> Vec<String> {
    vec![boss_core::roles::PLATFORM_ADMIN_ROLE.to_string()]
}

#[derive(Deserialize)]
pub struct FlowQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// Comma-separated owner roles.
    #[serde(default)]
    pub owner_roles: Option<String>,
}

/// `GET /api/views/flow` — what the team is getting through.
///
/// Wall-clock, unlike every other view: see `crate::flow` for why the
/// projection's timestamps cannot answer this question.
async fn flow(State(state): State<Arc<ViewsApiState>>, Query(q): Query<FlowQuery>) -> Response {
    let Some(repo) = state.flow.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "flow needs a postgres-backed views service",
        )
            .into_response();
    };
    let roles: Vec<String> = q
        .owner_roles
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(default_owner_roles);
    let limit = q
        .limit
        .unwrap_or(crate::flow::DEFAULT_LIMIT)
        .clamp(1, 20_000);
    match repo.flow(&roles, limit).await {
        Ok(f) => Json(f).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/views/fleet/{kind}` — every in-flight Job of one
/// Workflow kind, as per-step depth over the Workflow's shape.
///
/// Wall-clock for the same reason as `flow`; O(work-in-flight) by
/// construction — see `crate::fleet`.
async fn fleet(State(state): State<Arc<ViewsApiState>>, Path(kind): Path<String>) -> Response {
    let Some(repo) = state.fleet.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "fleet needs a postgres-backed views service",
        )
            .into_response();
    };
    match repo.fleet(&kind).await {
        Ok(f) => Json(f).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct StagesQuery {
    #[serde(default)]
    pub days: Option<i64>,
}

/// `GET /api/views/stage-durations/{kind}` — per-hop wall-clock
/// latency for one Workflow kind (see `crate::stages`).
async fn stage_durations(
    State(state): State<Arc<ViewsApiState>>,
    Path(kind): Path<String>,
    Query(q): Query<StagesQuery>,
) -> Response {
    let Some(repo) = state.stages.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "stage durations need a postgres-backed views service",
        )
            .into_response();
    };
    let days = q.days.unwrap_or(7).clamp(1, 90);
    match repo.stage_durations(&kind, days).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct RunsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /api/views/stage-runs/{kind}` — the last N Jobs of the kind
/// with per-step durations, newest first (the per-run rows behind the
/// stage-durations aggregate; backlog `a5096c8f`).
async fn stage_runs(
    State(state): State<Arc<ViewsApiState>>,
    Path(kind): Path<String>,
    Query(q): Query<RunsQuery>,
) -> Response {
    let Some(repo) = state.stages.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "stage runs need a postgres-backed views service",
        )
            .into_response();
    };
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    match repo.stage_runs(&kind, limit).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ResultsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn router(state: ViewsApiState) -> Router {
    Router::new()
        .route("/api/views/health", get(health))
        .route("/api/views", get(list_views).post(create_view))
        .route(
            "/api/views/{id}",
            get(get_view).put(replace_view).delete(delete_view),
        )
        .route("/api/views/{id}/results", get(view_results))
        .route("/api/views/os-map", get(os_map))
        .route("/api/views/flow", get(flow))
        .route("/api/views/fleet/{kind}", get(fleet))
        .route("/api/views/stage-durations/{kind}", get(stage_durations))
        .route("/api/views/stage-runs/{kind}", get(stage_runs))
        .with_state(Arc::new(state))
}

async fn health() -> Response {
    Json(serde_json::json!({ "status": "ok", "service": "views" })).into_response()
}

fn err_to_response(e: ViewsError) -> Response {
    match e {
        ViewsError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        // A filter that does not parse is the caller's text, not a
        // server fault — 422 so the authoring surface can show it
        // against the field.
        ViewsError::InvalidFilter(s) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid filter: {s}"),
        )
            .into_response(),
        ViewsError::Invalid(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        ViewsError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        ViewsError::Policy(e) => {
            tracing::warn!(error = %e, "views: caller's policy scope could not be decided");
            e.into_response()
        }
    }
}

async fn list_views(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // Identity comes from the request, never a `viewer_id` query
    // param — that param let any caller list any user's private Views
    // by naming them.
    match state.repo.list_for_viewer(&user.id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn get_view(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    match state.repo.get_for_viewer(&id, &user.id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn create_view(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
    Json(input): Json<ViewInput>,
) -> Response {
    // Owner is the authenticated caller. It used to be a field on the
    // body, which meant anyone could create a View attributed to
    // anyone.
    match state.repo.create(&user.id, &input).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn replace_view(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(input): Json<ViewInput>,
) -> Response {
    match state.repo.replace(&id, &user.id, &input).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn delete_view(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    match state.repo.delete(&id, &user.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_to_response(e),
    }
}

async fn view_results(
    State(state): State<Arc<ViewsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<ResultsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let view = match state.repo.get_for_viewer(&id, &user.id).await {
        Ok(v) => v,
        Err(e) => return err_to_response(e),
    };
    // The CALLER is scoped, not the View's author.
    match state.resolver.resolve(&view, &user, limit).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => err_to_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryViewsRepo;
    use crate::types::{View, ViewResults, Visibility};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::User;
    use chrono::DateTime;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Resolver stub — HTTP tests care about wiring, identity and
    /// status codes, not about what the projections contain. It echoes
    /// back the caller it was handed so the tests can assert that the
    /// CALLER, not the View's author, reaches the resolver.
    struct StubResolver;

    #[async_trait]
    impl ViewResolver for StubResolver {
        async fn resolve(
            &self,
            view: &View,
            user: &User,
            limit: usize,
        ) -> Result<ViewResults, ViewsError> {
            Ok(ViewResults {
                view_id: view.id.clone(),
                source: view.source,
                layout: view.layout,
                rows: vec![serde_json::json!({
                    "limit_seen": limit,
                    "scoped_for": user.id.clone(),
                })],
                matched: 1,
                pushed_down: 0,
                truncated: false,
            })
        }
    }

    fn app() -> Router {
        router(ViewsApiState {
            // These tests exercise the View surface; the map has its
            // own coverage and needs Postgres.
            os_map: None,
            flow: None,
            fleet: None,
            stages: None,
            repo: Arc::new(InMemoryViewsRepo::new(
                DateTime::from_timestamp(1_700_000_000, 0).expect("valid ts"),
            )),
            resolver: Arc::new(StubResolver),
        })
    }

    /// The header the gateway sets and `CurrentUser` parses.
    fn as_user(id: &str) -> String {
        serde_json::json!({
            "id": id,
            "role": "operator",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string()
    }

    fn body(title: &str, filter: &str, visibility: &str) -> String {
        serde_json::json!({
            "title": title,
            "source": "jobs",
            "filter": filter,
            "columns": [],
            "layout": "table",
            "visibility": visibility,
        })
        .to_string()
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        user: &str,
        b: Option<String>,
    ) -> (StatusCode, String) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-boss-user", as_user(user))
            .header("content-type", "application/json")
            .body(b.map(Body::from).unwrap_or_else(Body::empty))
            .expect("request builds");
        let resp = app.clone().oneshot(req).await.expect("router responds");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn create_as(app: &Router, user: &str, title: &str, visibility: &str) -> View {
        let (status, b) = send(
            app,
            "POST",
            "/api/views",
            user,
            Some(body(title, "", visibility)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {b}");
        serde_json::from_str(&b).expect("a view")
    }

    #[tokio::test]
    async fn create_then_list_and_run() {
        let app = app();
        let made = create_as(&app, "alice", "Open jobs", "private").await;

        let (status, _) = send(&app, "GET", "/api/views", "alice", None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = send(
            &app,
            "GET",
            &format!("/api/views/{}/results", made.id),
            "alice",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_owner_is_the_caller_not_a_field_in_the_body() {
        // The body cannot name an owner any more; even if a stale
        // client sends one it is ignored, because the type has no
        // such field.
        let app = app();
        let (status, b) = send(
            &app,
            "POST",
            "/api/views",
            "alice",
            Some(
                serde_json::json!({
                    "owner_id": "bob",
                    "title": "Spoof attempt",
                    "source": "jobs",
                    "filter": "",
                    "columns": [],
                    "layout": "table",
                    "visibility": "private",
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let made: View = serde_json::from_str(&b).expect("a view");
        assert_eq!(made.owner_id, "alice", "owner must come from the caller");
    }

    #[tokio::test]
    async fn a_request_with_no_identity_is_nobody_not_everybody() {
        // `CurrentUser` deliberately does not reject a missing
        // x-boss-user: it yields an anonymous guest, and the gateway
        // is the auth boundary. That is fine ONLY because anonymous
        // then owns nothing and is shown nothing private — which is
        // the property worth pinning, since the previous version let
        // an unauthenticated caller read and delete anyone's Views.
        let app = app();
        create_as(&app, "alice", "Alice private", "private").await;
        let made = create_as(&app, "alice", "Alice shared", "shared").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/views")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let list: Vec<View> = serde_json::from_slice(&bytes).expect("a list");
        assert_eq!(
            list.len(),
            1,
            "an anonymous caller must see only shared Views"
        );
        assert_eq!(list[0].id, made.id);
    }

    #[tokio::test]
    async fn an_anonymous_create_is_attributed_to_anonymous() {
        // Not to a real user, and not to whatever the body claimed.
        let app = app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/views")
                    .header("content-type", "application/json")
                    .body(Body::from(body("Anon", "", "private")))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let made: View = serde_json::from_slice(&bytes).expect("a view");
        assert_eq!(made.owner_id, "anonymous");
    }

    #[tokio::test]
    async fn a_stranger_cannot_read_a_private_view() {
        let app = app();
        let made = create_as(&app, "alice", "Alice private", "private").await;

        let (status, _) = send(&app, "GET", &format!("/api/views/{}", made.id), "bob", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(
            &app,
            "GET",
            &format!("/api/views/{}", made.id),
            "alice",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_stranger_cannot_delete_a_private_view() {
        // Reproduces the verified defect: this returned 204 and
        // destroyed the row.
        let app = app();
        let made = create_as(&app, "alice", "Alice private", "private").await;

        let (status, _) = send(
            &app,
            "DELETE",
            &format!("/api/views/{}", made.id),
            "bob",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(
            &app,
            "GET",
            &format!("/api/views/{}", made.id),
            "alice",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "alice's view must survive");
    }

    #[tokio::test]
    async fn listing_shows_only_your_own_plus_shared() {
        let app = app();
        create_as(&app, "alice", "Alice private", "private").await;
        create_as(&app, "alice", "Alice shared", "shared").await;

        let (_, b) = send(&app, "GET", "/api/views", "bob", None).await;
        let list: Vec<View> = serde_json::from_str(&b).expect("a list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Alice shared");
        assert_eq!(list[0].visibility, Visibility::Shared);
    }

    #[tokio::test]
    async fn a_shared_view_runs_scoped_to_whoever_ran_it() {
        // Not to its author. A shared View handed to a narrower role
        // must show that role their own rows.
        let app = app();
        let made = create_as(&app, "alice", "Ours", "shared").await;

        let (status, b) = send(
            &app,
            "GET",
            &format!("/api/views/{}/results", made.id),
            "bob",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let results: ViewResults = serde_json::from_str(&b).expect("results");
        assert_eq!(results.rows[0]["scoped_for"], serde_json::json!("bob"));
    }

    #[tokio::test]
    async fn a_malformed_filter_is_422_not_500() {
        let app = app();
        let (status, b) = send(
            &app,
            "POST",
            "/api/views",
            "alice",
            Some(body("Broken", "status =", "private")),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(b.contains("invalid filter"), "body was: {b}");
    }

    #[tokio::test]
    async fn results_clamps_an_absurd_limit_instead_of_honouring_it() {
        let app = app();
        let made = create_as(&app, "alice", "Everything", "private").await;
        let (_, b) = send(
            &app,
            "GET",
            &format!("/api/views/{}/results?limit=99999", made.id),
            "alice",
            None,
        )
        .await;
        let results: ViewResults = serde_json::from_str(&b).expect("results");
        assert_eq!(results.rows[0]["limit_seen"], serde_json::json!(MAX_LIMIT));
    }

    #[tokio::test]
    async fn running_a_missing_view_is_404() {
        let app = app();
        let (status, _) = send(&app, "GET", "/api/views/view-nope/results", "alice", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
