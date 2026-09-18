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
//!
//! STATIONS RIDE THE SAME SEED (backlog 393d3234, consolidation H4,
//! 2026-09-18). The platform station bundle lives BESIDE the Workflow
//! bundle — `infra/platform/stations/`, found as the sibling of
//! `--seed-path` — and is published after the workflows by
//! `boss_jobs::station_seed::seed_stations`: insert-if-missing by
//! (name, version), a version bump the edit path, and a bundle row
//! that differs from the live active row of the same (name, version)
//! a REFUSAL that names the field. Deriving the directory rather than
//! adding a flag every launcher must learn is what let bootstrap-db.sh,
//! the quickstart's init.sh and the image go untouched;
//! `--stations-path` exists for a bundle that lives elsewhere. The
//! binary keeps its name because two launchers and a bootstrap invoke
//! it by name.
//!
//! STEP PLUGINS RIDE IT TOO (car 2 of the same packet). The row that
//! names a plugin's JS bundle lives in `infra/platform/step-plugins/`
//! — the sibling of `--seed-path` again, `--step-plugins-path` to
//! override — and is published after the stations by
//! `boss_jobs::step_plugin_seed::seed_step_plugins`, the same decision
//! table (`boss_jobs::bundle_seed`). The JS itself still reaches the
//! cluster as the step-plugins ConfigMap the converge runner builds
//! from `infra/step-plugins/*.js`; this binary publishes the ROW.
//!
//! CADENCE RULES RIDE IT TOO (car 3). The conductor's schedule —
//! `infra/platform/cadence/`, the sibling again, `--cadence-path` to
//! override — is published after the step plugins by
//! `boss_jobs::cadence_seed::seed_cadence_rules`, the same table. The
//! difference this registry carries: it is live-editable by design, so
//! a row an operator re-versioned live is reported as ahead of its
//! file and left alone, and a rule the operator retired stays retired.
//!
//! THE DELIVERY POLICY RIDES IT LAST (car 4, the last registry). What
//! the train conductor decides by — `infra/platform/delivery-policy/`,
//! the sibling again, `--delivery-policy-path` to override — is
//! published after the cadence rules by
//! `boss_jobs::delivery_policy_seed::seed_delivery_policies`, the same
//! table. One row is the whole policy and a train pins the version it
//! departed under, so a version bump here changes the rules for the
//! NEXT boarding and never for a train in flight.

use anyhow::{Context, Result};
use boss_core::actor::ActorId;
use boss_jobs::cadence::{CadenceRuleSpec, PgCadence};
use boss_jobs::cadence_seed::{cadence_beside, seed_cadence_rules};
use boss_jobs::delivery::{DeliveryPolicySpec, PgDeliveryPolicy};
use boss_jobs::delivery_policy_seed::{delivery_policy_beside, seed_delivery_policies};
use boss_jobs::registry::PgWorkflows;
use boss_jobs::seed_loader::{
    SeedLoaderError, load_cadence_rules, load_delivery_policies, load_stations, load_step_plugins,
    load_workflows,
};
use boss_jobs::station_seed::{seed_stations, stations_beside};
use boss_jobs::step_plugin_seed::{seed_step_plugins, step_plugins_beside};
use boss_jobs::workflow_seed::seed_workflows;
use boss_jobs::{PgStations, PgStepPlugins, StationSpec, StepPluginSpec};
use clap::Parser;
use std::path::{Path, PathBuf};

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

    /// The station bundle: a directory of `<name>.toml` files.
    /// Defaults to the `stations` directory BESIDE `--seed-path`
    /// (`infra/platform/stations` for the in-tree default), so the
    /// launchers that already pass `--seed-path` needed no edit.
    #[arg(long)]
    stations_path: Option<PathBuf>,

    /// The step-plugin bundle: a directory of `<kind>.toml` files.
    /// Defaults to the `step-plugins` directory BESIDE `--seed-path`
    /// (`infra/platform/step-plugins` for the in-tree default).
    #[arg(long)]
    step_plugins_path: Option<PathBuf>,

    /// The cadence bundle: a directory of `<name>.toml` files.
    /// Defaults to the `cadence` directory BESIDE `--seed-path`
    /// (`infra/platform/cadence` for the in-tree default).
    #[arg(long)]
    cadence_path: Option<PathBuf>,

    /// The delivery-policy bundle: a directory of `<name>.toml` files.
    /// Defaults to the `delivery-policy` directory BESIDE `--seed-path`
    /// (`infra/platform/delivery-policy` for the in-tree default).
    #[arg(long)]
    delivery_policy_path: Option<PathBuf>,

    /// Report what would be inserted and write nothing.
    #[arg(long)]
    dry_run: bool,
}

