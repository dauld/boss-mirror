//! `boss-ledger-recognize` — ASC 606 revenue-recognition scheduler.
//!
//! Invoked by a systemd timer (see `infra/boss-ledger-recognize.timer`).
//! One tick: find every `revenue_schedules` row whose
//! `next_recognition_date` is on or before today, post a
//! `finance.revenue.recognized` fact per schedule (RuleSet v2 then
//! projects DR 2200 / CR revenue-account), advance the cursor, flip
//! to `status='closed'` when the schedule is fully recognized.
//!
//! Idempotent: re-running the same day is a no-op because the cursor
//! has already advanced past `today`, and the per-period fact is
//! uniquely indexed on `(kind, source_table, source_id)` inside a
//! single-day retry.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::recognize;
use chrono::Utc;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-ledger-recognize",
    about = "Run one tick of the ASC 606 revenue-recognition scheduler",
    version
)]
struct Cli {
    /// Config path. Shares `boss-ledger-api.toml` since the DB URL
    /// is identical — the scheduler just reads the same pool.
    #[arg(short, long, default_value = "/etc/boss-ledger-api.toml")]
    config: PathBuf,

    /// Override today's date (YYYY-MM-DD). Useful for back-filling a
    /// missed day or for one-off ops runs; defaults to the system
    /// clock.
    #[arg(long)]
    today: Option<chrono::NaiveDate>,
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

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            let p = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "ledger")
                .with_audit(Arc::new(boss_events::PgAuditWriter::new(pool.clone())));
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(Arc::new(p))
        }
        None => {
            info!("no nats_url configured — recognize events will not be published");
            None
        }
    };

    let today = cli.today.unwrap_or_else(|| Utc::now().date_naive());
    info!(%today, "running revenue-recognition tick");

    let summary = recognize::run_tick(&pool, &publisher, today)
        .await
        .with_context(|| "revenue recognition tick failed")?;

    info!(
        considered = summary.schedules_considered,
        posted = summary.periods_posted,
        closed = summary.schedules_closed,
        locked_skips = summary.locked_skips,
        errors = summary.errors.len(),
        "tick done"
    );
    // What the run DID, for the packet — not only that it ran
    // (backlog 18d6a6c9): boss-step.sh merges this onto the `run` step
    // so a tick over an empty table reads `recognized=0`, distinguishable
    // from one that posted a year. Unset path = hand run = no file.
    let summary_path = std::env::var_os("BOSS_RUN_SUMMARY_FILE").map(PathBuf::from);
    if let Err(e) = recognize::write_run_summary(summary_path.as_deref(), &summary) {
        // Visibility is never a precondition (infra/run-summary.sh): the
        // recognition itself succeeded, so say so and keep the verdict.
        error!(error = %e, "could not write the run summary; the packet will carry result alone");
    }

    // Exit non-zero on any per-schedule error so systemd surfaces the
    // degraded state via `systemctl is-failed`. Individual errors are
    // logged above; this keeps the timer-level alert paths honest.
    if !summary.errors.is_empty() {
        for e in &summary.errors {
            error!("schedule error: {e}");
        }
        std::process::exit(2);
    }
    Ok(())
}
