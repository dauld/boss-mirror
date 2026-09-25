//! `boss-events-api` — audit_log read surface.
//!
//! Hosts `/api/events/tail`, `/api/events/stream`,
//! `/api/events/export`, and `/api/events/public-tail` from
//! `boss_events::tail_http::audit_tail_router`. Pre-2026-06 these
//! routes were mounted in `boss-people-api` for convenience
//! (people-api already had the Postgres pool wired up). Splitting
//! them into a dedicated service makes audit_log access a
//! first-class tier-1 surface — same shape every other core domain
//! takes (boss-jobs-api, boss-ledger-api, boss-classes-api).
//!
//! Auth: same as the original router — operator/auditor tier or
//! has_global_read role for tail/stream/export; public-tail is
//! unauth (curated topic allowlist).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_events::events_api_config::EventsApiConfig;
use boss_events::tail_http::audit_tail_router;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-events-api",
    about = "Boss audit_log read surface (tail / stream / export)",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-events-api.toml")]
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
    let cfg = EventsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-events-api starting");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres for audit_log reads")?;

    let app = audit_tail_router(pool);
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-events-api listening");

    let app = boss_core::machine_gate::mount(app, "events", &["/api/events/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
