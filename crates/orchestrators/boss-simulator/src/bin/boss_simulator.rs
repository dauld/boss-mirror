//! boss-simulator — hosts the Simulator UX.
//!
//! Serves the `apps/simulator` SPA under `/simulator` (the gateway
//! reverse-proxies `/simulator/*` here WITHOUT stripping the prefix, so
//! we nest the whole sub-app under `/simulator`) plus the
//! `/simulator/api/*` control surface.
//!
//! This service is the single owner of sim CONTROL: the sim control plane
//! has no public path (clock-api isn't gateway-proxied; "configure
//! epoch/warp" has no other endpoint), so the controls live here behind
//! an operator gate, forwarding server-to-server to clock-api +
//! jobs-api's sim-clock endpoints. Read-only cockpit data (live jobs,
//! events) rides the existing gateway `/api/*` proxies, so it is NOT
//! re-hosted here.

use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use boss_policy_client::{CurrentUser, request_context_middleware};
use reqwest::Client;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

struct AppState {
    http: Client,
    jobs_url: String,
    clock_url: String,
    /// The brewery-sim DAEMON's localhost control+telemetry server
    /// (boss-ports `sim-control`). Read endpoints (telemetry, config GET)
    /// are open; config writes are operator-gated + forwarded here.
    sim_control_url: String,
}

const STUB_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>BOSS Simulator</title></head>\
<body style=\"font-family:system-ui;padding:40px;color:#1c1917\">\
<h1>BOSS Simulator</h1>\
<p>The simulator UI bundle is not installed yet. Build <code>apps/simulator</code> \
and deploy it to <code>BOSS_SIM_STATIC_DIR</code>.</p></body></html>";

/// Sim controls mutate the shared clock + trim audit_log, so they're for
/// signed-in operators only — never anonymous visitors (whose gateway
/// session carries a read-only-floor role: `visitor`, or `audit-readonly`
/// where the instance opts in — design 2830b6b7) or the headerless
/// `guest` fallback. Mirrors jobs-api's `operator_guard`, and asks the
/// same `boss_core::roles::is_read_only_floor` rather than a role name.
fn operator_guard(user: &CurrentUser) -> Option<Response> {
    let role = user.0.role.as_str();
    if boss_core::roles::is_read_only_floor(role) || role == "guest" {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "sim controls require a signed-in operator",
            )
                .into_response(),
        );
    }
    None
}

/// Forward an operator-gated POST to an upstream control endpoint,
/// re-attaching the caller's `x-boss-user` so the upstream's own operator
/// gate (jobs-api) also passes, and relaying the upstream status + body.
async fn forward_post(
    state: &AppState,
    url: String,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Response {
    let mut req = state.http.post(&url);
    if let Some(u) = headers.get("x-boss-user") {
        req = req.header("x-boss-user", u);
    }
    if let Some(b) = body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(b);
    }
    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    }
}

/// Forward a plain read-only GET to a localhost upstream — the daemon's
/// control server (telemetry / config) or clock-api (clock state) — and
/// relay the JSON. No operator gate and no actor header needed; these
/// upstreams are localhost-only.
async fn forward_get(state: &AppState, url: String) -> Response {
    match state.http.get(&url).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("upstream unreachable: {e}"),
        )
            .into_response(),
    }
}

/// GET /simulator/api/telemetry — how the daemon is engaging the public
/// API right now (Cockpit). Open read; proxies the daemon's /telemetry.
async fn get_telemetry(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/telemetry", s.sim_control_url)).await
}

/// GET /simulator/api/clock — the authoritative clock state (sim time,
/// warp, paused) straight from clock-api, which owns it. The cockpit reads
/// its clock readouts here rather than off the daemon's /telemetry, so they
/// stay correct even while the daemon is stopped (e.g. mid seed-rebuild,
/// when its /telemetry is down). clock-api isn't gateway-proxied, so this is
/// the cockpit's only path to it. Open read.
async fn get_clock(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/api/clock/now", s.clock_url)).await
}

/// GET /simulator/api/config — the daemon's effective behavior config
/// (the Controls editor loads this). Open read.
async fn get_config(State(s): State<Arc<AppState>>) -> Response {
    forward_get(&s, format!("{}/config", s.sim_control_url)).await
}

