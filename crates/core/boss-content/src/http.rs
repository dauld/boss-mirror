//! HTTP surface for `boss-content-api`. Covers bulletins (v1a) and the
//! company manual (v1c).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::ContentError;

use crate::port::ContentRepository;
use crate::types::{BulletinDraft, BulletinPatch, ManualPatch, ManualSectionDraft, UserContext};

pub struct ContentApiState {
    pub repo: Arc<dyn ContentRepository>,
    /// Domain event publisher. Optional so test setups that don't
    /// wire NATS keep working; production binaries always attach a
    /// `PgAuditWriter` so bulletin events land in `audit_log`.
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: ContentApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/content/health", get(health))
        .route("/api/content/bulletins/my-day", get(list_my_day))
        .route("/api/content/bulletins", get(list_all).post(create))
        .route("/api/content/bulletins/{id}", put(update).delete(remove))
        .route("/api/content/bulletins/{id}/dismiss", post(dismiss))
        .route("/api/content/manual", get(manual_tree).post(section_create))
        .route("/api/content/manual-history/{*slug}", get(section_history))
        .route(
            "/api/content/manual/{*slug}",
            get(section_get).put(section_update),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-content-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

/// Pull the caller's identity from the `X-Boss-User` header (injected
/// by the gateway). v1a uses this for audience filtering + dismissal;
/// later we'll also use it for policy checks. Returns a status + body
/// tuple so callers shape the `Response` at the call site — avoids the
/// `result_large_err` hit from carrying a full `Response` in `Err`.
fn user_from_headers(headers: &HeaderMap) -> Result<UserContext, (StatusCode, String)> {
    let Some(raw) = headers.get("x-boss-user").and_then(|v| v.to_str().ok()) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing X-Boss-User header".into(),
        ));
    };
    serde_json::from_str::<UserContext>(raw)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad user header: {e}")))
}

fn err(e: ContentError) -> Response {
    match e {
        ContentError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        ContentError::Validation(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        ContentError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

// --- bulletins ------------------------------------------------------------

#[derive(Deserialize)]
struct ListMyDayQuery {
    #[serde(default)]
    include_dismissed: bool,
}

async fn list_my_day(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Query(q): Query<ListMyDayQuery>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    match state
        .repo
        .list_bulletins_for(&user, today, q.include_dismissed)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn list_all(State(state): State<Arc<ContentApiState>>) -> Response {
    match state.repo.list_all_bulletins().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Json(draft): Json<BulletinDraft>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    // OUTBOX (phase 2): the adapter records the bulletin events
    // inside the domain transaction; nothing publishes post-commit.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .repo
        .create_bulletin_at(draft, &user.id, stamp.timestamp, &stamp)
        .await
    {
        Ok(b) => (StatusCode::CREATED, Json(b)).into_response(),
        Err(e) => err(e),
    }
}

async fn update(
    State(state): State<Arc<ContentApiState>>,
    Path(id): Path<Uuid>,
    _headers: HeaderMap,
    Json(patch): Json<BulletinPatch>,
) -> Response {
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .repo
        .update_bulletin_at(id, patch, stamp.timestamp, &stamp)
        .await
    {
        Ok(b) => Json(b).into_response(),
        Err(e) => err(e),
    }
}

async fn remove(
    State(state): State<Arc<ContentApiState>>,
    Path(id): Path<Uuid>,
    _headers: HeaderMap,
) -> Response {
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .repo
        .delete_bulletin_at(id, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

async fn dismiss(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .repo
        .dismiss_bulletin_at(id, &user.id, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

// --- manual ---------------------------------------------------------------

async fn manual_tree(State(state): State<Arc<ContentApiState>>, headers: HeaderMap) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.manual_tree(&user).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn section_get(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.get_section(&slug, &user).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("section {slug}")).into_response(),
        Err(e) => err(e),
    }
}

async fn section_create(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Json(draft): Json<ManualSectionDraft>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.create_section(draft, &user.id).await {
        Ok(s) => (StatusCode::CREATED, Json(s)).into_response(),
        Err(e) => err(e),
    }
}

async fn section_update(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(patch): Json<ManualPatch>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.update_section(&slug, patch, &user.id).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err(e),
    }
}

/// The history answers only what the read would (backlog 18345a5b):
/// it asks `get_section` for the caller first, so an unpublished section
/// or one whose audience does not match is the read's 404 here too.
/// Until then the route took no caller and returned every version's
/// full body, so a section the read refused was readable through it.
async fn section_history(
    State(state): State<Arc<ContentApiState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let user = match user_from_headers(&headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.repo.get_section(&slug, &user).await {
        Ok(Some(_)) => {}
        Ok(None) => return (StatusCode::NOT_FOUND, format!("section {slug}")).into_response(),
        Err(e) => return err(e),
    }
    match state.repo.section_history(&slug).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}
