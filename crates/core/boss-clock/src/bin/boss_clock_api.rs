//! `boss-clock-api` — single authority for "what time is it"
//! across BOSS services. Two runtime modes:
//!
//! - **wall** — every `/api/clock/now` returns `Utc::now()`.
//!   Production deploys.
//! - **sim** — `/api/clock/now` returns a formula-derived sim
//!   instant. The formula is:
//!
//!     sim_now = epoch_start + (wall_now − wall_anchor − paused_offset) × warp_factor
//!
//!   Time advances by itself — nothing outside clock-api
//!   writes time. The simulator polls `/now` and emits events
//!   for whatever days have passed since its last read.
//!
//! Mode is set via `BOSS_CLOCK_MODE=wall|sim` (default `wall`).
//! Sim mode also takes a `--postgres-url` so the formula
//! parameters survive restarts.
//!
//! See `boss_clock::types::ClockNow` for the wire shape every
//! consumer reads via `boss-clock-client::ClockClient`.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use boss_clock::http::{ClockApiState, router};
use boss_clock::types::{ClockMode, SimClockParams};
use chrono::Utc;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-clock-api", about = "Boss Clock API", version)]
struct Cli {
    /// HTTP bind address. Defaults to loopback on the canonical clock
    /// port (7060 via boss-ports). `BOSS_CLOCK_HTTP_BIND` widens it
    /// where a deployment declares that it should: the cluster's
    /// `boss-clock-internal` Service targets this port from other
    /// pods (the conductor), and a loopback bind answers it with
    /// connection refused — which is exactly what happened on
    /// 2026-09-05 when the Service landed and the clock did not
    /// (train #211: the conductor kept falling back to wallclock).
    #[arg(long, env = "BOSS_CLOCK_HTTP_BIND", default_value_t = default_bind())]
    http_bind: String,

    /// Postgres URL — required when running in sim mode so the
    /// formula parameters survive restarts. Ignored in wall mode.
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: Option<String>,

    /// Clock mode. Override via `BOSS_CLOCK_MODE` env var.
    #[arg(long, env = "BOSS_CLOCK_MODE", default_value = "wall")]
    mode: String,

    /// Default warp factor for fresh boots — sim-seconds per
    /// wall-second. `8640.0` is brewery playground's 1 sim-day
    /// per 10 wall-seconds. `1.0` is real-time. Backtests use
    /// very large values. Ignored when sim_clock already has
    /// persisted parameters.
    #[arg(long, env = "BOSS_SIM_WARP_FACTOR", default_value_t = 8640.0)]
    sim_warp_factor: f64,
}

