//! `boss-accounts-api` — accounts + account_notes + account_team_members
//! + account_next_actions + account_risk_scores + support_cases.
//! One binary per domain, the pattern every other core domain uses
//! (boss-jobs-api, boss-commerce-api, boss-ledger-api, etc).
//!
//! Cross-service dep tree: Postgres pool, NATS publisher (with audit
//! writer), Clock client, ClassesClient (for account_team_role
//! validation), AssetsClient (for the open-ticket-count lookup on
//! account detail).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_accounts::account_next_actions::next_actions_router;
use boss_accounts::account_notes::account_notes_router;
use boss_accounts::account_risk_scores::risk_scores_router;
use boss_accounts::account_team_members::account_team_router;
use boss_accounts::accounts::accounts_router;
use boss_accounts::accounts_api_config::AccountsApiConfig;
use boss_accounts::support_cases::support_cases_router;
use boss_assets_client::{AssetsClient, ReqwestAssetsClient};
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-accounts-api",
    about = "Boss Accounts API service (accounts + team + notes + cases)",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-accounts-api.toml")]
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
    let cfg = AccountsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-accounts-api starting");

    let pool = PgPoolOptions::new()
        .max_connections(15)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    // Class registry for write-time taxonomy validation: account_team_role,
    // plus account_type / tier / note-kind under subject_kind='account'.
    let classes_client: Arc<dyn ClassesClient> =
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone()));

    // Assets client for open-ticket-count on account detail.
    let assets_client: Arc<dyn AssetsClient> =
        Arc::new(ReqwestAssetsClient::new(cfg.assets_api_url.clone()));

    // Clock client — every domain writer stamps `now` through it so
    // sim-mode audit_log rows pick up the sim-day clock.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // NATS publisher + audit writer. Optional — without NATS, the
    // routers still serve reads but writes don't emit to audit_log.
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "accounts")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())))
                .with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
                    clock.clone(),
                )));
            info!(nats_url = %url, "NATS publisher + audit_log writer wired");
            Some(pub_)
        }
        None => {
            tracing::warn!("no nats_url configured — account writes won't emit to audit_log");
            None
        }
    };

    // Compose the six routers under one app. Mirrors the boss-people-api
    // merge order so the route table is identical to the pre-split state.
    let app = axum::Router::new()
        .merge(accounts_router(
            pool.clone(),
            publisher.clone(),
            assets_client.clone(),
            clock.clone(),
            Some(classes_client.clone()),
        ))
        .merge(account_team_router(
            pool.clone(),
            publisher.clone(),
            clock.clone(),
            Some(classes_client.clone()),
        ))
        .merge(account_notes_router(
            pool.clone(),
            publisher.clone(),
            clock.clone(),
            Some(classes_client.clone()),
        ))
        .merge(next_actions_router(pool.clone(), clock.clone()))
        .merge(risk_scores_router(pool.clone()))
        .merge(support_cases_router(
            pool.clone(),
            publisher.clone(),
            clock.clone(),
        ))
        // Deploy + assets probes hit /api/accounts/health; mount
        // a simple liveness route so they don't 404. The six sub-routers
        // all mount under /api/people/accounts/*; this is the canonical
        // service-level health.
        .route(
            "/api/accounts/health",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"status":"ok","service":"boss-accounts-api"}))
            }),
        );
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
    info!(addr = %http_addr, "boss-accounts-api listening");

    let app = boss_core::machine_gate::mount(app, "accounts", &["/api/accounts/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
