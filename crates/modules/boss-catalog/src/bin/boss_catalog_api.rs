//! `boss-catalog-api` service: device-catalog API backed by Postgres.
//!
//! Connects to a Postgres database and serves the device catalog —
//! system models, parts, consumables — over HTTP.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_assets_client::{AssetsClient, ReqwestAssetsClient};
use boss_catalog::http::{KbApiState, router};
use boss_catalog::kb_config::KbApiConfig;
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_clock_client::{ClockClient, ReqwestClockClient};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-catalog-api",
    about = "Boss Knowledge Base API service",
    version
)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-catalog-api.toml")]
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
    let cfg = KbApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        assets_api_url = %cfg.assets_api_url,
        classes_api_url = %cfg.classes_api_url,
        "boss-catalog-api starting"
    );

    let assets_client: Arc<dyn AssetsClient> =
        Arc::new(ReqwestAssetsClient::new(cfg.assets_api_url.clone()));

    // Connect to Postgres.
    let pg_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let catalog = Arc::new(boss_catalog::PgKb::new(pg_pool.clone()));

    // Connect to NATS for domain event publishing + audit trail (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            #[allow(unused_mut)]
            let mut pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "kb");
            {
                pub_ = pub_.with_audit(Arc::new(boss_events::PgAuditWriter::new(pg_pool.clone())));
            }
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — domain events will not be published");
            None
        }
    };

    // Class registry validation for the catalog's tenant-extensible
    // taxonomy codes — asset-model `category`, document `kind`, and
    // marketing-asset `kind`. Fail-loud (matching boss-people): the
    // URL is a required config field, so this is always wired —
    // app-layer validation is the only gate keeping a typo'd or
    // unregistered code out. The state field stays `Option` (tests
    // pass `None`); only the binary is fail-loud.
    let classes_client: Option<Arc<dyn ClassesClient>> = Some(Arc::new(ReqwestClassesClient::new(
        cfg.classes_api_url.clone(),
    )) as Arc<dyn ClassesClient>);
    info!(classes_api_url = %cfg.classes_api_url, "Class registry validation enabled");

    let clock_url =
        std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| "http://localhost:7060".to_string());
    let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(clock_url.clone()));
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // every stamp automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    // Share the same Class-registry handle with the marketing-assets
    // sub-router so its `kind` codes validate against
    // `subject_kind='marketing-asset'` exactly as the device-catalog
    // router validates categories + document kinds. Cloned because
    // `KbApiState` takes ownership below.
    let marketing_classes_client = classes_client.clone();

    let state = KbApiState {
        catalog,
        publisher,
        assets_client,
        classes_client,
        clock,
    };
    // Merge the Marketing Asset KB sub-router (session 2 of
    // examples/used-device-shop/design/marketing-needs.md). Postgres-only: marketing_assets
    // is backed by Postgres via PgMarketingAssets, no in-memory
    // fallback since the InMemoryKb is device-catalog specific.
    let app = {
        use boss_catalog::marketing_assets::http::{
            MarketingAssetsApiState, router as marketing_assets_router,
        };
        router(state).merge(marketing_assets_router(MarketingAssetsApiState {
            pool: pg_pool.clone(),
            classes_client: marketing_classes_client,
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
    info!(addr = %http_addr, "kb HTTP API listening");

    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use boss_core::startup::mask_password;

    #[test]
    fn masks_password() {
        assert_eq!(
            mask_password("postgres://boss:secret@localhost/boss"),
            "postgres://boss:***@localhost/boss"
        );
    }

    #[test]
    fn no_password_unchanged() {
        assert_eq!(
            mask_password("postgres://localhost/boss"),
            "postgres://localhost/boss"
        );
    }
}
