//! Used-device-shop operating-engine library — sister to
//! `boss-brewery-engine`.
//!
//! Loads the used-device-shop seed bundle (`tenant.toml` +
//! `workflows.toml` + `data/employees.json`), seeds the subject
//! pool, and drives the day-loop via
//! `boss_sim::engines::run_ticks_with_handlers`. Side-effect
//! dispatch lives in the boss-dispatcher daemon's rule registry; the
//! engine just emits step.done.<kind> via PUT to jobs-api which the
//! dispatcher routes.
//!
//! Two entry points mirror brewery-engine's public surface:
//!
//! - [`run_used_device_shop`] — convenience wrapper around an
//!   `InMemoryOutput` for tests + assertion-style use.
//! - [`run_used_device_shop_into`] — generic-output variant for
//!   running against a `LiveApiOutput` pointed at a real API
//!   stack.

pub mod prepare;

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;

use boss_jobs::registry::WorkflowSpec;
use boss_jobs::seed_loader::load_workflows_with_owning_team;
use boss_jobs::step_registry::StepRegistry;
use boss_sim::calendar::CalendarRegistry;
use boss_sim::engines::{
    CounterpartyEngine, PeriodicEngine, RunReport, SimEventBus, end_of_day_rollup,
    run_one_tick_with_handlers, run_ticks_with_handlers,
};
use boss_sim::output::{InMemoryOutput, SimOutput};
use boss_sim::rng::Rng;
use boss_sim::shape_driven::{ShapeDrivenState, TenantConfig};

/// Result of a used-device-shop run — the day-loop's RunReport
/// plus the populated InMemoryOutput so callers can assert on
/// emitted facts.
pub struct UsedDeviceShopRunResult {
    pub report: RunReport,
    pub output: InMemoryOutput,
}

/// Seed the canonical used-device-shop subjects so the Workflows have
/// targets to anchor on. Production deployments derive this from
/// live tables; the standalone runner / test path uses this hand-seed
/// PLUS the per-day subject birth driven by `[subject_rates]` in
/// `tenant.toml`. This bootstrap pool is small on purpose — most
/// growth comes from the birth path so the demo's "install base grows over
/// time" arc is visible.
///
/// Seeds:
/// - 5 locations (HQ + 4 regional warehouses)
/// - 1 vendor (`vnd-spares-distributor`, the spares-supplier
///   counterparty target)
/// - 50 starter accounts so the early days have draw targets before
///   `[subject_rates.account]` ramps the population
/// - Every active employee from `examples/used-device-shop/data/employees.json`,
///   registered both as a `subject_kind="employee"` Subject AND as
///   a role-keyed actor pool so `advance_steps` can pick a real
///   Employee for each step transition (sim-fidelity).
///
/// The employees path defaults to the in-tree
/// `examples/used-device-shop/data/employees.json`; pass an override
/// for non-default deploy layouts.
pub fn seed_used_device_shop_subjects(
    state: &mut ShapeDrivenState,
    employees_json_path: Option<&Path>,
) {
    state.seed_subject("location", "loc-hq");
    state.seed_subject("location", "loc-warehouse-east");
    state.seed_subject("location", "loc-warehouse-west");
    state.seed_subject("location", "loc-warehouse-central");
    state.seed_subject("location", "loc-warehouse-south");
    state.seed_subject("vendor", "vnd-spares-distributor");
    for i in 0..50 {
        state.seed_subject("account", &format!("acc-{i:04}"));
    }

    let default_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/used-device-shop/data/employees.json");
    let path = employees_json_path.unwrap_or(default_path.as_path());
    if let Ok(bytes) = std::fs::read(path)
        && let Ok(roster) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
    {
        for emp in roster {
            let id = emp.get("id").and_then(|v| v.as_str());
            let role = emp.get("role").and_then(|v| v.as_str());
            let status = emp
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active");
            if status != "active" {
                continue;
            }
            if let (Some(id), Some(role)) = (id, role) {
                state.register_employee(role, id);
                state.seed_subject("employee", id);
            }
        }
    }
}

/// Mutable engine state a long-running daemon holds across ticks.
/// The in-flight pending state of every CounterpartyEngine chain
/// (bank-ach 30bd delay, ar-aging 30bd delay, RMA inbound 3-stage
/// scans, etc.) must survive tick boundaries; at 1-day chunks every
/// chained delay would otherwise be lost on each chunk.
///
/// Construct via [`UsedDeviceShopEngineState::load`] once, then call
/// [`run_used_device_shop_one_day`] in a loop to advance day-by-day
/// without losing pending-action state.
pub struct UsedDeviceShopEngineState {
    pub kinds: Vec<WorkflowSpec>,
    pub registry: StepRegistry,
    pub tenant: TenantConfig,
    pub state: ShapeDrivenState,
    pub rng: Rng,
    pub periodic: PeriodicEngine,
    pub counterparty: CounterpartyEngine,
    /// Cross-tick bus + report; see
    /// `boss_brewery_engine::BreweryEngineState` for the rationale.
    pub bus: SimEventBus,
    pub report: RunReport,
}