/// Where a sibling bundle is, and that it is there. A directory
/// `--seed-path` names has its siblings REQUIRED: the image copies
/// infra/platform whole, so a missing `stations/` or `step-plugins/`
/// beside a present `workflows/` is a packaging fault, and a packaging
/// fault must read like one rather than as a seed that quietly did
/// less (the three silences bootstrap-db.sh records). A single bundle
/// FILE has no sibling to derive, so a sibling bundle is published
/// only when its `--<name>-path` names it, and the binary says so.
fn load_sibling_bundle<T>(
    cli: &Cli,
    label: &str,
    name: &str,
    flag: &str,
    override_path: &Option<PathBuf>,
    beside: fn(&Path) -> PathBuf,
    load: fn(&Path) -> std::result::Result<Vec<T>, SeedLoaderError>,
) -> Result<Option<(PathBuf, Vec<T>)>> {
    let dir = match override_path {
        Some(p) => p.clone(),
        None if cli.seed_path.is_dir() => beside(&cli.seed_path),
        None => {
            println!(
                "{label}: --seed-path is a file, so no {name} bundle sits beside it; \
                 pass {flag} to publish {name}"
            );
            return Ok(None);
        }
    };
    if !dir.is_dir() {
        anyhow::bail!(
            "{label}: {name} bundle NOT FOUND at {} — the platform {name} bundle is \
             published from the `{name}` directory beside the Workflow bundle \
             (infra/platform/{name}/ in the tree, copied into the image with \
             infra/platform/). A missing bundle is a packaging fault: every platform \
             row of this registry would be absent from this deployment.",
            dir.display()
        );
    }
    let specs = load(&dir).with_context(|| format!("reading {}", dir.display()))?;
    Ok(Some((dir, specs)))
}

fn load_station_bundle(cli: &Cli) -> Result<Option<(PathBuf, Vec<StationSpec>)>> {
    load_sibling_bundle(
        cli,
        "platform-station-seed",
        "stations",
        "--stations-path",
        &cli.stations_path,
        stations_beside,
        |dir| load_stations(dir),
    )
}

fn load_step_plugin_bundle(cli: &Cli) -> Result<Option<(PathBuf, Vec<StepPluginSpec>)>> {
    load_sibling_bundle(
        cli,
        "platform-step-plugin-seed",
        "step-plugins",
        "--step-plugins-path",
        &cli.step_plugins_path,
        step_plugins_beside,
        |dir| load_step_plugins(dir),
    )
}

fn load_cadence_bundle(cli: &Cli) -> Result<Option<(PathBuf, Vec<CadenceRuleSpec>)>> {
    load_sibling_bundle(
        cli,
        "platform-cadence-seed",
        "cadence",
        "--cadence-path",
        &cli.cadence_path,
        cadence_beside,
        |dir| load_cadence_rules(dir),
    )
}

fn load_delivery_policy_bundle(cli: &Cli) -> Result<Option<(PathBuf, Vec<DeliveryPolicySpec>)>> {
    load_sibling_bundle(
        cli,
        "platform-delivery-policy-seed",
        "delivery-policy",
        "--delivery-policy-path",
        &cli.delivery_policy_path,
        delivery_policy_beside,
        |dir| load_delivery_policies(dir),
    )
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
    // Read EVERY bundle before touching the database: a station bundle
    // that does not parse refuses the whole run before any workflow is
    // published, so a half-seeded deployment is not a state this binary
    // can leave behind.
    let stations = load_station_bundle(&cli)?;
    let step_plugins = load_step_plugin_bundle(&cli)?;
    let cadence = load_cadence_bundle(&cli)?;
    let delivery_policy = load_delivery_policy_bundle(&cli)?;
    if specs.is_empty() {
        println!("platform-workflow-seed: bundle is empty, nothing to do");
    }

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.database_url)
        .await
        .context("connecting to Postgres")?;
    let registry = PgWorkflows::new(pool.clone());
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

    // Present means ANY version — the decision is `bundle_seed`'s, the
    // same table the stations and step plugins below read. Until
    // 2026-09-18 (backlog 8b2eaff2) it was an inline loop here that
    // read "present" as "an active row exists", and re-published the
    // kind an operator had retired twenty-two minutes earlier.
    if !specs.is_empty() {
        let report = seed_workflows(&registry, &specs, &actor, now, cli.dry_run)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("{report}");
    }

    // A refusal is collected for every row before anything is written,
    // and it is the boot's to see: the row the tree declares and the
    // row the deployment runs disagree, and only a version bump in the
    // tree resolves that. A refused registry stops the run here; the
    // rows already published stand.
    if let Some((dir, station_specs)) = stations {
        if station_specs.is_empty() {
            println!(
                "platform-station-seed: bundle at {} is empty",
                dir.display()
            );
        } else {
            let registry = PgStations::new(pool.clone());
            let report = seed_stations(&registry, &station_specs, &actor, now, cli.dry_run)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{report}");
        }
    }

    if let Some((dir, plugin_specs)) = step_plugins {
        if plugin_specs.is_empty() {
            println!(
                "platform-step-plugin-seed: bundle at {} is empty",
                dir.display()
            );
        } else {
            let registry = PgStepPlugins::new(pool.clone());
            let report = seed_step_plugins(&registry, &plugin_specs, &actor, now, cli.dry_run)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{report}");
        }
    }

    if let Some((dir, cadence_specs)) = cadence {
        if cadence_specs.is_empty() {
            println!(
                "platform-cadence-seed: bundle at {} is empty",
                dir.display()
            );
        } else {
            let registry = PgCadence::new(pool.clone());
            let report = seed_cadence_rules(&registry, &cadence_specs, &actor, now, cli.dry_run)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{report}");
        }
    }

    if let Some((dir, policy_specs)) = delivery_policy {
        if policy_specs.is_empty() {
            println!(
                "platform-delivery-policy-seed: bundle at {} is empty",
                dir.display()
            );
        } else {
            let registry = PgDeliveryPolicy::new(pool);
            let report = seed_delivery_policies(&registry, &policy_specs, &actor, now, cli.dry_run)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{report}");
        }
    }
    Ok(())
}
