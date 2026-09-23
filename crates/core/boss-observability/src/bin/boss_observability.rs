//! `boss-observability` service: cross-VM observability for the Cybernetics stack.
//!
//! - Subscribes to `cybernetics.>` on NATS and fans events out as SSE
//! - Aggregates per-VM Cybernetics HTTP snapshots
//! - Serves the static web dashboard when configured

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use boss_observability::aggregator::{Aggregator, Endpoint, VmResult};
use boss_observability::config::Config;
use boss_observability::demo_agents::{self, Roster};
use boss_observability::sse::{SseHub, run_nats_forwarder};
use clap::Parser;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-observability",
    about = "Boss Observability service",
    version
)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-observability.toml")]
    config: PathBuf,
}

#[derive(Clone)]
struct AppState {
    hub: SseHub,
    aggregator: Arc<Aggregator>,
    /// The tenant's roster when `[demo_agents]` is set: its presence
    /// is what switches `/api/snapshot` to the synthetic answer.
    demo_roster: Option<Arc<Roster>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        bind = %cfg.bind,
        nats_url = %cfg.nats_url,
        vms = cfg.vms.len(),
        demo_agents = cfg.demo_agents.is_some(),
        "boss-observability starting"
    );

    // The roster is the tenant's file (backlog 1c68aebc). A block
    // naming one that is missing or malformed refuses here, naming the
    // path, rather than serving an empty "demo" that reads as real;
    // `boss tenant check` judges the same file with the same loader
    // before the bundle can ride a train.
    let demo_roster = match &cfg.demo_agents {
        Some(demo) => Some(Arc::new(Roster::load(&demo.roster).with_context(|| {
            format!(
                "[demo_agents] roster in {} could not be loaded",
                cli.config.display()
            )
        })?)),
        None => None,
    };

    let hub = SseHub::new();
    let aggregator = Arc::new(Aggregator::new(cfg.vms.clone()));

    let nats = async_nats::connect(&cfg.nats_url)
        .await
        .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?;

    let (cancel_tx, cancel_rx) = watch::channel(false);

    let forwarder_hub = hub.clone();
    let forwarder_cancel = cancel_rx.clone();
    let forwarder_task = tokio::spawn(async move {
        if let Err(e) = run_nats_forwarder(nats, forwarder_hub, forwarder_cancel).await {
            error!(error = %e, "NATS forwarder exited with error");
        }
    });

    if let (Some(demo), Some(roster)) = (&cfg.demo_agents, &demo_roster) {
        demo_agents::spawn_telemetry_loop(
            hub.clone(),
            roster.as_ref().clone(),
            demo.tick_seconds,
            cancel_rx.clone(),
        );
    }

    let state = AppState {
        hub: hub.clone(),
        aggregator: aggregator.clone(),
        demo_roster,
    };
    let app = build_router(state, cfg.static_dir.as_deref());

    let addr: SocketAddr = cfg
        .bind
        .parse()
        .with_context(|| format!("invalid bind `{}`", cfg.bind))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    info!(addr = %addr, "observability HTTP listening");

    let mut shutdown_rx = cancel_rx.clone();
    let http_task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = shutdown_rx.changed().await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!(error = %e, "http server exited with error");
        }
    });

    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);

    let _ = tokio::join!(forwarder_task, http_task);
    info!("boss-observability shut down cleanly");
    Ok(())
}

