//! `boss-classes-api` service: read-only Class registry over Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_classes::classes_config::ClassesApiConfig;
use boss_classes::http::{ClassesApiState, router};
use boss_classes::port::ClassRepository;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-classes-api",
    about = "Boss Class registry API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-classes-api.toml")]
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
    let cfg = ClassesApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-classes-api starting");

    let classes: Arc<dyn ClassRepository> = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        Arc::new(boss_classes::PgClasses::new(pool))
    };

    let state = ClassesApiState { classes };
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
    info!(addr = %http_addr, "classes HTTP API listening");

    axum::serve(listener, app).await?;
    Ok(())
}
