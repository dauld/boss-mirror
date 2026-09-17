//! `boss-ledger-api` — HTTP surface over the GL projection, plus the
//! live fact subscriber.
//!
//! Posting happens inside the domain write transactions (boss-commerce +
//! boss-inventory call `boss_ledger::post_fact_in_tx`). This binary is for
//! queries: chart, trial balance, entry lookups for drill-down — and,
//! since backlog 5621d166, for the facts whose ONLY writer is the
//! `gl_fact_projection_rules` registry: a durable consumer on the
//! platform event stream (`boss_ledger::live_facts`) projects and posts
//! each such event as it lands, so the journal follows the work within
//! seconds instead of at the next facts rebuild.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::http::{LedgerApiState, router};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-ledger-api", about = "Boss Ledger API", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-ledger-api.toml")]
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
    let cfg = LedgerApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        "boss-ledger-api starting"
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    // Seed the executive-role cache from the Class registry so the
    // IT-providers gate's `has_global_read` recognises tenant-defined
    // executives. Skip on missing config or transport failure —
    // platform-admin + audit-readonly still grant global read.
    if let Some(url) = &cfg.classes_api_url {
        let client = boss_classes_client::ReqwestClassesClient::new(url.clone());
        match boss_classes_client::seed_executive_role_cache(&client).await {
            Ok(n) => info!(count = n, classes_api_url = %url, "executive role cache seeded"),
            Err(e) => {
                tracing::warn!(error = %e, "failed to seed executive roles from classes; falling back to platform-admin/audit-readonly only")
            }
        }
    } else {
        info!("classes_api_url unset; executive role cache disabled");
    }

    // Clock-api URL: env override (BOSS_CLOCK_URL) takes
    // precedence; default goes to the canonical port via
    // boss-ports. Production deploys point at a wall-mode
    // clock-api; demo deploys point at the sim-mode clock-api.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // One NATS connection: the publisher's bus is also the bus the live
    // fact subscriber binds its durable consumer through.
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (publisher, live_facts_task) = match &cfg.nats_url {
        Some(url) => {
            let bus = Arc::new(
                boss_nats::NatsEventBus::connect(url)
                    .await
                    .with_context(|| format!("connecting to NATS at {url}"))?,
            );
            let p = boss_core::publisher::DomainPublisher::new(bus.clone(), "ledger")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())))
                .with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
                    clock.clone(),
                )));
            info!(nats_url = %url, "domain event publishing + audit trail enabled (with sim probe)");
            let task = tokio::spawn(boss_ledger::live_facts::run(
                bus,
                pool.clone(),
                cancel_rx.clone(),
            ));
            (Some(Arc::new(p)), Some(task))
        }
        None => {
            info!(
                "no nats_url configured — ledger events will not be published and \
                 registry-only facts reach the ledger at the next facts rebuild"
            );
            (None, None)
        }
    };

    let state = LedgerApiState {
        pool: pool.clone(),
        publisher,
        clock,
        // Read gate on the whole surface. Same wrapping the other
        // policy consumers use: sim traffic authorized at the
        // boundary, real traffic enforced per-role.
        policy: Some(Arc::new(boss_policy_client::SimBypassPolicyClient::new(
            Arc::new(boss_policy_client::ReqwestPolicyClient::new(
                std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy")),
            )),
        ))),
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
    info!(addr = %http_addr, "boss-ledger-api listening");
    let mut http_cancel = cancel_rx.clone();
    let shutdown = async move {
        let _ = http_cancel.changed().await;
    };
    let http_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            tracing::error!(error = %e, "HTTP server exited with error");
        }
    });

    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);
    let _ = http_task.await;
    if let Some(task) = live_facts_task {
        let _ = task.await;
    }
    info!("boss-ledger-api shut down cleanly");
    Ok(())
}
