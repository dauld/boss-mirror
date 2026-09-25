//! Control + telemetry HTTP server for the `boss-brewery-sim` daemon.
//!
//! The daemon is a pure public-API client (it never touches the DB or the
//! bus). This localhost-only server lets `boss-simulator` OBSERVE how the
//! daemon is engaging the public API (the Cockpit) and — Phase 2 — GOVERN
//! its behavior config. It shares the daemon's in-process state via
//! `Arc<Mutex<…>>` and is reached only by `boss-simulator` on localhost
//! (NOT gateway-proxied). Inert unless the daemon runs (demo mode).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use boss_sim::actor_coverage::ActorCoverage;
use boss_sim::api_activity::ActorActivity;
use boss_sim::output::live::LiveApiStats;
use boss_sim::shape_driven::TenantConfig;
use boss_sim::workforce::WorkforceStats;

/// Path of the control-plane config override the daemon writes + reads.
/// Lives in the daemon's own state dir (NOT the seed bundle), so the
/// authored seed `tenant.toml` stays pristine. JSON for clean typed
/// round-trips with the Controls UI.
pub fn override_path() -> PathBuf {
    let dir =
        std::env::var("BOSS_SIM_STATE_DIR").unwrap_or_else(|_| "/var/lib/boss-sim".to_string());
    Path::new(&dir).join("tenant-override.json")
}

fn load_override(path: &Path) -> anyhow::Result<TenantConfig> {
    let text = std::fs::read_to_string(path)?;
    let cfg: TenantConfig = serde_json::from_str(&text)?;
    cfg.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(cfg)
}

/// Resolve the effective tenant config: the control-plane override if it
/// exists + parses + validates, else the seed `tenant.toml`. A bad
/// override falls back to the seed (logged) so a corrupt override can't
/// brick the daemon. Used at daemon startup AND by `GET /config`.
pub fn effective_tenant(seeds: &Path) -> anyhow::Result<TenantConfig> {
    let op = override_path();
    if op.exists() {
        match load_override(&op) {
            Ok(cfg) => {
                tracing::info!(path = %op.display(), "loaded tenant config override");
                return Ok(cfg);
            }
            Err(e) => tracing::warn!(
                path = %op.display(), error = %e,
                "config override invalid — using seed tenant.toml"
            ),
        }
    }
    let seed = seeds.join("tenant.toml");
    TenantConfig::load(&seed).map_err(|e| anyhow::anyhow!("loading seed tenant.toml: {e}"))
}

/// How many recent per-tick activity rows to retain in the ring buffer.
const RECENT_TICKS: usize = 60;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Clock + speed of the simulator's engagement — the cadence it drives the
/// public API at.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Cadence {
    pub sim_date: Option<String>,
    pub paused: bool,
    pub epoch_start: Option<String>,
    pub epoch_end: Option<String>,
    pub warp_factor: Option<f64>,
    pub days_per_tick: Option<i32>,
    pub tick_interval_seconds: Option<i32>,
}

/// One tick's worth of public-API engagement — the deltas the sim drove
/// that tick. The Cockpit renders these as the recent-activity feed.
#[derive(Debug, Clone, Serialize)]
pub struct TickActivity {
    pub tick: u64,
    pub sim_date: Option<String>,
    pub claimed: u64,
    pub completed: u64,
    pub deferred: u64,
    pub errors: u64,
}

