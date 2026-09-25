//! `boss-campaigns-api` — HTTP service for marketing campaigns.
//!
//! No NATS publisher: the create path stages its event on the
//! transactional outbox (#118) inside the same transaction as the
//! domain write; boss-event-relay moves it to audit_log + NATS.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_campaigns::http::{CampaignsApiState, router};
use boss_campaigns::postgres::PgCampaigns;
use clap::Parser;
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-campaigns-api", about = "Boss Campaigns API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-campaigns-api.toml")]
    config: PathBuf,
}

#[derive(Deserialize)]
struct Config {
    http_bind: String,
    postgres_url: String,
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
    let cfg: Config = toml::from_str(
        &std::fs::read_to_string(&cli.config)
            .with_context(|| format!("reading config {}", cli.config.display()))?,
    )
    .with_context(|| format!("parsing config {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-campaigns-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .context("connecting to Postgres")?;

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let state = CampaignsApiState {
        campaigns: Arc::new(PgCampaigns::new(pool)),
        clock,
    };

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-campaigns-api listening");
    let app =
        boss_core::machine_gate::mount(router(state), "campaigns", &["/api/campaigns/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
