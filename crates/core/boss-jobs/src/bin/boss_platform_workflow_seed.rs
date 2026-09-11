//! `boss-platform-workflow-seed` — load the platform Workflow bundle
//! into the registry, inserting only what is missing.
//!
//! David, 2026-08-15: "get those protocols prioritized to be fully
//! moved into data and registry configurations. That is hunting
//! leakage between the layers too." Three of the four registries
//! carrying the operating model already seed as data; `workflows` did
//! not, and a kind that lives as a Rust literal cannot be changed
//! without a deploy — CLAUDE.md's own definition of a protocol that
//! has leaked into the substrate.
//!
//! INSERT-IF-MISSING, AND NOTHING ELSE. This is protocols-as-data Q1,
//! as David answered it: "the seed binary inserts what is missing and
//! touches nothing that exists — the same idempotent posture
//! boss-operator-baseline-seed already has ... Drift-healing goes away
//! deliberately: it is the feature that reverts operator edits."
//!
//! So this deliberately does NOT reconcile. `bootstrap_reconcile`
//! republishes the code default over any bootstrap-owned row whose body
//! drifted, which is exactly how two protocol edits were silently
//! undone on 2026-08-14 (68331085). A kind that has moved to the bundle
//! is out of `platform_workflows()` and therefore out of reconcile's
//! reach; this binary gives it a first version and then leaves it
//! alone forever.
//!
//! Publishing goes through `create_draft` + `publish` rather than an
//! INSERT, so the viability lint runs on every row exactly as it would
//! for a workflow authored in the UI. A malformed bundle fails here,
//! loudly, on the deployment that is booting — not later, on the first
//! Job that tries to use it.

use anyhow::{Context, Result};
use boss_core::actor::ActorId;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry};
use boss_jobs::seed_loader::load_workflows;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "boss-platform-workflow-seed",
    about = "Insert missing platform Workflow rows from the in-tree bundle",
    version
)]
struct Cli {
    /// Postgres URL for the deployment being seeded.
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    database_url: String,

    /// The bundle: a directory of `<kind>.toml` files (or one bundle
    /// file). Defaults to the in-tree platform bundle.
    #[arg(long, default_value = "infra/platform/workflows")]
    seed_path: PathBuf,

    /// Report what would be inserted and write nothing.
    #[arg(long)]
    dry_run: bool,
}

/// Who the platform seed publishes as.
///
/// Machine-shaped on purpose. It is not `bootstrap`: that string is
/// reconcile's discriminator for "the platform owns this row and may
/// rewrite it", and a bundled kind is precisely one nothing should
/// rewrite. Naming the seed also means a row's provenance survives —
/// `created_by` answers "who put this here" for a bundle row the same
/// way it now does for an operator's edit.
const SEED_ACTOR: &str = "automation:platform-workflow-seed";

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let specs = load_workflows(&cli.seed_path)
        .with_context(|| format!("reading {}", cli.seed_path.display()))?;
    if specs.is_empty() {
        println!("platform-workflow-seed: bundle is empty, nothing to do");
        return Ok(());
    }

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.database_url)
        .await
        .context("connecting to Postgres")?;
    let registry = PgWorkflows::new(pool);
    let actor = ActorId::Automation(SEED_ACTOR.trim_start_matches("automation:").to_string());
    // Bootstrap runs before the clock-api is necessarily up, so this
    // takes the wall client explicitly rather than reaching for
    // `Utc::now()` — which infra/lint/no-wallclock.sh forbids outside
    // the clock crate, for the good reason that a stray wallclock stamp
    // in a sim-mode deployment is invisible until someone reads the
    // audit log months later. A seed row's created_at is the one
    // timestamp where wall time is honest: it records when the
    // deployment was built, not when its work happened.
    let clock: std::sync::Arc<dyn boss_clock_client::ClockClient> =
        std::sync::Arc::new(boss_clock_client::WallClockClient);
    let now = boss_clock_client::now_from(&clock).await;

    let (mut inserted, mut present) = (0usize, 0usize);
    for spec in specs {
        let kind = spec.kind.clone();
        // Present means present. Any active row for this kind — a
        // version an operator published, or one an earlier run of this
        // binary inserted — is left exactly as it is.
        if registry.get_active(&kind).await.is_ok() {
            present += 1;
            println!("  {kind}: already present, untouched");
            continue;
        }
        if cli.dry_run {
            inserted += 1;
            println!("  {kind}: WOULD insert (dry run)");
            continue;
        }
        registry
            .create_draft(spec, &actor, now)
            .await
            .with_context(|| format!("drafting {kind}"))?;
        registry
            .publish(&kind, &actor, now)
            .await
            .with_context(|| format!("publishing {kind} — the viability lint refused it"))?;
        inserted += 1;
        println!("  {kind}: inserted");
    }
    println!("platform-workflow-seed: {inserted} inserted, {present} already present");
    Ok(())
}