/// A snapshot of how the simulator is engaging the public API. Served at
/// `GET /telemetry`; the Cockpit is oriented around it.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SimTelemetry {
    // --- identity: the sim drives the company AS the workforce, over the
    // public API (x-boss-user: automation:sim + x-sim-origin) ---
    pub actor: String,
    pub role: String,
    pub api_base: String,
    pub started_unix: i64,

    // --- clock / cadence ---
    pub cadence: Cadence,
    pub tick_count: u64,
    pub last_tick_unix: Option<i64>,

    // --- cumulative engagement since process start ---
    /// Workforce step transitions (checkins/claimed/completed/deferred/
    /// in_progress/errors) — the PUT /api/jobs/{}/steps engagement.
    pub workforce: WorkforceStats,
    /// Per-domain API writes (invoices/shipments/jobs/payments/…) — the
    /// step.done side-effect POSTs to the domain services.
    pub api_writes: LiveApiStats,

    // --- recent per-tick activity (ring buffer) ---
    pub recent: VecDeque<TickActivity>,

    // --- per-actor API engagement (the cockpit's actor panels) ---
    /// How the sim engages the API, attributed to the acting party
    /// (Employee by role · Account · Vendor · Bank · Environment) → each
    /// endpoint's calls + errors. Cumulative since process start.
    pub actors: Vec<ActorActivity>,
    /// Sim-date at the first recorded tick — the rate denominator
    /// (calls/sim-day = calls ÷ (current sim-date − this)).
    pub started_sim_date: Option<String>,

    // --- actor coverage (is the sim driving the WHOLE roster?) ---
    /// Per-role roster vs distinct-actors-acting vs completions, plus
    /// headline totals. The under-simulation signal: a role no live
    /// workflow step ever routes to shows up DORMANT on the telemetry's
    /// face instead of being rediscovered by SQL. Operator-held roles
    /// (platform-admin — excluded from sim driving by design) are
    /// labeled `operator`, never dormant.
    pub actor_coverage: ActorCoverage,
}

impl SimTelemetry {
    pub fn new(actor: String, role: String, api_base: String) -> Self {
        Self {
            actor,
            role,
            api_base,
            started_unix: now_unix(),
            ..Default::default()
        }
    }

    /// Refresh from the tick loop after a working tick: push a per-tick
    /// activity row (deltas vs the previous cumulative workforce
    /// snapshot), then replace the cumulative stats + cadence.
    pub fn record_tick(
        &mut self,
        tick: u64,
        cadence: Cadence,
        workforce: &WorkforceStats,
        api_writes: &LiveApiStats,
        actors: Vec<ActorActivity>,
        actor_coverage: ActorCoverage,
    ) {
        // Deltas vs the previous cumulative snapshot (read before we
        // overwrite self.workforce below).
        let claimed = workforce.claimed.saturating_sub(self.workforce.claimed);
        let completed = workforce.completed.saturating_sub(self.workforce.completed);
        let deferred = workforce.deferred.saturating_sub(self.workforce.deferred);
        let errors = workforce.errors.saturating_sub(self.workforce.errors);
        self.recent.push_back(TickActivity {
            tick,
            sim_date: cadence.sim_date.clone(),
            claimed,
            completed,
            deferred,
            errors,
        });
        while self.recent.len() > RECENT_TICKS {
            self.recent.pop_front();
        }

        if self.started_sim_date.is_none() {
            self.started_sim_date = cadence.sim_date.clone();
        }
        self.cadence = cadence;
        self.tick_count = tick;
        self.last_tick_unix = Some(now_unix());
        self.workforce = workforce.clone();
        self.api_writes = api_writes.clone();
        self.actors = actors;
        self.actor_coverage = actor_coverage;
    }

    /// Update just the cadence (paused / restarting branches, where no
    /// work was done so no activity row is pushed).
    pub fn note_cadence(&mut self, cadence: Cadence) {
        self.cadence = cadence;
        self.last_tick_unix = Some(now_unix());
    }
}

/// Shared handle held by both the tick loop (writer) and the control
/// server (reader).
pub type SharedTelemetry = Arc<Mutex<SimTelemetry>>;

#[derive(Clone)]
struct ControlState {
    telemetry: SharedTelemetry,
    seeds: PathBuf,
}

async fn get_telemetry(State(st): State<ControlState>) -> Json<SimTelemetry> {
    let snapshot = st.telemetry.lock().map(|t| t.clone()).unwrap_or_default();
    Json(snapshot)
}

