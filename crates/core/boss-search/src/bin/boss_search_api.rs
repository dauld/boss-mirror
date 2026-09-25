//! `boss-search-api` — the global search read surface.

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-search-api", about = "Global search API", version)]
struct Cli {
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: String,
    #[arg(long, default_value_t = boss_ports::prod("search"))]
    http_port: u16,
    #[arg(long, env = "BOSS_POLICY_URL", default_value_t = boss_ports::url("policy"))]
    policy_url: String,
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

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&cli.postgres_url)
        .await
        .context("connecting to Postgres")?;

    // Same wrapping jobs-api uses: on a sim instance a sim caller is
    // authorized at the boundary; everything else is enforced per-role
    // by the inner client (backlog 85e7f10f).
    let policy: std::sync::Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(std::sync::Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(cli.policy_url),
        ));

    let app = boss_search::http::router(boss_search::http::SearchApiState { pool, policy });
    let addr = format!("127.0.0.1:{}", cli.http_port);
    tracing::info!(addr = %addr, "boss-search-api listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let app = boss_core::machine_gate::mount(app, "search", &["/api/search/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
