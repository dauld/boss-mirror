//! `boss-inventory-api` service: inventory items and purchase orders backed by Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_assets_client::ReqwestAssetsClient;
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_inventory::http::{InventoryApiState, WarehouseClients, router};
use boss_inventory::inventory_config::InventoryApiConfig;
use boss_jobs_client::ReqwestJobsClient;
use boss_shipping_client::ReqwestShippingClient;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-inventory-api",
    about = "Boss Inventory API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-inventory-api.toml")]
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
    let cfg = InventoryApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-inventory-api starting");

    // One pool per service. PgPool is internally Arc'd, so cloning is
    // cheap and every sub-router/audit-writer shares the same slots.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let inventory = Arc::new(boss_inventory::PgInventory::new(pool.clone()));

    // Connect to NATS for domain event publishing (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            #[allow(unused_mut)]
            let mut pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "inventory");
            {
                pub_ = pub_.with_audit(std::sync::Arc::new(boss_events::PgAuditWriter::new(
                    pool.clone(),
                )));
            }
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — domain events will not be published");
            None
        }
    };

    let clients = WarehouseClients {
        jobs: Arc::new(ReqwestJobsClient::new(cfg.jobs_api_url.clone())),
        assets: Arc::new(ReqwestAssetsClient::new(cfg.assets_api_url.clone())),
        shipping: Arc::new(ReqwestShippingClient::new(cfg.shipping_api_url.clone())),
    };
    info!(
        jobs = %cfg.jobs_api_url,
        assets = %cfg.assets_api_url,
        shipping = %cfg.shipping_api_url,
        "warehouse-status cross-service clients configured"
    );

    // Class registry validation for DiscrepancyKind. Mandatory and
    // fail-loud: the endpoint is wired from required config, so a
    // present `discrepancy_kind` is always validated against the
    // registry. A clean three-way match (no discrepancy_kind) still
    // skips the lookup — the gate is identity-first, not the wiring.
    // Mirrors the
    // boss-people / boss-accounts role-validation wiring.
    let classes_client = Some(
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone())) as Arc<dyn ClassesClient>,
    );
    info!(classes_url = %cfg.classes_api_url, "DiscrepancyKind validation enabled");

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every event
    // stamp resolves `_simulated: bool` from clock mode without
    // per-handler changes (outbox phase 2: handlers build
    // EventStamps from this publisher and the repository records
    // the events in the domain transaction).
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    let state = InventoryApiState {
        inventory,
        publisher,
        clients: Some(clients),
        classes_client,
        clock,
    };
    // Merge the Vendor CRM sub-router (session 1 of procurement-
    // team-needs). Postgres-only: it reads/writes vendor_contacts,
    // vendor_interactions, vendor_account_team, vendor_contracts
    // directly via PgProcurement.
    let app = {
        use boss_inventory::procurement::http::{
            ProcurementApiState, router as procurement_router,
        };
        let proc_pub = state.publisher.clone();
        let proc_clock = state.clock.clone();
        router(state).merge(procurement_router(ProcurementApiState {
            pool: pool.clone(),
            publisher: proc_pub,
            clock: proc_clock,
        }))
    };
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
    info!(addr = %http_addr, "inventory HTTP API listening");

    axum::serve(listener, app).await?;
    Ok(())
}