fn build_router(state: AppState, static_dir: Option<&str>) -> Router {
    let api = Router::new()
        .route("/api/events", get(sse_handler))
        .route("/api/vms", get(list_vms))
        .route("/api/snapshot", get(snapshot))
        .route("/api/vms/{id}/health", get(vm_health))
        .route("/api/vms/{id}/agents", get(vm_agents))
        .route("/api/vms/{id}/queues", get(vm_queues))
        .route("/api/vms/{id}/runs", get(vm_runs))
        .route("/api/vms/{id}/costs", get(vm_costs))
        .route("/api/agents", get(all_agents))
        .route("/api/queues", get(all_queues))
        .route("/api/runs", get(all_runs))
        .route("/api/costs", get(all_costs))
        .route("/api/health", get(all_health))
        // Alias the canonical `/api/<port-name>/health` shape every
        // other service follows, so the SPA's IT Monitoring probe
        // finds us. Without this the panel reports
        // `boss-observability-api` as down even when running. The
        // existing `/api/health` is the cross-VM aggregator endpoint
        // (an actual app feature, not a liveness probe).
        .route(
            "/api/observability/health",
            get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        )
        .with_state(state);

    let router = api.layer(TraceLayer::new_for_http());

    if let Some(dir) = static_dir {
        // SPA-style fallback: any path that doesn't match an API route or a
        // real file on disk serves index.html so client-side routes work on
        // reload (e.g. /exec, /ops).
        let index = format!("{}/index.html", dir.trim_end_matches('/'));
        // `.fallback(...)` (not `.not_found_service(...)`) preserves the
        // inner service's 200 status — an SPA serves index.html for unknown
        // paths and lets client-side routing take over.
        let serve = ServeDir::new(dir).fallback(ServeFile::new(index));
        router.fallback_service(serve)
    } else {
        router
    }
}

async fn sse_handler(State(s): State<AppState>) -> impl IntoResponse {
    s.hub.sse_response()
}

async fn list_vms(State(s): State<AppState>) -> impl IntoResponse {
    #[derive(Serialize)]
    struct VmInfo<'a> {
        id: &'a str,
        http_url: &'a str,
    }
    let list: Vec<VmInfo> = s
        .aggregator
        .vms()
        .iter()
        .map(|v| VmInfo {
            id: &v.id,
            http_url: &v.http_url,
        })
        .collect();
    axum::Json(list).into_response()
}

async fn all_health(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(s.aggregator.fetch_all(Endpoint::Health).await).into_response()
}
async fn all_agents(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(s.aggregator.fetch_all(Endpoint::Agents).await).into_response()
}
async fn all_queues(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(s.aggregator.fetch_all(Endpoint::Queues).await).into_response()
}
async fn all_runs(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(s.aggregator.fetch_all(Endpoint::Runs).await).into_response()
}
async fn all_costs(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(s.aggregator.fetch_all(Endpoint::Costs).await).into_response()
}

async fn snapshot(State(s): State<AppState>) -> impl IntoResponse {
    if let Some(roster) = s.demo_roster.as_deref() {
        return axum::Json(demo_agents::snapshot(roster)).into_response();
    }
    #[derive(Serialize)]
    struct Snapshot {
        demo_mode: bool,
        health: Vec<VmResult>,
        agents: Vec<VmResult>,
        queues: Vec<VmResult>,
        runs: Vec<VmResult>,
        costs: Vec<VmResult>,
    }
    let (health, agents, queues, runs, costs) = tokio::join!(
        s.aggregator.fetch_all(Endpoint::Health),
        s.aggregator.fetch_all(Endpoint::Agents),
        s.aggregator.fetch_all(Endpoint::Queues),
        s.aggregator.fetch_all(Endpoint::Runs),
        s.aggregator.fetch_all(Endpoint::Costs),
    );
    axum::Json(Snapshot {
        demo_mode: false,
        health,
        agents,
        queues,
        runs,
        costs,
    })
    .into_response()
}

async fn vm_health(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    single(&s, &id, Endpoint::Health).await
}
async fn vm_agents(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    single(&s, &id, Endpoint::Agents).await
}
async fn vm_queues(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    single(&s, &id, Endpoint::Queues).await
}
async fn vm_runs(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    single(&s, &id, Endpoint::Runs).await
}
async fn vm_costs(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    single(&s, &id, Endpoint::Costs).await
}

async fn single(s: &AppState, id: &str, endpoint: Endpoint) -> axum::response::Response {
    match s.aggregator.fetch_one_by_id(id, endpoint).await {
        Some(r) => axum::Json(r).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("unknown vm: {id}")})),
        )
            .into_response(),
    }
}
