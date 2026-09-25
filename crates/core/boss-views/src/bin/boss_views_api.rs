//! `boss-views-api` — the View registry + the endpoint that runs one.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-views-api", about = "Views API", version)]
struct Cli {
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: String,
    #[arg(long, default_value_t = boss_ports::prod("views"))]
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

    // A View reads policy-scoped tables, so the resolver needs the
    // policy engine the same way every other scoped read surface does.
    // Same wrapping jobs-api uses: on a sim instance a sim caller is
    // authorized at the boundary; everything else is enforced per-role
    // by the inner client (backlog 85e7f10f).
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        boss_policy_client::SimBypassPolicyClient::from_env(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(cli.policy_url),
        ));

    let app = boss_views::http::router(boss_views::http::ViewsApiState {
        repo: Arc::new(boss_views::PgViewsRepo::new(pool.clone())),
        // The OS map reads `event_facts` through the same adapter.
        os_map: Some(Arc::new(boss_views::PgViewsRepo::new(pool.clone()))),
        // Flow reads `audit_log` — the only view that does, because
        // it is the only place the wall clock survives.
        flow: Some(Arc::new(boss_views::PgViewsRepo::new(pool.clone()))),
        // Fleet reads the live step set + audit_log for wall-clock
        // ages (crate::fleet — same doctrine as flow).
        fleet: Some(Arc::new(boss_views::PgViewsRepo::new(pool.clone()))),
        // Stage durations read audit_log wall time (crate::stages).
        stages: Some(Arc::new(boss_views::PgViewsRepo::new(pool.clone()))),
        resolver: Arc::new(boss_views::PgViewResolver::new(pool, policy)),
    });
    let addr = format!("127.0.0.1:{}", cli.http_port);
    tracing::info!(addr = %addr, "boss-views-api listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let app = boss_core::machine_gate::mount(app, "views", &["/api/views/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
