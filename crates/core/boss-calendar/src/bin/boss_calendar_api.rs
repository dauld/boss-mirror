//! `boss-calendar-api` — production HTTP service for the calendar
//! primitive. Lives behind the gateway at `/api/calendar/*`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar::{CalendarApiConfig, CalendarApiState, CalendarClient, router};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-calendar-api",
    about = "Boss Calendar API service",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-calendar-api.toml")]
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
    let cfg = CalendarApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-calendar-api starting");

    let (calendar, publisher): (
        Arc<dyn CalendarClient>,
        Option<boss_core::publisher::DomainPublisher>,
    ) = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        let calendar: Arc<dyn CalendarClient> =
            Arc::new(boss_calendar::PgCalendar::new(pool.clone()));
        let publisher = match &cfg.nats_url {
            Some(url) => {
                let bus = boss_nats::NatsEventBus::connect(url)
                    .await
                    .with_context(|| format!("connecting to NATS at {url}"))?;
                let pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "calendar")
                    .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool)));
                info!(nats_url = %url, "domain event publishing + audit trail enabled");
                Some(pub_)
            }
            None => {
                info!("no nats_url configured — calendar events will not be published");
                None
            }
        };
        (calendar, publisher)
    };

    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every stamp
    // injects `_simulated: bool` into the audit_log payload without
    // per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    let state = CalendarApiState {
        calendar,
        publisher,
        clock,
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
    info!(addr = %http_addr, "calendar HTTP API listening");
    axum::serve(listener, app).await?;
    Ok(())
}