fn default_bind() -> String {
    // Loopback by default: the gateway is the sole trust boundary
    // and every deployment co-locates it (SECURITY.md §Deployment
    // trust model). Pass --http-bind to widen deliberately.
    format!("127.0.0.1:{}", boss_ports::prod("clock"))
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

    let mode = match cli.mode.as_str() {
        "wall" => ClockMode::Wall,
        "sim" => ClockMode::Sim,
        other => {
            anyhow::bail!("unknown BOSS_CLOCK_MODE `{other}` — expected `wall` or `sim`");
        }
    };
    info!(?mode, "boss-clock-api starting");

    // Sim mode: prime the formula parameters from sim_clock so
    // the formula stays continuous across restarts. Wall mode
    // doesn't touch Postgres.
    let (params, pool) = match (mode, &cli.postgres_url) {
        (ClockMode::Sim, Some(url)) => {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(url)
                .await
                .with_context(|| "connecting to Postgres")?;
            let params = match boss_clock::postgres::read_params(&pool).await {
                Ok(p) => {
                    info!(
                        epoch_start = %p.epoch_start,
                        warp_factor = p.warp_factor,
                        wall_anchor = %p.wall_anchor,
                        "primed formula params from sim_clock"
                    );
                    Some(p)
                }
                Err(e) => {
                    // Cold start. Try BOSS_SIM_EPOCH_START env
                    // var; otherwise leave None and /now will
                    // log loud warnings until /configure lands.
                    match std::env::var("BOSS_SIM_EPOCH_START")
                        .ok()
                        .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
                    {
                        Some(epoch_start) => {
                            info!(
                                %epoch_start,
                                warp_factor = cli.sim_warp_factor,
                                "sim_clock not yet seeded; auto-priming from BOSS_SIM_EPOCH_START"
                            );
                            let p = SimClockParams {
                                epoch_start,
                                epoch_end: None,
                                warp_factor: cli.sim_warp_factor,
                                wall_anchor: Utc::now(),
                                paused: false,
                                paused_at: None,
                                paused_offset_seconds: 0.0,
                                restart_in_progress: false,
                            };
                            if let Err(e) = boss_clock::postgres::write_params(&pool, &p).await {
                                tracing::warn!(
                                    error = %e,
                                    "couldn't persist seeded params to sim_clock — restart will re-read env"
                                );
                            }
                            Some(p)
                        }
                        None => {
                            tracing::warn!(
                                error = %e,
                                "sim_clock not yet seeded — waiting for first /configure \
                                 (or set BOSS_SIM_EPOCH_START env to auto-seed at boot)"
                            );
                            None
                        }
                    }
                }
            };
            (Arc::new(RwLock::new(params)), Some(pool))
        }
        (ClockMode::Sim, None) => {
            tracing::warn!(
                "sim mode but no --postgres-url; formula parameters will not survive restart"
            );
            (Arc::new(RwLock::new(None)), None)
        }
        (ClockMode::Wall, _) => (Arc::new(RwLock::new(None)), None),
    };

    let state = ClockApiState { mode, params, pool };

    // Sim mode with a pool: the DB `sim_clock` row is the source of
    // truth, and external processes (notably the demo-loop Reset in
    // boss-jobs) rewind it with a direct SQL UPDATE that never goes
    // through this service's write endpoints. Refresh the in-memory
    // formula params from the DB on a short cadence so those external
    // rewinds (and any DB-side pause/resume) show up in `/now` within
    // a couple seconds instead of requiring a process restart.
    if let Some(pool) = state.pool.clone() {
        boss_clock::http::spawn_db_refresher(
            pool,
            Arc::clone(&state.params),
            std::time::Duration::from_secs(2),
        );
        info!("sim_clock DB refresher running (2s cadence)");
    }

    let app = router(state);

    let bind: SocketAddr = cli
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cli.http_bind))?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding HTTP listener on {bind}"))?;
    info!(addr = %bind, "boss-clock-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_defaults_to_loopback_and_the_env_widens_it() {
        // Loopback unless a deployment says otherwise — the trust
        // model's default, unchanged.
        let cli = Cli::try_parse_from(["boss-clock-api"]).unwrap();
        assert_eq!(
            cli.http_bind,
            format!("127.0.0.1:{}", boss_ports::prod("clock"))
        );

        // The cluster declares the wider bind in its manifest, as an
        // env var, so the Service that targets this port is answered.
        // SAFETY: tests in this binary do not share this variable.
        unsafe { std::env::set_var("BOSS_CLOCK_HTTP_BIND", "0.0.0.0:7060") };
        let cli = Cli::try_parse_from(["boss-clock-api"]).unwrap();
        unsafe { std::env::remove_var("BOSS_CLOCK_HTTP_BIND") };
        assert_eq!(cli.http_bind, "0.0.0.0:7060");

        // The flag still wins over the env — an operator's explicit
        // word beats a deployment default.
        unsafe { std::env::set_var("BOSS_CLOCK_HTTP_BIND", "0.0.0.0:7060") };
        let cli = Cli::try_parse_from(["boss-clock-api", "--http-bind", "127.0.0.1:1"]).unwrap();
        unsafe { std::env::remove_var("BOSS_CLOCK_HTTP_BIND") };
        assert_eq!(cli.http_bind, "127.0.0.1:1");
    }
}