/// POST /simulator/api/config — operator-gated. Persists an operator-
/// edited behavior config to the daemon, which validates it, writes the
/// override, and restarts to apply it (the "edit + restart" model).
async fn post_config(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = operator_guard(&user) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/config", s.sim_control_url),
        &headers,
        Some(body),
    )
    .await
}

async fn control_pause(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard(&user) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/pause", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_resume(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard(&user) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/resume", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_restart_epoch(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = operator_guard(&user) {
        return r;
    }
    forward_post(
        &s,
        format!("{}/api/jobs/sim-clock/restart-epoch", s.jobs_url),
        &headers,
        None,
    )
    .await
}

async fn control_configure(
    State(s): State<Arc<AppState>>,
    user: CurrentUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = operator_guard(&user) {
        return r;
    }
    // The new capability with no public path: epoch_start / epoch_end /
    // warp_factor. clock-api validates the body; we just proxy it.
    forward_post(
        &s,
        format!("{}/api/clock/configure", s.clock_url),
        &headers,
        Some(body),
    )
    .await
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "boss-simulator" }))
}

async fn stub() -> Html<&'static str> {
    Html(STUB_HTML)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    // Loopback default: reached through the gateway's /simulator
    // proxy, never directly (SECURITY.md §Deployment trust model).
    // BOSS_SIM_BIND widens deliberately.
    let bind = std::env::var("BOSS_SIM_BIND")
        .unwrap_or_else(|_| format!("127.0.0.1:{}", boss_ports::prod("simulator")));
    let static_dir = std::env::var("BOSS_SIM_STATIC_DIR")
        .unwrap_or_else(|_| "/var/lib/boss-simulator/dist".to_string());

    let state = Arc::new(AppState {
        http: Client::new(),
        jobs_url: std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| boss_ports::url("jobs")),
        clock_url: std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock")),
        sim_control_url: std::env::var("BOSS_SIM_CONTROL_URL")
            .unwrap_or_else(|_| boss_ports::url("sim-control")),
    });

    // The /simulator sub-app: control API routes + the SPA (or a stub
    // until apps/simulator is built). Nested under /simulator because the
    // gateway forwards that prefix unchanged.
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/telemetry", get(get_telemetry))
        .route("/api/clock", get(get_clock))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/control/pause", post(control_pause))
        .route("/api/control/resume", post(control_resume))
        .route("/api/control/restart-epoch", post(control_restart_epoch))
        .route("/api/control/configure", post(control_configure))
        .with_state(state);

    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let sim = if Path::new(&index).exists() {
        let serve = ServeDir::new(&static_dir).fallback(ServeFile::new(index));
        api.fallback_service(serve)
    } else {
        info!(dir = %static_dir, "no SPA bundle found — serving stub page");
        api.fallback(stub)
    };

    let app = Router::new()
        .nest("/simulator", sim)
        // Scopes the request actor (x-boss-user) + sim-origin flag for the
        // duration of each handler, like every other service.
        .layer(axum::middleware::from_fn(request_context_middleware))
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind `{bind}`"))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    info!(addr = %addr, static_dir = %static_dir, "boss-simulator listening");
    let app = boss_core::machine_gate::mount(app, "simulator", &["/simulator/api/health"]);
    axum::serve(listener, app).await.context("serving HTTP")?;
    Ok(())
}

use tokio::net::TcpListener;

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{AccessTier, User};

    fn as_role(role: &str) -> CurrentUser {
        CurrentUser(User {
            id: "x".into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
    }

    /// Design 2830b6b7: the same floor as jobs-api's guard. The OSS
    /// guest's `visitor` is refused; keyed on the name `audit-readonly`
    /// it would have driven the engine.
    #[test]
    fn every_read_only_floor_role_and_the_headerless_guest_are_refused() {
        for role in boss_core::roles::READ_ONLY_FLOOR_ROLES
            .into_iter()
            .chain(["guest"])
        {
            let refused = operator_guard(&as_role(role));
            assert_eq!(
                refused.map(|r| r.status()),
                Some(StatusCode::FORBIDDEN),
                "{role} must not drive the simulator"
            );
        }
        assert!(operator_guard(&as_role(boss_core::roles::VISITOR_ROLE)).is_some());
        assert!(operator_guard(&as_role("platform-admin")).is_none());
    }
}