/// GET /config — the effective tenant config (override if set, else the
/// seed) as typed JSON. The Controls UI edits this and POSTs it back.
async fn get_config(State(st): State<ControlState>) -> Response {
    match effective_tenant(&st.seeds) {
        Ok(cfg) => Json(cfg).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("config load failed: {e}"),
        )
            .into_response(),
    }
}

/// POST /config — validate an operator-edited config, persist it as the
/// override, then exit non-zero so systemd (Restart=on-failure) restarts
/// the daemon with it (the "edit + restart" model). boss-simulator
/// operator-gates this; the daemon control server is localhost-only.
async fn post_config(State(_st): State<ControlState>, Json(cfg): Json<TenantConfig>) -> Response {
    if let Err(e) = cfg.validate() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid config: {e}"),
        )
            .into_response();
    }
    let path = override_path();
    let json = match serde_json::to_string_pretty(&cfg) {
        Ok(j) => j,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("serialize: {e}")).into_response();
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, json) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("write override: {e}"),
        )
            .into_response();
    }
    tracing::warn!(path = %path.display(), "config override written; exiting to restart with new config");
    // Respond first, then exit non-zero so systemd restarts us with the
    // new config. The brief delay lets the HTTP response flush.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        std::process::exit(75);
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "applied", "restarting": true })),
    )
        .into_response()
}

async fn health() -> &'static str {
    "ok"
}

/// Run the control + telemetry server. Binds `bind` (e.g.
/// `127.0.0.1:7011`); localhost-only. Spawned by the daemon as a
/// background task — returns only on listener error.
pub async fn serve(bind: String, telemetry: SharedTelemetry, seeds: PathBuf) -> anyhow::Result<()> {
    let state = ControlState { telemetry, seeds };
    let app = Router::new()
        .route("/telemetry", get(get_telemetry))
        .route("/config", get(get_config).post(post_config))
        .route("/health", get(health))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "sim control + telemetry server listening");
    let app = boss_core::machine_gate::mount(app, "sim-control", &["/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use boss_sim::actor_coverage;

    use super::*;

    #[test]
    fn record_tick_stores_actor_coverage_on_the_wire() {
        let mut t = SimTelemetry::new(
            "automation:sim".to_string(),
            "system-sim".to_string(),
            "direct://127.0.0.1".to_string(),
        );
        let emp_roles: HashMap<String, String> = HashMap::from([
            ("emp-1".to_string(), "brewer".to_string()),
            ("emp-2".to_string(), "cellar-hand".to_string()),
        ]);
        let completions: HashMap<String, u64> = HashMap::from([("emp-1".to_string(), 4)]);
        let cov = actor_coverage::compute(&emp_roles, &completions, &HashSet::new());
        t.record_tick(
            1,
            Cadence::default(),
            &WorkforceStats::default(),
            &LiveApiStats::default(),
            Vec::new(),
            cov,
        );

        // The stored block carries the dormancy verdict...
        assert_eq!(t.actor_coverage.roles_total, 2);
        assert_eq!(t.actor_coverage.roles_acting, 1);
        assert_eq!(t.actor_coverage.roles_dormant, 1);

        // ...and GET /telemetry serializes it under the key + shape the
        // SPA reads (apps/simulator/src/types.ts `ActorCoverage`).
        let v = serde_json::to_value(&t).unwrap();
        let cov = v.get("actor_coverage").expect("actor_coverage block");
        assert_eq!(cov["employees_total"], 2);
        assert_eq!(cov["employees_acting"], 1);
        assert_eq!(cov["roles"][0]["role"], "brewer");
        assert_eq!(cov["roles"][0]["status"], "acting");
        assert_eq!(cov["roles"][0]["completions"], 4);
        assert_eq!(cov["roles"][1]["role"], "cellar-hand");
        assert_eq!(cov["roles"][1]["status"], "dormant");
    }
}
