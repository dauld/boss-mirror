//! `boss-jobs-rebuild` — drop the `jobs` + `steps` projections and
//! reconstruct them from `audit_log` alone.
//!
//! See `docs/design/projection-rebuilders.md`.
//!
//! Exit codes:
//! - `0` — rebuild succeeded
//! - other — operational error

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_jobs::jobs_config::JobsApiConfig;
use boss_jobs::rebuild_jobs_and_steps;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-jobs-rebuild",
    about = "Rebuild the jobs + steps projections from audit_log",
    version
)]
struct Cli {
    /// Postgres connection string. If omitted, falls back to the
    /// `postgres_url` in the jobs-api config or DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,

    /// Path to a jobs-api config file (default
    /// `/etc/boss-jobs-api.toml`). Used only to discover the
    /// postgres URL when `--database-url` isn't passed.
    #[arg(short, long, default_value = "/etc/boss-jobs-api.toml")]
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
    // jobs-api config carries `postgres_url` as Option<String>, so the
    // extraction flattens one extra level vs the String-typed configs.
    let config_url = (cli.config.exists())
        .then(|| JobsApiConfig::load(&cli.config).ok())
        .flatten()
        .and_then(|cfg| cfg.postgres_url);
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["DATABASE_URL"],
        "pass --database-url, point --config at a valid \
         boss-jobs-api.toml, or set DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = rebuild_jobs_and_steps(&pool)
        .await
        .with_context(|| "rebuilding jobs + steps projections")?;

    // The report's own Display names every counter (backlog 25590ca4):
    // this line listed six by hand and dropped `stamps_voided` and
    // `sign_offs_unreproduced`, the one that says the replay differs.
    info!(report = %report, "jobs + steps rebuild complete");
    if report.sign_offs_unreproduced > 0 {
        warn!(
            sign_offs_unreproduced = report.sign_offs_unreproduced,
            "the rebuilt steps lack sign-off stamps the log records only as markers; see the per-marker warnings above"
        );
    }
    Ok(())
}
