//! `GET /api/yard/routes?window=24h` — every route the IT map may draw,
//! with the sources that support it (design e765b3fc §2b; car R2 on
//! feedback 84cba7e2). The derivation is [`crate::routes`], pure and
//! pinned against the tree; this is the adapter that reads what it walks
//! — the active Workflow rows, the station rows as the stations read
//! projects them, and the moves record's counts.
//!
//! THE SAME DERIVATION EVERY JUDGE READS. The regions read judges its
//! observed-undeclared band against it ([`derived`]), and the mover
//! stamps each move it records against it, so the route a surface draws,
//! the route a move is judged by and the route this read serves are one
//! answer (CLAUDE.md §9a).
//!
//! UNREAD IS NOT EMPTY. With no workflow or station registry wired, or
//! either unreadable, there is nothing to walk: this read answers 503 and
//! names which, and the regions read leaves `undeclared` null rather than
//! judging every move against an empty set of routes.

use super::*;

use crate::moves::RouteCount;
use crate::routes::RouteMap;

/// The routes, derived from the registries this API holds, with the
/// observed counts laid over them. `Err` names the registry that could
/// not be read.
pub(super) async fn derived<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    observed: Option<&[RouteCount]>,
) -> Result<RouteMap, String> {
    let kinds = state
        .kind_registry
        .as_ref()
        .ok_or("no workflow registry is wired, so no protocol can be walked")?;
    let reg = state
        .stations
        .as_ref()
        .ok_or("no station registry is wired, so no station can hold a packet")?;
    let specs = kinds
        .list_active(None)
        .await
        .map_err(|e| format!("the workflow registry could not be read: {e}"))?;
    let stations = super::stations::effective_stations(state, reg)
        .await
        .map_err(|resp| {
            format!(
                "the station registry could not be read (HTTP {})",
                resp.status()
            )
        })?;
    let now = boss_clock_client::now_from(&state.clock).await;
    Ok(crate::routes::derive_from(&specs, &stations, observed, now))
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct RoutesQuery {
    window: Option<String>,
}

pub(super) async fn yard_routes<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<RoutesQuery>,
) -> Response {
    // The routes name kinds and steps, never a packet, and the counts
    // beside them are totals per route — so any caller who may read a
    // packet at all may read them; one who may read none is refused by
    // name rather than served an empty map.
    match state.policy.scope_predicate(&user, Resource::job()).await {
        Err(e) => return e.into_response(),
        Ok(p) if job_scope_from_predicate(&user, &p) == JobScope::None => {
            return (
                StatusCode::FORBIDDEN,
                "the routes are read by a caller who may read packets; this caller may read none",
            )
                .into_response();
        }
        Ok(_) => {}
    }
    let window_hours = match crate::regions::parse_window(q.window.as_deref()) {
        Ok(h) => h,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    // The record's counts, when it is wired and answers. `None` serves
    // the declared routes with no counts beside them — never zeroes.
    let observed = match state.yard_moves.as_ref() {
        Some(feed) => {
            let since = boss_clock_client::wall_now() - chrono::Duration::hours(window_hours);
            feed.store.crossings(since).await.ok()
        }
        None => None,
    };
    match derived(&state, observed.as_deref()).await {
        Ok(map) => Json(serde_json::json!({
            "window_hours": window_hours,
            "observed": observed.is_some(),
            "routes": map.routes,
            "walked": map.walked,
            "refused": map.refused,
        }))
        .into_response(),
        Err(why) => (StatusCode::SERVICE_UNAVAILABLE, why).into_response(),
    }
}
