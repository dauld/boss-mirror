//! `boss-policy-bootstrap` — CLI shell over
//! [`boss_policy::bootstrap::publish_policy_rules`]. Seeds a tenant's
//! role-grant matrix at first boot by POSTing each rule in
//! `examples/<tenant>/seeds/policy_rules.toml` to `/api/policy/rules`.
//!
//! Core ships only platform rules (`platform-admin` /
//! `audit-readonly` / `smoke-tester` / `guest`) — see
//! `boss-policy::default_rules`. Tenant role grants
//! (ceo / coo / sales-rep / brewer / controller / …) live in
//! tenant seed data and arrive via this binary.
//!
//! The seeding logic lives in the library (`boss_policy::bootstrap`)
//! so the bin and tenant `prepare` steps drive identical code; this
//! binary just resolves the policy-api / gateway base URL and hands
//! off. See that module for the idempotence contract.
//!
//! Usage:
//!   boss-policy-bootstrap --seeds examples/brewery/seeds/policy_rules.toml

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-policy-bootstrap",
    about = "Seed tenant policy rules from a TOML file",
    version
)]
struct Cli {
    /// Path to the tenant's `policy_rules.toml`.
    #[arg(long)]
    seeds: PathBuf,

    /// boss-policy-api base URL. Default pulled from boss_ports —
    /// single source of truth shared with the config generator.
    #[arg(long, default_value_t = boss_ports::url("policy"))]
    policy_base: String,

    /// If set, all per-service base URLs are ignored; the bootstrap
    /// routes through the gateway with the canonical
    /// `/api/policy/*` prefix.
    #[arg(long)]
    gateway_base: Option<String>,

    /// Operator-tier x-boss-user header. Defaults to a
    /// hardcoded platform-bootstrap identity since the binary runs
    /// out-of-band.
    #[arg(long)]
    x_boss_user: Option<String>,

    /// Overwrite existing rules instead of skipping them. Default
    /// is skip-on-conflict so operator tuning survives re-runs.
    /// Use after editing the seed file when the operator wants the
    /// new grants applied to the live registry.
    #[arg(long)]
    force: bool,

    /// `changed_by` value attributed to rows this run upserts.
    /// Convention: `"<tenant>-policy-bootstrap"`. Surfaces in the
    /// `policy_rules.updated_by` column for audit.
    #[arg(long, default_value = "policy-bootstrap")]
    changed_by: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();

    let api_base = cli
        .gateway_base
        .clone()
        .unwrap_or_else(|| cli.policy_base.clone());

    boss_policy::bootstrap::publish_policy_rules(
        &api_base,
        &cli.seeds,
        cli.force,
        &cli.changed_by,
        cli.x_boss_user.as_deref(),
    )
}
