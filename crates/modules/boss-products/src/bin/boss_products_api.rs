//! `boss-products-api` — HTTP service for the finished-product
//! catalog + per-location on-hand inventory.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_products::config::ProductsApiConfig;
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-products-api", about = "Boss Products API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-products-api.toml")]
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
    let cfg = ProductsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-products-api starting");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let products = Arc::new(PgProducts::new(pool.clone()));

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    let classes_client = Some(Arc::new(boss_classes_client::ReqwestClassesClient::new(
        cfg.classes_api_url.clone(),
    )) as Arc<dyn boss_classes_client::ClassesClient>);
    info!(classes_url = %cfg.classes_api_url, "product taxonomy validation enabled");

    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            // with_sim_probe → publisher auto-injects
            // `_simulated: bool` on every audit_log row.
            let p = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "products")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())))
                .with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
                    clock.clone(),
                )));
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(Arc::new(p))
        }
        None => {
            info!("no nats_url configured — products events will not be published");
            None
        }
    };

    let state = ProductsApiState {
        products,
        publisher,
        clock,
        classes_client,
    };
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
    info!(addr = %http_addr, "boss-products-api listening");
    let app = boss_core::machine_gate::mount(app, "products", &["/api/products/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
