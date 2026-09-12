//! `boss-commerce-api` service: opportunities and invoices backed by Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_commerce::commerce_config::CommerceApiConfig;
use boss_commerce::http::{CommerceApiState, router};
use boss_people_client::{PeopleClient, ReqwestPeopleClient};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-commerce-api",
    about = "Boss Commerce API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-commerce-api.toml")]
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
    let cfg = CommerceApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        http_bind = %cfg.http_bind,
        people_api_url = %cfg.people_api_url,
        "boss-commerce-api starting"
    );

    let people_client: Arc<dyn PeopleClient> =
        Arc::new(ReqwestPeopleClient::new(cfg.people_api_url.clone()));

    // Class registry validation for InvoiceStatus under
    // subject_kind='invoice'. Required and fail-loud: the URL comes
    // from config (validated non-empty at startup), so the gate is
    // always wired in production — mirrors the boss-shipping carrier
    // wiring. The state field stays `Option` only so tests can pass
    // `None`; production always passes `Some`.
    let classes_client = Some(
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone())) as Arc<dyn ClassesClient>,
    );
    info!(classes_url = %cfg.classes_api_url, "InvoiceStatus validation enabled");

    // One pool per service. PgPool is internally Arc'd, so cloning is
    // cheap and every sub-router shares the same connection slots.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let commerce = Arc::new(boss_commerce::PgCommerce::new(pool.clone()));

    // Connect to NATS for domain event publishing (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            #[allow(unused_mut)]
            let mut pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "commerce");
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

    let agreements_publisher = publisher.clone();
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // every stamp automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    let state = CommerceApiState {
        commerce,
        publisher,
        people_client,
        policy: None,
        clock,
        classes_client,
    };

    let agreements_app = boss_commerce::agreements::agreements_router(
        pool.clone(),
        agreements_publisher,
        state.clock.clone(),
    );

    let app = router(state).merge(agreements_app);
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
    info!(addr = %http_addr, "commerce HTTP API listening");

    axum::serve(listener, app).await?;
    Ok(())
}