impl UsedDeviceShopEngineState {
    /// Build a fresh engine state from the seed bundle.
    /// Step-completion side effects route through
    /// boss-dispatcher's rule registry.
    pub fn load(seeds: &Path) -> Result<Self> {
        let tenant_path = seeds.join("tenant.toml");
        let kinds_path = seeds.join("workflows.toml");
        let tenant = TenantConfig::load(&tenant_path)
            .with_context(|| format!("loading tenant config from {}", tenant_path.display()))?;
        let kinds = load_workflows_with_owning_team(&kinds_path, &tenant.meta.tenant_id)
            .with_context(|| format!("loading job kinds from {}", kinds_path.display()))?;
        let registry = StepRegistry::v1();

        let mut state = ShapeDrivenState::new();
        // Default employees path = `<seeds>/../data/employees.json`.
        // Same convention `examples/used-device-shop/` uses.
        let employees_path = seeds.join("..").join("data").join("employees.json");
        let employees_arg = if employees_path.exists() {
            Some(employees_path.as_path())
        } else {
            None
        };
        seed_used_device_shop_subjects(&mut state, employees_arg);

        // Business calendars as DATA. This tenant has no live-fetch
        // daemon wired yet, so it uses the inline test calendars (the
        // same us-banking / us-tax / weekdays-only set the old
        // `with_builtins` provided). When this tenant grows a live
        // daemon, fetch from boss-calendar the way boss-brewery-engine
        // does and pass the result through here.
        let periodic = PeriodicEngine::new(tenant.periodic_specs(), CalendarRegistry::for_tests());
        let counterparty =
            CounterpartyEngine::new(tenant.counterparty_specs(), CalendarRegistry::for_tests());

        let rng = Rng::new(tenant.meta.seed);

        Ok(Self {
            kinds,
            registry,
            tenant,
            state,
            rng,
            periodic,
            counterparty,
            bus: SimEventBus::new(),
            report: RunReport::default(),
        })
    }
}

/// Advance a long-lived engine state by exactly one day. The
/// daemon path uses this in its tick loop so counterparty +
/// periodic + batch pending chains survive across ticks.
pub fn run_used_device_shop_one_day(
    engine: &mut UsedDeviceShopEngineState,
    day: NaiveDate,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let ticks_per_day = engine.tenant.meta.ticks_per_day();
    run_ticks_with_handlers(
        day,
        day,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
    )
}

/// Advance the engine by exactly one tick. See
/// `boss_brewery_engine::run_brewery_one_tick` for the contract;
/// callers must pair with [`used_device_shop_end_of_day`].
///
/// `ticks_per_day` is the number of slices the CALLER is running
/// this sim-day — the one clock, a parameter for the same reason
/// as the brewery: a warp daemon that collapses a day to one slice
/// must get a full-day tick, not a `1/ticks_per_day` sliver of one
/// (`tests/one_clock_ticks_per_day.rs`).
pub fn run_used_device_shop_one_tick(
    engine: &mut UsedDeviceShopEngineState,
    day: NaiveDate,
    tick_idx: u32,
    ticks_per_day: u32,
    output: &mut dyn SimOutput,
) -> Result<()> {
    if !engine.tenant.meta.is_operating_day(day) {
        return Ok(());
    }
    run_one_tick_with_handlers(
        day,
        tick_idx,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
        &mut engine.bus,
        &mut engine.report,
    )
}

/// End-of-sim-day flush — companion to
/// [`run_used_device_shop_one_tick`].
pub fn used_device_shop_end_of_day(
    engine: &mut UsedDeviceShopEngineState,
    day: NaiveDate,
    output: &mut dyn SimOutput,
) -> Result<()> {
    end_of_day_rollup(day, &mut engine.bus, output, &mut engine.report)
}

/// Run the engine for `days` days starting at `start` (defaults to
/// the tenant's configured start date) into the given `SimOutput`.
/// The caller owns the output so it can be `InMemoryOutput` (tests
/// and assertion checks) or `LiveApiOutput` (live-stack runs that
/// drive audit_log via the API services).
pub fn run_used_device_shop_into(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let mut engine = UsedDeviceShopEngineState::load(seeds)?;
    let start = start.unwrap_or(engine.tenant.meta.start_date);
    let end = start + chrono::Duration::days(days as i64 - 1);
    let ticks_per_day = engine.tenant.meta.ticks_per_day();

    run_ticks_with_handlers(
        start,
        end,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
    )
}

/// Run the engine for `days` days into a fresh `InMemoryOutput`.
/// Tests + the CLI's default mode use this.
pub fn run_used_device_shop(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
) -> Result<UsedDeviceShopRunResult> {
    let mut output = InMemoryOutput::default();
    let report = run_used_device_shop_into(seeds, days, start, &mut output)?;
    Ok(UsedDeviceShopRunResult { report, output })
}
