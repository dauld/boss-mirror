//! `boss-cybernetics` service: per-VM agent runtime.
//!
//! Wires NATS (event bus) + in-memory adapters + stub dispatcher onto the
//! Cybernetics runtime, plus the ingress bridge and HTTP introspection API.
//! Reads config from a TOML file.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use boss_clock_client::{ClockClient, ReqwestClockClient};
use boss_core::agent::{AgentId, Message};
use boss_core::port::EventBus;
use boss_cybernetics::Cybernetics;
use boss_cybernetics::config::Config;
use boss_cybernetics::http::{HttpState, router};
use boss_cybernetics::ingress::Ingress;
use boss_events::claude_dispatcher::ClaudeCodeDispatcher;
use boss_events::ledger::InMemoryCostLedger;
use boss_events::queue::InMemoryMessageQueue;
use boss_events::registry::InMemoryAgentRegistry;
use boss_nats::NatsEventBus;
use clap::Parser;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-cybernetics", about = "Boss Cybernetics service", version)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-cybernetics.toml")]
    config: PathBuf,
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
        vm_id = %cfg.vm_id,
        nats_url = %cfg.nats_url,
        http_bind = %cfg.http_bind,
        agents = cfg.agents.len(),
        "boss-cybernetics starting"
    );

    let specs = cfg.to_specs().context("building agent specs")?;
    let registry = Arc::new(InMemoryAgentRegistry::new(specs));
    let queue = Arc::new(InMemoryMessageQueue::new());
    let ledger = Arc::new(InMemoryCostLedger::new());
    let dispatcher = Arc::new(ClaudeCodeDispatcher::new());
    let bus_nats = NatsEventBus::connect(&cfg.nats_url)
        .await
        .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?;
    let bus: Arc<dyn EventBus> = Arc::new(bus_nats);

    // Authoritative clock — every telemetry timestamp stamps from
    // here so sim mode produces sim-dated audit_log rows.
    let clock_url =
        std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| "http://localhost:7060".to_string());
    let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(clock_url));

    // Build a DomainPublisher with both the NATS bus AND the Postgres
    // audit writer so cybernetics.* events land in audit_log — NATS
    // alone drops agent dispatch/denial/cost-record rows when the
    // broadcast channel rolls over. Source includes the vm_id so
    // per-VM telemetry is grep-able in audit_log: `cybernetics/<vm_id>`.
    let source = format!("cybernetics/{}", cfg.vm_id);
    let publisher = boss_core::publisher::DomainPublisher::new(bus.clone(), &source);
    // This binary is `required-features = ["postgres"]`, so there is
    // no feature-less build to fall back to an in-memory recorder; the
    // outbox recorder is the only recorder.
    let recorder: Arc<dyn boss_core::port::EventRecorder> = {
        let pg_url = std::env::var("BOSS_POSTGRES_URL")
            .or_else(|_| {
                cfg.postgres_url
                    .clone()
                    .ok_or(std::env::VarError::NotPresent)
            })
            .context(
                "BOSS_POSTGRES_URL not set and no postgres_url in config — \
                 cybernetics needs Postgres for the audit_log writer (#76)",
            )?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&pg_url)
            .await
            .with_context(|| "connecting to Postgres for cybernetics telemetry outbox")?;
        // OUTBOX (phase 2): telemetry records on the transactional
        // outbox; boss-event-relay delivers to audit_log + NATS. The
        // publisher no longer needs a direct audit writer.
        info!("transactional-outbox recorder wired for cybernetics events");
        Arc::new(boss_events::outbox::PgOutboxRecorder::new(pool))
    };
    let publisher = Arc::new(publisher.with_sim_probe(Arc::new(
        boss_clock_client::ClockSimProbe::new(clock.clone()),
    )));

    let cyb = Arc::new(Cybernetics::new(
        cfg.vm_id.clone(),
        queue.clone(),
        ledger.clone(),
        dispatcher.clone(),
        registry.clone(),
        publisher,
        recorder,
    ));

    let (cancel_tx, cancel_rx) = watch::channel(false);

    // 1. Attach ingress (subscribe to boss.s1.{vm}.> BEFORE we announce ready).
    let attached = Ingress::new(cyb.clone())
        .attach(bus.clone())
        .await
        .context("attaching ingress")?;

    // 2. Spawn Cybernetics completion loop.
    let loop_cyb = cyb.clone();
    let loop_rx = cancel_rx.clone();
    let loop_task = tokio::spawn(async move {
        if let Err(e) = loop_cyb.run(loop_rx).await {
            error!(error = %e, "cybernetics loop exited with error");
        }
    });

    // 3. Spawn ingress consume loop.
    let ingress_rx = cancel_rx.clone();
    let ingress_task = tokio::spawn(async move {
        if let Err(e) = attached.run(ingress_rx).await {
            error!(error = %e, "ingress exited with error");
        }
    });

    // 4. Spawn scheduled agent triggers.
    let scheduler_handles =
        boss_cybernetics::scheduler::spawn_schedulers(&cfg.agents, cyb.clone(), cancel_rx.clone());

    // 5. Spawn HTTP server.
    let http_state = HttpState {
        vm_id: Arc::from(cfg.vm_id.as_str()),
        queue: queue.clone(),
        dispatcher: dispatcher.clone(),
        ledger: ledger.clone(),
        registry: registry.clone(),
        clock: clock.clone(),
    };
    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "HTTP introspection API listening");

    // Dispatch endpoint — allows the CTO dashboard to trigger agent runs.
    let dispatch_cyb = cyb.clone();
    let dispatch_router = axum::Router::new()
        .route("/dispatch", post(dispatch_handler))
        .with_state(dispatch_cyb);

    let app = router(http_state).merge(dispatch_router);
    let mut http_rx = cancel_rx.clone();
    let http_task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = http_rx.changed().await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!(error = %e, "http server exited with error");
        }
    });

    // 6. Wait for Ctrl+C.
    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);

    let _ = tokio::join!(loop_task, ingress_task, http_task);
    for h in scheduler_handles {
        let _ = h.await;
    }
    info!("boss-cybernetics shut down cleanly");
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch endpoint
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DispatchRequest {
    agent_id: String,
    prompt: String,
}

async fn dispatch_handler(
    State(cyb): State<Arc<Cybernetics>>,
    Json(body): Json<DispatchRequest>,
) -> impl IntoResponse {
    let agent_id = match AgentId::try_new(&body.agent_id) {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid agent_id: {e}")),
    };

    let message = Message::new(
        agent_id,
        "script.run",
        serde_json::json!({ "prompt": body.prompt }),
    );

    match cyb.submit(message).await {
        Ok(msg_id) => (
            StatusCode::ACCEPTED,
            format!("{{\"ok\":true,\"message_id\":\"{msg_id}\"}}"),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"{e}\"}}"),
        ),
    }
}
