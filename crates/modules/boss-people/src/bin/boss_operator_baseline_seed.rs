//! `boss-operator-baseline-seed` — POST the operator-baseline
//! (system audit account + bootstrap admin) to the people-api at
//! `POST /api/people`, idempotently.
//!
//! All data loading goes through the public API: this binary no
//! longer writes `people.employee.created` rows into `audit_log`
//! directly. Each hire is POSTed to the people-api, which runs the
//! policy check + validation and emits the event itself through the
//! service-mediated path — the same pipeline every other employee
//! lands through.
//!
//! Event time is clock-authoritative: the people-api stamps each
//! event from clock-api, so this binary sets no timestamp. The
//! caller primes the clock (e.g. via `/api/clock/configure`) before
//! running this.
//!
//! Source TOML lives at `infra/operator-baseline/operator_hires.toml`
//! (system-level concern, not tenant data).
//!
//! Idempotence: a 409 Conflict on a duplicate `id` is treated as
//! "already exists → skip". Safe to run twice; safe to run before or
//! after a tenant publish — since backlog 0d2d7daa (2026-09-16) the
//! bootstrap-admin injection asks the roster who holds the email
//! before injecting, and since backlog 1ee28274 (2026-09-18) it reads
//! the tenant's declared roster file (`--tenant-dir` / BOSS_TENANT_DIR,
//! `seeds/employees.json`) before that, so a tenant that declares the
//! operator as one of its own people is not given a second row even
//! when the baseline runs BEFORE the publish — which it must, being
//! what puts the first platform-admin on an example instance.
//!
//! The logic lives in `boss_people::operator_baseline` so the seed
//! path can be proven against a real people router over a TestDb;
//! this file is argv + tracing + one call.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-operator-baseline-seed",
    about = "Seed operator-baseline employees via the people-api POST /api/people",
    version
)]
struct Cli {
    /// People-API base URL. Defaults to `boss_ports::url("people")`.
    #[arg(long, default_value_t = boss_ports::url("people"))]
    people_base: String,

    /// TOML file describing the operator hires. Defaults to the
    /// in-tree `infra/operator-baseline/operator_hires.toml`.
    #[arg(long, default_value = "infra/operator-baseline/operator_hires.toml")]
    seed_path: PathBuf,

    /// The tenant directory whose `seeds/employees.json` the publish
    /// is about to send (BOSS_TENANT_DIR when unset). When that roster
    /// declares the bootstrap email, the injection is skipped and the
    /// publish lands the row (backlog 1ee28274). Unset, or a directory
    /// with no roster file: the live roster alone decides.
    #[arg(long, env = "BOSS_TENANT_DIR")]
    tenant_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();
    boss_people::operator_baseline::seed(
        &cli.people_base,
        &cli.seed_path,
        cli.tenant_dir.as_deref(),
    )?;
    Ok(())
}
