//! `boss-used-device-shop-engine` — the used-device-shop tenant's
//! CLI. Mirrors the brewery's converged one-tool shape
//! (`boss-brewery-sim`), with the one subcommand this tenant needs
//! today:
//!
//! - `prepare` — seed the whole tenant model (classes → company
//!   identity → policy → roster → catalog → Workflows) through the
//!   public API against a running stack, then exit. Idempotent.
//!   Env-driven, matching `boss-brewery-sim prepare`:
//!   `BOSS_SIM_SEEDS_DIR` points at the seed bundle (default
//!   `/opt/boss/examples/used-device-shop/seeds`);
//!   `BOSS_SIM_PREPARE_GATEWAY` routes every `/api/*` call through
//!   one gateway URL instead of per-service localhost ports.
//!
//! `infra/bootstrap-vm.sh`'s `TENANT=device-shop` branch calls this
//! after deploying the service stack — that is the tenant's install
//! story. There is no daemon mode: unlike the brewery this tenant
//! ships no live sim service; after prepare, work is driven by human
//! and agent actors through the normal surfaces (the engine library
//! drives the day-loop in tests and offline runs).

use std::path::PathBuf;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let seeds_dir = std::env::var("BOSS_SIM_SEEDS_DIR")
        .unwrap_or_else(|_| "/opt/boss/examples/used-device-shop/seeds".to_string());
    let seeds = PathBuf::from(&seeds_dir);

    match std::env::args().nth(1).as_deref() {
        Some("prepare") => {
            let gateway = std::env::var("BOSS_SIM_PREPARE_GATEWAY").ok();
            info!(seeds = %seeds_dir, gateway = ?gateway, "boss-used-device-shop-engine prepare");
            boss_used_device_shop_engine::prepare::prepare_model(gateway.as_deref(), &seeds)
        }
        other => {
            eprintln!("usage: boss-used-device-shop-engine prepare   (got {other:?})");
            eprintln!(
                "  BOSS_SIM_SEEDS_DIR         seed bundle (default /opt/boss/examples/used-device-shop/seeds)"
            );
            eprintln!("  BOSS_SIM_PREPARE_GATEWAY   route all calls through one gateway URL");
            std::process::exit(2);
        }
    }
}
