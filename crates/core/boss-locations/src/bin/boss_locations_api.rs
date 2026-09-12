//! `boss-locations-api` service: read-only Locations registry over Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_locations::http::{LocationsApiState, router};
use boss_locations::locations_config::LocationsApiConfig;
use boss_locations::port::LocationRepository;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-locations-api",
    about = "Boss Locations registry API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-locations-api.toml")]
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
    let cfg = LocationsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-locations-api starting");

    let locations: Arc<dyn LocationRepository> = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        Arc::new(boss_locations::PgLocations::new(pool))
    };

    let state = LocationsApiState { locations };
    let app = router(state);
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
    info!(addr = %http_addr, "locations HTTP API listening");

    axum::serve(listener, app).await?;
    Ok(())
}
