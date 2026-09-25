//! `boss-ml-api` service entry point.
//!
//! Wires the `boss-ml` library + the canonical inference plugin
//! set (`boss-ml-plugins`) into a runnable axum server. Tenant
//! deployments that need additional plugins ship their own
//! `boss-ml-api` binary that adds extra `register_plugin` calls
//! before the dispatcher is wrapped in `Arc`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

use boss_ml::bootstrap::seed_phase_two_candidates;
use boss_ml::config::MlApiConfig;
use boss_ml::http::{MlApiState, router};
use boss_ml::port::MlRepository;
use boss_ml::{InferenceDispatcher, PgMlRepo};
use boss_ml_plugins::AccountChurnRiskV1;

#[derive(Parser, Debug)]
#[command(name = "boss-ml-api", about = "Boss ML Platform API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-ml-api.toml")]
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
    let cfg = MlApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        "boss-ml-api starting"
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;
    let repo: Arc<dyn MlRepository> = Arc::new(PgMlRepo::new(pool.clone()));

    // Route every "today" lookup in declarative rules + plugin
    // paths through ClockClient so sim-mode inferences see sim-today.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let mut dispatcher = InferenceDispatcher::new(pool, repo.clone(), clock);
    // Canonical plugin set. Tenant binaries can layer their own
    // plugins here before the dispatcher is wrapped in Arc below.
    dispatcher.register_plugin(Arc::new(AccountChurnRiskV1::new()));
    info!(
        plugins = ?dispatcher.plugin_names(),
        "inference plugins registered"
    );
    let dispatcher = Some(Arc::new(dispatcher));

    // Bootstrap: upsert the candidate models from embedded seeds.
    // Idempotent across restarts (docs/architecture-decisions.md
    // §ML platform).
    match seed_phase_two_candidates(repo.as_ref()).await {
        Ok(n) => info!(count = n, "bootstrap seed complete"),
        Err(e) => tracing::warn!(error = %e, "bootstrap seed failed — continuing anyway"),
    }

    let state = MlApiState { repo, dispatcher };
    let app = router(state);

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-ml-api listening");
    let app = boss_core::machine_gate::mount(app, "ml", &["/api/ml/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
