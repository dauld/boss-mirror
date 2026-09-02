//! `workforce` — the sim-as-workforce executor.
//!
//! The simulator stops holding a job/step mirror. Instead it acts as the
//! workforce the way real executors do: it reads the live system (the
//! clock, its open assignments, real inventory) and works them through
//! the public API. This is the executor side of BOSS's
//! "human-powered state machine" framing — the sim is just CPUs driving
//! the same state machine a real deployment runs.
//!
//! ## Clock-coordinated, never clock-driving
//!
//! Time comes from the clock-api's freely-advancing formula clock
//! (`sim = epoch_start + (wall_now − wall_anchor) × warp_factor`). The
//! workforce *reads* `/api/clock/now`; it never manipulates the clock.
//! A step that became Active at `started_at` completes only once
//! `now ≥ started_at + duration` — so a 5-day fermentation is genuinely
//! held for five sim-days of clock time while a 15-minute QC finishes
//! almost immediately. Durations are real, paced by the clock, not a
//! per-tick completion-probability roll. The duration itself resolves
//! spec-first: a step whose Workflow spec authors `duration_hours`
//! (read via the Job's pinned workflow version) is paced by that; the
//! StepType kind's typical duration is only the fallback.
//!
//! ## Pull the whole assigned backlog in one query
//!
//! Routing and execution are separate layers. The dispatcher (and, later,
//! managers) ASSIGN Ready steps to employees; the workforce EXECUTES what
//! it's been assigned. Each pass pulls the entire assigned-and-workable
//! backlog in a single query — `GET /api/jobs/assignments?all_assigned=true`
//! — and drives every step in it, attributing the work to each step's own
//! assignee. The workforce holds no roster and makes no routing decision:
//! it works whatever has been assigned, regardless of who assigned it or
//! to whom. That keeps it decoupled from assignment policy (load
//! distribution can change without touching the executor) and replaces a
//! per-employee query fan-out — which dominated pass time — with one
//! round-trip.
//!
//! Each `work_once`:
//! 1. read `/api/clock/now`
//! 2. `GET /api/jobs/assignments?all_assigned=true` — every assigned
//!    Ready step to start + Active step to finish
//! 3. for each:
//!    - Ready: gate (production-consume checks *real* inventory and
//!      defers if short; demand-gate is handled at completion) → claim
//!      (Ready→Active, stamp `metadata.started_at`)
//!    - Active: if `now ≥ started_at + duration`, complete
//!      (Active→Completed; demand-gate reads real finished-goods stock to
//!      decide brew vs oversupply); otherwise leave it in progress
//!
//! The driver calls `work_once` on a wall-cadence until the clock
//! reaches `epoch_end`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::actor_coverage::ActorCoverage;
use crate::api_activity::{self, ActorKind, ApiActivity};

/// Default duration for step kinds with no `typical_duration_hours`
/// (generic / task / sub-job) — paces an untyped step as a full
/// working day.
const DEFAULT_STEP_HOURS: f64 = 8.0;

/// Resolve a service URL the same way `LiveApiOutput` does: `direct://`
/// host hits per-service prod ports, `scratch://` adds the +1000 offset,
/// anything else is treated as a gateway base prefix.
fn service_url(api_base: &str, path: &str) -> String {
    let (host, offset) = if let Some(rest) = api_base.strip_prefix("direct://") {
        (rest, 0i32)
    } else if let Some(rest) = api_base.strip_prefix("scratch://") {
        (rest, 1000i32)
    } else {
        return format!("{api_base}{path}");
    };
    let base_port: i32 = if path.starts_with("/api/jobs") {
        7900
    } else if path.starts_with("/api/people") {
        7500
    } else if path.starts_with("/api/inventory") {
        7300
    } else if path.starts_with("/api/products") {
        7840
    } else if path.starts_with("/api/clock") {
        7060
    } else {
        4443
    };
    format!("http://{host}:{}{path}", base_port + offset)
}

/// A required-at-done field the executor must supply when completing a
/// step — its `name` and the StepType `field_type` (enum types arrive
/// pipe-joined, e.g. `"pass|fail|conditional"`). The workforce synthesizes
/// a type-appropriate value when the Workflow didn't default it — the
/// simulated worker filling the step's form the way a human would before
/// marking it done. Built from the StepRegistry's `FieldSpec`s and passed
/// to [`Workforce::new`].
#[derive(Debug, Clone)]
pub struct RequiredField {
    pub name: String,
    pub field_type: String,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct WorkforceStats {
    pub checkins: u64,
    pub claimed: u64,
    pub completed: u64,
    /// Real (non-simulated) assignments the boundary refused to touch.
    /// Nonzero is normal - humans and agents have queues too - but this
    /// number existing is what makes the boundary observable.
    pub real_skipped: u64,
    /// Steps left Ready because real inventory was short (the
    /// auto-reorder catches up).
    pub deferred: u64,
    /// Steps left Active because their duration hasn't elapsed yet.
    pub in_progress: u64,
    pub errors: u64,
    /// Steps assigned to operator identities (real logins) that the
    /// sim deliberately left Ready for the human — see
    /// `workable_assignee`.
    pub operator_skipped: u64,
    /// Steps left Ready because the assignee's labor-hour day was
    /// full (d64fe2d2). Only specs that author `labor_hours` can
    /// produce these; the count existing is what makes the capacity
    /// model observable rather than a silent cap.
    pub labor_deferred: u64,
}

/// How many steps the workforce drives in parallel per check-in.
/// Completion is the sim's bottleneck: each step claim/complete is a
/// blocking HTTP round-trip, so a serial loop caps throughput at
/// ~10 completions/wall-sec, which trails job generation at warp and
/// grows WIP over a long regen. Completion capacity ≈ (polls ×
/// workers) has to cover the run's step-completions or open Jobs pile
/// up. Completions hit jobs-api + the side-effect services, whose
/// connection pools have the headroom, and any transient blip is
/// NAK'd + redelivered by the JetStream layer rather than lost. The
/// invariant: saturate the DB, don't error it — if a service pool
/// starts queueing, dead-letters reappear and this comes back down.
const WORKFORCE_WORKERS: usize = 16;

/// Per-step tally returned by `work_step` so the parallel completion loop
/// sums results without sharing `&mut self.stats` across threads.
#[derive(Debug, Default)]
struct StepDelta {
    claimed: u64,
    completed: u64,
    deferred: u64,
    in_progress: u64,
    errors: u64,
    /// Assigned to an operator identity — left for the human.
    operator_skipped: u64,
    /// Left Ready because the assignee's labor-hour day is full.
    labor_deferred: u64,
}

/// A spec's authored hours, all three legs (d64fe2d2). `duration` is
/// the pre-split field and feeds ONLY the wall-clock leg — a legacy
/// fermentation's 168h is calendar, and reading it as labor would eat
/// 21 days of one person's budget on a step nobody attends.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SpecHours {
    pub labor: Option<f64>,
    pub wall: Option<f64>,
    pub duration: Option<f64>,
}

/// The wall-clock pacing leg: `wall_clock_hours`, then the pre-split
/// `duration_hours`, then the kind's typical. This is what gates
/// Active → complete on elapsed sim-time.
pub(crate) fn pacing_hours(spec: Option<SpecHours>, kind_typical: f64) -> f64 {
    spec.and_then(|s| s.wall.or(s.duration))
        .unwrap_or(kind_typical)
}

/// The labor leg: Some only where the spec AUTHORS `labor_hours`.
/// Unauthored meters nothing — the Q3 rider makes realism a
/// protocol-authoring expectation, not a sweep invariant, so every
/// existing Workflow keeps today's unmetered behaviour.
pub(crate) fn labor_commitment(spec: Option<SpecHours>) -> Option<f64> {
    spec.and_then(|s| s.labor)
}

/// One person contributes at most this many labor-hours per sim day
/// (David's Q3 norm). The boundary is inclusive: a commitment that
/// lands exactly on the cap still fits the day.
pub(crate) const LABOR_DAY_CAP: f64 = 8.0;

/// May `commitment` more labor-hours join `spent` today?
pub(crate) fn labor_fits(spent: f64, commitment: f64, cap: f64) -> bool {
    spent + commitment <= cap
}

#[derive(Debug, Deserialize)]
struct ClockNowResp {
    now: DateTime<Utc>,
    #[serde(default)]
    epoch_end: Option<chrono::NaiveDate>,
}

/// The sim-as-workforce executor. Holds no job/step state — every
/// decision reads the live system.
pub struct Workforce {
    client: reqwest::blocking::Client,
    api_base: String,
    /// StepType kind → typical duration hours, sourced from the
    /// StepRegistry. Drives the duration-gated completion — as the
    /// FALLBACK: a step whose Workflow spec authors its own
    /// `duration_hours` is paced by that instead (see
    /// `step_duration_hours`).
    durations: HashMap<String, f64>,
    /// (workflow kind, pinned version) → spec slug → spec-authored
    /// `duration_hours`. Lazily fetched from the registry's versioned
    /// read surface on first sight of a (workflow, version) pair and
    /// cached — Workflow versions are append-only, so a cached row
    /// can never go stale. Mutex, not RwLock: the critical sections
    /// are a lookup or an insert, and the worker threads spend their
    /// time in HTTP round-trips, not here.
    spec_durations: std::sync::Mutex<HashMap<(String, i32), HashMap<String, SpecHours>>>,
    /// (employee, sim day) → labor-hours committed. Grows one key per
    /// worker per day and resets by keying on the day, not by sweeping.
    /// Only steps whose spec authors `labor_hours` write here.
    labor_spent: std::sync::Mutex<HashMap<(String, chrono::NaiveDate), f64>>,
    /// StepType kind → its required-at-done fields, sourced from the
    /// StepRegistry. On completion the workforce supplies any the Workflow
    /// didn't default — the executor filling the step's form.
    required_fields: HashMap<String, Vec<RequiredField>>,
    /// Shared per-actor API-call tally (cockpit telemetry). The daemon
    /// injects the shared handle via `with_actor_telemetry`; default is a
    /// detached fresh handle so non-daemon callers + tests work unchanged.
    api_activity: ApiActivity,
    /// Employee id → role. Attributes this executor's `PUT /steps` calls
    /// to the worker's role (sign-offs carry their own role). The workforce
    /// holds no *routing* roster — this is display attribution only.
    emp_roles: HashMap<String, String>,
    /// Operator identities the sim must never act as (real logins —
    /// the bootstrap admin, any platform-admin-role identity). Steps
    /// the dispatcher routes to them stay Ready for the human; see
    /// `workable_assignee`.
    excluded_assignees: std::collections::HashSet<String>,
    /// Employee id → steps completed this process lifetime. The input
    /// to [`Self::actor_coverage`] — WHO acts, where `WorkforceStats`
    /// only counts HOW MUCH. Mutex (not a plain field): `complete`
    /// takes `&self` from the parallel step workers.
    completions_by_actor: std::sync::Mutex<HashMap<String, u64>>,
    pub stats: WorkforceStats,
}

impl Workforce {
    /// `api_base` accepts the same forms as `LiveApiOutput::new`
    /// (`direct://host`, `scratch://host`, or a gateway base).
    /// `durations` maps StepType kind → typical_duration_hours;
    /// `required_fields` maps StepType kind → its required-at-done fields
    /// (so the worker can fill any the Workflow didn't default).
    pub fn new(
        api_base: &str,
        durations: HashMap<String, f64>,
        required_fields: HashMap<String, Vec<RequiredField>>,
    ) -> Self {
        // Same actor identity as LiveApiOutput: id=system, role=
        // system-sim, plus x-sim-origin so the receiving services'
        // policy gate takes the sim bypass. Attribution to the real
        // employee rides on the body's assignee_id / completed_by.
        let mut headers = reqwest::header::HeaderMap::new();
        let actor = json!({
            "id": "automation:sim",
            "role": "system-sim",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&actor) {
            headers.insert("x-boss-user", v);
        }
        headers.insert(
            "x-sim-origin",
            reqwest::header::HeaderValue::from_static("true"),
        );
        let client = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        Self {
            client,
            api_base: api_base.to_string(),
            durations,
            spec_durations: std::sync::Mutex::new(HashMap::new()),
            labor_spent: std::sync::Mutex::new(HashMap::new()),
            required_fields,
            api_activity: api_activity::new_handle(),
            emp_roles: HashMap::new(),
            excluded_assignees: Default::default(),
            completions_by_actor: std::sync::Mutex::new(HashMap::new()),
            stats: WorkforceStats::default(),
        }
    }

    /// Inject the shared per-actor API-activity handle + the emp→role map
    /// (cockpit telemetry). The daemon creates one handle, hands it to
    /// both the workforce + the live output, and snapshots it each tick.
    pub fn with_actor_telemetry(
        mut self,
        handle: ApiActivity,
        emp_roles: HashMap<String, String>,
    ) -> Self {
        self.api_activity = handle;
        self.emp_roles = emp_roles;
        self
    }

    /// Mark identities the sim must never act as (extends across calls,
    /// so the seed roster's operators and the API-discovered ones
    /// compose). Steps assigned to them are left untouched — Ready for
    /// the real human.
    pub fn with_excluded_assignees(mut self, ids: impl IntoIterator<Item = String>) -> Self {
        self.excluded_assignees.extend(ids);
        self
    }

    /// Count one completed step against `emp` — the actor-coverage
    /// tally ([`Self::actor_coverage`]). A poisoned lock drops the
    /// sample rather than panicking: telemetry never wedges the sim.
    fn note_completion(&self, emp: &str) {
        if let Ok(mut m) = self.completions_by_actor.lock() {
            *m.entry(emp.to_string()).or_default() += 1;
        }
    }

    /// Snapshot actor coverage: who on the roster this executor is
    /// actually driving, per role, vs who has never completed a step.
    /// Pure function of the emp→role map, the per-employee completion
    /// tally, and the operator-exclusion set (operators are labeled,
    /// never counted dormant). The daemon serves this in `/telemetry`.
    pub fn actor_coverage(&self) -> ActorCoverage {
        let completions = self
            .completions_by_actor
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        crate::actor_coverage::compute(&self.emp_roles, &completions, &self.excluded_assignees)
    }

    /// The role to attribute an employee's API calls to (cockpit display).
    fn role_of(&self, emp: &str) -> &str {
        self.emp_roles
            .get(emp)
            .map(String::as_str)
            .unwrap_or("unassigned-role")
    }

    /// Record one workforce call on its ack, under the Employee actor —
    /// rolled up by `role` (the cockpit panel grouping) and counting `emp`
    /// toward that role's distinct-people tally.
    fn record_employee_call(&self, method: &str, path: &str, emp: &str, role: &str, ok: bool) {
        let endpoint = api_activity::endpoint_label(method, path);
        api_activity::record(
            &self.api_activity,
            ActorKind::Employee,
            role,
            &endpoint,
            ok,
            emp,
        );
    }

    fn duration_hours(&self, kind: &str) -> f64 {
        self.durations
            .get(kind)
            .copied()
            .unwrap_or(DEFAULT_STEP_HOURS)
    }

    /// The spec-authored hours for this row's step, if its pinned
    /// Workflow version authors any. First sight of a (workflow,
    /// version) pair fetches the version row and caches its slug →
    /// hours map. A failed fetch resolves to `None` (kind-default
    /// pacing, no labor metering, for this pass) and is NOT cached,
    /// so a transient registry error heals on the next check-in.
    fn spec_hours(&self, row: &Value, step: &Value) -> Option<SpecHours> {
        let workflow = row.get("workflow")?.as_str()?;
        let version = i32::try_from(row.get("workflow_version")?.as_i64()?).ok()?;
        let slug = step.get("spec_slug")?.as_str()?;
        let key = (workflow.to_string(), version);
        if let Ok(cache) = self.spec_durations.lock()
            && let Some(by_slug) = cache.get(&key)
        {
            return by_slug.get(slug).copied();
        }
        match self.fetch_spec_durations(workflow, version) {
            Ok(by_slug) => {
                let hours = by_slug.get(slug).copied();
                if let Ok(mut cache) = self.spec_durations.lock() {
                    cache.insert(key, by_slug);
                }
                hours
            }
            Err(e) => {
                warn!(
                    workflow, version, error = %e,
                    "spec-duration fetch failed; kind default paces this pass"
                );
                None
            }
        }
    }

    /// GET the pinned Workflow version row and index each step's
    /// spec-authored hours (all three legs — labor, wall-clock, and
    /// the pre-split duration) by its slug (`title`). 404 is
    /// definitive — no such version row — and comes back as an empty
    /// map so it caches; transport / 5xx errors bubble up so the
    /// caller retries next pass.
    fn fetch_spec_durations(
        &self,
        workflow: &str,
        version: i32,
    ) -> Result<HashMap<String, SpecHours>> {
        let url = service_url(
            &self.api_base,
            &format!("/api/workflows/{workflow}/versions/{version}"),
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(HashMap::new());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            anyhow::bail!("GET {url} -> {status}");
        }
        let spec: Value = resp.json().with_context(|| format!("decode {url}"))?;
        let leg = |s: &Value, key: &str| s.get(key).and_then(Value::as_f64);
        Ok(spec
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| {
                        Some((
                            s.get("title")?.as_str()?.to_string(),
                            SpecHours {
                                labor: leg(s, "labor_hours"),
                                wall: leg(s, "wall_clock_hours"),
                                duration: leg(s, "duration_hours"),
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Read the freely-advancing sim clock. Returns `(now, epoch_end)`.
    pub fn clock_now(&self) -> Result<(DateTime<Utc>, Option<chrono::NaiveDate>)> {
        let url = service_url(&self.api_base, "/api/clock/now");
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let parsed: ClockNowResp = resp.json().context("decode /api/clock/now")?;
        Ok((parsed.now, parsed.epoch_end))
    }

    /// Configure the formula clock ONCE at run kickoff: sim-time starts
    /// at `epoch_start` (as of wall-now) and free-runs at `warp_factor`
    /// sim-seconds per wall-second up to `epoch_end`. After this the
    /// clock is never touched again — the whole system coordinates
    /// against it. `warp_factor` is the single pacing knob (wall-time ≈
    /// sim-span ÷ warp); pick the fastest the system sustains.
    pub fn configure_clock(
        &self,
        epoch_start: chrono::NaiveDate,
        epoch_end: chrono::NaiveDate,
        warp_factor: f64,
    ) -> Result<()> {
        let url = service_url(&self.api_base, "/api/clock/configure");
        let body = json!({
            "epoch_start": epoch_start.to_string(),
            "epoch_end": epoch_end.to_string(),
            "warp_factor": warp_factor,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("clock configure {url} -> {status}: {text}");
        }
        Ok(())
    }

    /// One workforce check-in pass against the current clock instant.
    ///
    /// Pulls the entire assigned-and-workable backlog in ONE query and
    /// drives each step. No roster, no per-employee fan-out: the executor
    /// works whatever has been assigned (see the module docs).
    pub fn work_once(&mut self) -> Result<()> {
        let (now, _) = self.clock_now()?;
        self.stats.checkins += 1;
        let url = service_url(
            &self.api_base,
            "/api/jobs/assignments?all_assigned=true&limit=50000",
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let body: Value = resp.json().context("decode /api/jobs/assignments")?;
        let all_rows = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        // THE SIM BOUNDARY, enforced where selection happens. This
        // query returns every assigned workable step in the SYSTEM -
        // real packets included - and on 2026-08-20/21 this executor
        // "completed" two real cars' proven steps and all six daily
        // maintenance sweeps under their assignees' identities, filling
        // required fields with capitalized field-name echoes
        // (verified='Verified'), and left the sweeps fork-unfireable
        // (defect 88798c96). A simulated worker works simulated
        // packets, full stop. FAIL CLOSED: a row that does not say
        // simulated=true is not the sim's to touch - absent means real
        // (rows predating the column carry sim tags the envelope
        // already folds into `simulated`).
        let mut real_skipped = 0u64;
        let rows: Vec<Value> = all_rows
            .into_iter()
            .filter(|row| {
                let sim = row_is_simulated(row);
                if !sim {
                    real_skipped += 1;
                }
                sim
            })
            .collect();
        self.stats.real_skipped += real_skipped;

        // Drive steps with bounded parallelism (see WORKFORCE_WORKERS). The
        // blocking client is Clone+Send+Sync and each step claims/completes
        // its own row independently, so N workers pull from a shared queue
        // and tally local deltas — no `&mut self` shared across threads. A
        // single Job's steps still surface one at a time (each goes Ready
        // only after its predecessor completes), so this doesn't reorder a
        // Job's pipeline.
        let this: &Self = self;
        let next = std::sync::atomic::AtomicUsize::new(0);
        let deltas: Vec<StepDelta> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WORKFORCE_WORKERS)
                .map(|_| {
                    scope.spawn(|| {
                        let mut d = StepDelta::default();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(row) = rows.get(i) else { break };
                            match this.work_step(row, now) {
                                Ok(sd) => {
                                    d.claimed += sd.claimed;
                                    d.completed += sd.completed;
                                    d.deferred += sd.deferred;
                                    d.in_progress += sd.in_progress;
                                    d.operator_skipped += sd.operator_skipped;
                                    d.labor_deferred += sd.labor_deferred;
                                }
                                Err(e) => {
                                    d.errors += 1;
                                    warn!(error = %e, "workforce: step failed");
                                }
                            }
                        }
                        d
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for d in deltas {
            self.stats.claimed += d.claimed;
            self.stats.completed += d.completed;
            self.stats.deferred += d.deferred;
            self.stats.in_progress += d.in_progress;
            self.stats.errors += d.errors;
            self.stats.operator_skipped += d.operator_skipped;
            self.stats.labor_deferred += d.labor_deferred;
        }
        Ok(())
    }

    fn work_step(&self, row: &Value, now: DateTime<Utc>) -> Result<StepDelta> {
        let mut delta = StepDelta::default();
        let job_id = row.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        let step = row.get("step").unwrap_or(&Value::Null);
        let step_id = step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let kind = step.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let status = step.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let authored_fields: Vec<(String, String)> = step
            .get("fields")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let required = f.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
                        if !required {
                            return None;
                        }
                        Some((
                            f.get("name")?.as_str()?.to_string(),
                            f.get("field_type")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sign_offs_required: Vec<String> = step
            .get("sign_offs_required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let metadata = step.get("metadata").cloned().unwrap_or_else(|| json!({}));
        if job_id.is_empty() || step_id.is_empty() {
            return Ok(delta);
        }
        // Execute only ASSIGNED work, attributed to the actual assignee.
        // An unassigned row is someone else's to route (the dispatcher
        // assigns role-bearing steps; the marker handler completes the
        // no-role markers) — skip it. Steps assigned to operator
        // identities are also skipped (`workable_assignee`): they are
        // a real human's queue, not the sim's.
        let emp = match workable_assignee(step, &self.excluded_assignees) {
            Some(e) => e.to_string(),
            None => {
                if assignee_of(step).is_some() {
                    delta.operator_skipped += 1;
                }
                return Ok(delta);
            }
        };

        // Spec-authored hours (via the Job's pinned workflow version):
        // the wall-clock leg paces, the labor leg meters capacity.
        let spec = self.spec_hours(row, step);
        let step_hours = pacing_hours(spec, self.duration_hours(kind));

        match status {
            "ready" => {
                // Don't start a brew's consume without the ingredients —
                // leave it Ready so the auto-reorder can catch up; the
                // role queue re-surfaces it next pass.
                if kind == "production-consume" && self.short_on_ingredients(&metadata)? {
                    delta.deferred += 1;
                    return Ok(delta);
                }
                // The labor budget (d64fe2d2, Q3 norm): a person holds
                // at most LABOR_DAY_CAP labor-hours per sim day, and a
                // step meters against it only where its spec authors
                // `labor_hours`. Committed at CLAIM — that is when the
                // person's day is spoken for — and left Ready when the
                // day is full; the role queue re-surfaces it tomorrow.
                if let Some(commitment) = labor_commitment(spec) {
                    let day = now.date_naive();
                    let mut spent = self
                        .labor_spent
                        .lock()
                        .map_err(|_| anyhow::anyhow!("labor_spent lock poisoned"))?;
                    let so_far = spent.get(&(emp.clone(), day)).copied().unwrap_or(0.0);
                    if !labor_fits(so_far, commitment, LABOR_DAY_CAP) {
                        delta.labor_deferred += 1;
                        return Ok(delta);
                    }
                    spent.insert((emp.clone(), day), so_far + commitment);
                }
                self.claim(job_id, step_id, &emp, &metadata, now)?;
                delta.claimed += 1;
                // An assigned zero-duration step completes the same pass —
                // no point holding it. (Structural markers complete
                // elsewhere: triggers are resolved at materialization, and
                // outcome / milestone are completed by the dispatcher's
                // marker handler — none are ever assigned to a worker.)
                if step_hours <= 0.0 {
                    self.complete(
                        job_id,
                        step_id,
                        kind,
                        &metadata,
                        &emp,
                        &sign_offs_required,
                        &authored_fields,
                        now,
                    )?;
                    delta.completed += 1;
                }
            }
            "active" => {
                let started = metadata
                    .get("started_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));
                let elapsed_enough = match started {
                    Some(start) => now >= start + duration_from_hours(step_hours),
                    // No stamp (claimed before this model / orphan) — let
                    // it complete rather than wedge forever.
                    None => true,
                };
                if elapsed_enough {
                    self.complete(
                        job_id,
                        step_id,
                        kind,
                        &metadata,
                        &emp,
                        &sign_offs_required,
                        &authored_fields,
                        now,
                    )?;
                    delta.completed += 1;
                } else {
                    delta.in_progress += 1;
                }
            }
            _ => {}
        }
        Ok(delta)
    }

    /// Ready → Active. Stamps `started_at` so the completion gate can
    /// measure elapsed sim-time. Metadata is sent whole (PATCH-on-PUT
    /// replaces it wholesale) so no existing keys are lost.
    fn claim(
        &self,
        job_id: &str,
        step_id: &str,
        emp: &str,
        metadata: &Value,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut md = metadata.clone();
        if let Some(obj) = md.as_object_mut() {
            obj.insert("started_at".to_string(), json!(now.to_rfc3339()));
        }
        let body = json!({
            "status": "active",
            "assignee_id": emp,
            "metadata": md,
        });
        self.put_step(job_id, step_id, &body, emp)
    }

    /// Active → Completed, attributed to `emp`. For a demand-gate step,
    /// reads real finished-goods stock to stamp the brew/oversupply
    /// outcome the Workflow forks on. Co-signs in the same PUT when the
    /// step needs sign-off (the sim-origin bypass authorizes it).
    fn complete(
        &self,
        job_id: &str,
        step_id: &str,
        kind: &str,
        metadata: &Value,
        emp: &str,
        sign_offs_required: &[String],
        authored_fields: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut body = json!({
            "status": "completed",
            "completed_by": emp,
        });
        let obj = body.as_object_mut().expect("object");
        // Supply the step's required-at-done fields the executor would
        // fill in. Start from the current metadata so PATCH-on-PUT keeps
        // existing keys, then fill any required field the Workflow didn't
        // already default.
        let mut md = metadata.as_object().cloned().unwrap_or_default();
        self.fill_required_fields(kind, &mut md, step_id, now);
        // Inline authoring: the step's own required fields
        // are part of the completion contract too.
        for (name, field_type) in authored_fields {
            if !md.contains_key(name) {
                md.insert(
                    name.clone(),
                    synth_field_value(field_type, name, step_id, now),
                );
            }
        }
        // Invariant-bound keg-fleet fields (93f936b9): where the fields
        // above were synthesized free, overwrite them with values that
        // satisfy the fleet's own conservation contract.
        self.reconcile_keg_fleet_fields(&mut md, metadata, job_id, step_id);
        // Gates (demand-gate / availability-gate) are agent-executed: the
        // dispatcher reads real stock and stamps the outcome on step.ready.
        // The workforce only drives assigned steps and agent steps are never
        // assigned, so the workforce never sees a gate — the gate decision
        // lives in boss-dispatcher's gate.resolve handler (the sim drives
        // only labor; see docs/architecture-decisions.md).
        obj.insert("metadata".to_string(), Value::Object(md.clone()));
        if sign_offs_required.is_empty() {
            self.put_step(job_id, step_id, &body, emp)?;
            self.note_completion(emp);
            return Ok(());
        }
        // Sign-off contract: stamps attest the step's FINAL shape, so the
        // metadata the executor fills lands first, then the stamps,
        // then the status flip. Stamping happens as the role-matched
        // human — policy (the seeded step-signoff:<role> rules)
        // decides, no sim exemption.
        self.put_step(
            job_id,
            step_id,
            &json!({ "metadata": Value::Object(md) }),
            emp,
        )?;
        for role in sign_offs_required {
            self.post_sign_off(job_id, step_id, emp, role)?;
        }
        self.put_step(
            job_id,
            step_id,
            &json!({ "status": "completed", "completed_by": emp }),
            emp,
        )?;
        self.note_completion(emp);
        Ok(())
    }

    /// Fill the kind's required-at-done fields the Workflow didn't default,
    /// with type-appropriate values — the simulated executor supplying the
    /// inputs a human would type before marking the step done. Existing
    /// keys (Workflow defaults, values set on an earlier transition) are
    /// never overwritten. `step_id` is the spread key for enum-typed
    /// fields: stable per step, varied across the population.
    fn fill_required_fields(
        &self,
        kind: &str,
        md: &mut serde_json::Map<String, Value>,
        step_id: &str,
        now: DateTime<Utc>,
    ) {
        let Some(fields) = self.required_fields.get(kind) else {
            return;
        };
        for f in fields {
            if md.contains_key(&f.name) {
                continue;
            }
            md.insert(
                f.name.clone(),
                synth_field_value(&f.field_type, &f.name, step_id, now),
            );
        }
    }

    /// The keg ledger's conservation contract, honored at the source
    /// (93f936b9, David's Q1 decision — the full balance-sheet keg
    /// model). Two fixups, both gated on the field having been
    /// SYNTHESIZED this pass (absent from the incoming metadata):
    /// an operator- or Workflow-authored value is never overwritten.
    ///
    /// - Fleet-out leg (`kegs_out` + `deposit_cents` together): the
    ///   deposit derives from the count at $30/keg — the one keg field
    ///   with real-world ground truth — instead of a free-form fake.
    /// - Returns leg (`kegs_returned` + `kegs_lost` together): the
    ///   pair becomes a conserved partition of the fleet's own
    ///   `kegs_out` (read off the job's earlier log-fleet-out step),
    ///   because `/api/ledger/keg-deposit-settlements` 422s any fleet
    ///   whose counts don't satisfy `returned + lost == out`. A job
    ///   with no `kegs_out` leg keeps the free fakes — there is no
    ///   invariant to satisfy.
    fn reconcile_keg_fleet_fields(
        &self,
        md: &mut serde_json::Map<String, Value>,
        incoming: &Value,
        job_id: &str,
        step_id: &str,
    ) {
        let synthesized = |md: &serde_json::Map<String, Value>, key: &str| -> bool {
            incoming.get(key).is_none() && md.contains_key(key)
        };
        if synthesized(md, "kegs_out")
            && synthesized(md, "deposit_cents")
            && let Some(out) = md.get("kegs_out").and_then(|v| v.as_i64())
            && out > 0
        {
            md.insert(
                "deposit_cents".to_string(),
                json!(out * KEG_DEPOSIT_CENTS_PER_KEG),
            );
        }
        if synthesized(md, "kegs_returned") && synthesized(md, "kegs_lost") {
            match self.job_kegs_out(job_id) {
                Ok(Some(out)) if out > 0 => {
                    let (returned, lost) = keg_return_split(out as u64, step_id);
                    md.insert("kegs_returned".to_string(), json!(returned));
                    md.insert("kegs_lost".to_string(), json!(lost));
                }
                Ok(_) => {}
                Err(e) => {
                    debug!(%job_id, error = %e, "keg conservation: job fetch failed; leaving synthesized counts");
                }
            }
        }
    }

    /// The fleet's `kegs_out`, read off the job's own earlier step
    /// (the keg-return protocol's log-fleet-out leg). `None` when no
    /// step on the job carries a positive `kegs_out`.
    fn job_kegs_out(&self, job_id: &str) -> Result<Option<i64>> {
        let url = service_url(&self.api_base, &format!("/api/jobs/{job_id}"));
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("GET {url} -> {}", resp.status());
        }
        let job: Value = resp.json().context("decode job")?;
        Ok(job
            .get("steps")
            .and_then(|s| s.as_array())
            .and_then(|steps| {
                steps.iter().find_map(|s| {
                    s.get("metadata")
                        .and_then(|m| m.get("kegs_out"))
                        .and_then(|v| v.as_i64())
                        .filter(|n| *n > 0)
                })
            }))
    }

    /// Stamp a step (POST .../sign-offs) as the executing employee in
    /// the required role. Idempotent server-side per (role, shape).
    fn post_sign_off(&self, job_id: &str, step_id: &str, emp: &str, role: &str) -> Result<()> {
        let url = service_url(
            &self.api_base,
            &format!("/api/jobs/{job_id}/steps/{step_id}/sign-offs"),
        );
        let user = json!({
            "id": emp,
            "role": role,
            "access_tier": "user",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();
        let resp = self
            .client
            .post(&url)
            .header("x-boss-user", user)
            .json(&json!({ "role": role }))
            .send()
            .with_context(|| format!("POST {url}"))?;
        let ok = resp.status().is_success();
        self.record_employee_call(
            "POST",
            &format!("/api/jobs/{job_id}/steps/{step_id}/sign-offs"),
            emp,
            role,
            ok,
        );
        if !ok {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }
        Ok(())
    }

    fn put_step(&self, job_id: &str, step_id: &str, body: &Value, emp: &str) -> Result<()> {
        let path = format!("/api/jobs/{job_id}/steps/{step_id}");
        let url = service_url(&self.api_base, &path);
        let resp = self
            .client
            .put(&url)
            .json(body)
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let ok = resp.status().is_success();
        let role = self.role_of(emp);
        self.record_employee_call("PUT", &path, emp, role, ok);
        if !ok {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("PUT {url} -> {status}: {text}");
        }
        debug!(%url, "workforce step PUT ok");
        Ok(())
    }

    /// True if any `ingredients_consumed` line exceeds real on-hand.
    fn short_on_ingredients(&self, metadata: &Value) -> Result<bool> {
        let Some(items) = metadata
            .get("ingredients_consumed")
            .and_then(|v| v.as_array())
        else {
            return Ok(false);
        };
        for it in items {
            let Some(sku) = it.get("part_sku").and_then(|v| v.as_str()) else {
                continue;
            };
            let qty = it.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
            if self.inventory_on_hand(sku)? < qty {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn inventory_on_hand(&self, sku: &str) -> Result<i64> {
        let url = service_url(&self.api_base, &format!("/api/inventory/items/{sku}"));
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            // Unknown SKU reads as zero on-hand → treat as short.
            return Ok(0);
        }
        let v: Value = resp.json().context("decode inventory item")?;
        Ok(v.get("on_hand").and_then(|x| x.as_i64()).unwrap_or(0))
    }
}

/// `chrono::Duration` from fractional hours (seconds granularity).
fn duration_from_hours(hours: f64) -> Duration {
    Duration::seconds((hours * 3600.0).round() as i64)
}

/// The step's assignee id, or `None` when unassigned/empty. The workforce
/// executes only assigned steps — routing (who does the work) is the
/// dispatcher's layer, not the executor's.
fn assignee_of(step: &Value) -> Option<&str> {
    step.get("assignee_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// `assignee_of`, minus operator identities. The sim workforce simulates
/// the ROSTER — it must never act as a real person's login. The
/// dispatcher may legitimately route a step to an operator (an
/// authority-gated review lands with the platform admin); the workforce
/// leaves it sitting Ready for the human instead of completing their
/// work for them at warp speed.
fn workable_assignee<'a>(
    step: &'a Value,
    excluded: &std::collections::HashSet<String>,
) -> Option<&'a str> {
    assignee_of(step).filter(|emp| !excluded.contains(*emp))
}

/// A reasonable value for a required field, by its StepType `field_type` —
/// what a human executor would put in the box. Type-valid against
/// `boss_jobs::step_registry::validate_field_type`: dates/times get a real
/// instant, enums (pipe-joined) spread deterministically over ALL their
/// variants (see `enum_variant`), and free text gets a readable label
/// derived from the field name.
fn synth_field_value(field_type: &str, name: &str, step_id: &str, now: DateTime<Utc>) -> Value {
    match field_type {
        "number" | "integer" => json!(small_count(name, step_id)),
        "boolean" => json!(true),
        "array" => json!([]),
        "object" => json!({}),
        "date" => json!(now.date_naive().to_string()), // YYYY-MM-DD (len 10)
        "date-time" => json!(now.to_rfc3339()),        // len ≥ 19
        "uri" => json!("https://docs.example.internal/sop"),
        // Enum types are pipe-joined.
        s if s.contains('|') => json!(enum_variant(s, name, step_id)),
        // "string" / "id-ref" / anything else accepts any string.
        _ => json!(humanize(name)),
    }
}

/// Pick one variant of a pipe-joined enum by hash-spreading over the
/// step's identity.
///
/// Always taking the leading variant made every synthesized outcome a
/// constant — all ten closed `tasting-panel` packets carried
/// `verdict: "release"`, so the hold rate read 0% by construction and
/// any outcome-distribution measured over sim traffic measured nothing.
/// Spreading over `(step_id, field)` gives the same step the same answer
/// on every replay while the population visits every variant. The field
/// name is in the key so two enums on one step draw independently.
/// Unweighted and uniform: the sim has no ground truth about how often a
/// real panel holds a batch, and inventing weights would dress a guess
/// up as data.
fn enum_variant<'a>(field_type: &'a str, name: &str, step_id: &str) -> &'a str {
    let variants: Vec<&str> = field_type.split('|').collect();
    let h = fxhash(&format!("{step_id}\u{1f}{name}"));
    // Fold the high half down before the modulus. FNV-1a's low bits are
    // weak — bit 0 is just the parity of the input bytes — so a two-value
    // enum keyed on the raw hash makes every field on a step agree.
    // `split` always yields at least one item, so the modulus is safe.
    let idx = ((h ^ (h >> 32)) as usize) % variants.len();
    variants[idx]
}

/// A small positive count, spread deterministically over `(step, field)`.
///
/// Always emitting `1` made every synthesized number a constant — the
/// same defect the enum faker had before `enum_variant` landed. Measured
/// over 130 closed keg-return packets: 86 carried `kegs_out = 1`, and 76
/// carried `kegs_returned = 1` AND `kegs_lost = 1`, so one keg went out
/// and two came back accounted for. A loss rate over that data measures
/// nothing (feedback 52f49cc7).
///
/// THE RANGE IS ARBITRARY AND SAYS SO. 1..=20 is a plausible small count
/// and nothing more; the sim has no ground truth about how many kegs an
/// order ships, and inventing a distribution would dress a guess up as
/// data — the same reason `enum_variant` is uniform and unweighted.
/// What this fixes is being a CONSTANT, not being wrong.
///
/// IT DOES NOT — AND CANNOT — SATISFY A RELATIONSHIP. No amount of
/// independent per-field synthesis makes `kegs_returned + kegs_lost`
/// equal `kegs_out`; three fields faked separately will disagree, and
/// with a range they will disagree VISIBLY rather than looking tidy at
/// 1/1/1. Fields bound by an invariant need an executor that knows the
/// invariant — which the keg-fleet fields now have:
/// `reconcile_keg_fleet_fields` (93f936b9, David's Q1 decision)
/// overwrites the free-field fakes with a conserved partition of the
/// fleet's own `kegs_out`, and the faker fills only genuinely free
/// fields.
fn small_count(name: &str, step_id: &str) -> u64 {
    let h = fxhash(&format!("{step_id}\u{1f}{name}"));
    // Fold the high half down before the modulus, for the same reason
    // enum_variant does: FNV-1a's low bits are weak, and without the
    // fold two integer fields on one step move together.
    ((h ^ (h >> 32)) % 20) + 1
}

/// A real-world half-barrel keg deposit, in cents per keg ($30 — the
/// industry's standard order of magnitude). Unlike the counts, the
/// deposit HAS ground truth, so the fleet-out leg derives
/// `deposit_cents = kegs_out × this` instead of faking it free-form.
const KEG_DEPOSIT_CENTS_PER_KEG: i64 = 3_000;

/// Partition a fleet's `kegs_out` into `(kegs_returned, kegs_lost)` —
/// the invariant-aware executor the keg ledger's conservation contract
/// requires (93f936b9): the two counts are one draw, not two, so
/// `returned + lost == out` by construction. The loss draw spreads
/// uniformly over `0..=ceil(out/10)` — the industry's own order of
/// magnitude for per-cycle keg loss, stated as a band rather than an
/// invented distribution. Keyed on `(step_id, "kegs_lost")` so a
/// replayed run partitions identically.
fn keg_return_split(kegs_out: u64, step_id: &str) -> (u64, u64) {
    let h = fxhash(&format!("{step_id}\u{1f}kegs_lost"));
    // Fold the high half down before the modulus (see small_count).
    let lost = ((h ^ (h >> 32)) % (kegs_out.div_ceil(10) + 1)).min(kegs_out);
    (kegs_out - lost, lost)
}

/// Small stable string hash (FNV-1a) — NOT `DefaultHasher`, whose seed
/// varies per process and would make a replayed run pick different
/// values than the run it replays. Same shape as the owner-resolution
/// spread in `boss-jobs`.
fn fxhash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `"document_title"` → `"Document title"`: a readable label for a synthetic
/// free-text value.
fn humanize(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// The sim boundary as one checkable question. `true` only when the
/// assignment row SAYS simulated=true; absent, null, or false all read
/// as REAL and are not the sim's to touch (fail closed - defect
/// 88798c96 is what the open version of this predicate did).
fn row_is_simulated(row: &Value) -> bool {
    row.get("simulated").and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    /// A Workforce whose kind-default map says `task` takes 8h, with a
    /// primed spec-duration cache (no HTTP in tests — a cached
    /// (workflow, version) entry short-circuits the registry fetch).
    fn workforce_with_cache(cache: HashMap<String, super::SpecHours>) -> super::Workforce {
        let wf = super::Workforce::new(
            "http://127.0.0.1:9", // never dialed: the cache is primed
            HashMap::from([("task".to_string(), 8.0)]),
            HashMap::new(),
        );
        wf.spec_durations
            .lock()
            .unwrap()
            .insert(("morning-brew".to_string(), 3), cache);
        wf
    }

    fn hours(labor: Option<f64>, wall: Option<f64>, duration: Option<f64>) -> super::SpecHours {
        super::SpecHours {
            labor,
            wall,
            duration,
        }
    }

    fn row_and_step() -> (serde_json::Value, serde_json::Value) {
        let step = json!({ "kind": "task", "spec_slug": "fermentation-start" });
        let row = json!({
            "workflow": "morning-brew",
            "workflow_version": 3,
            "step": step,
        });
        (row.clone(), row["step"].clone())
    }

    #[test]
    fn spec_duration_hours_beats_kind_default() {
        // The step's own spec (via the Job's pinned workflow version)
        // says fermentation takes 168h; the `task` kind default is 8h.
        // The spec wins — this is the fidelity fix that stops a 7-day
        // fermentation "completing" in one workday.
        let wf = workforce_with_cache(HashMap::from([(
            "fermentation-start".to_string(),
            hours(None, None, Some(168.0)),
        )]));
        let (row, step) = row_and_step();
        let pace = super::pacing_hours(wf.spec_hours(&row, &step), wf.duration_hours("task"));
        assert_eq!(pace, 168.0);
    }

    #[test]
    fn kind_default_when_spec_is_silent() {
        // Known workflow version, but the spec authors no duration for
        // this slug → the StepType kind default paces it, exactly as
        // before this field existed.
        let wf = workforce_with_cache(HashMap::new());
        let (row, step) = row_and_step();
        let spec = wf.spec_hours(&row, &step);
        assert_eq!(super::pacing_hours(spec, wf.duration_hours("task")), 8.0);
        // And a kind the durations map doesn't know falls to the
        // DEFAULT_STEP_HOURS floor.
        assert_eq!(
            super::pacing_hours(spec, wf.duration_hours("some-unknown-kind")),
            super::DEFAULT_STEP_HOURS
        );
    }

    /// The split (d64fe2d2): wall-clock beats the pre-split duration
    /// for PACING; labor never paces. A spec authoring all three legs
    /// paces by wall, and its labor commitment is the labor leg alone.
    #[test]
    fn wall_clock_beats_duration_and_labor_never_paces() {
        let spec = Some(hours(Some(0.5), Some(168.0), Some(24.0)));
        assert_eq!(super::pacing_hours(spec, 8.0), 168.0, "wall wins");
        assert_eq!(super::labor_commitment(spec), Some(0.5));
        // Wall absent → the pre-split duration is the wall-clock leg.
        let legacy = Some(hours(None, None, Some(24.0)));
        assert_eq!(super::pacing_hours(legacy, 8.0), 24.0);
        // And CRITICALLY the pre-split duration never reads as labor:
        // a legacy 168h fermentation must not eat 21 days of one
        // person's budget.
        assert_eq!(super::labor_commitment(legacy), None);
        assert_eq!(super::labor_commitment(None), None);
    }

    /// The Q3 norm's boundary is inclusive: a commitment landing
    /// exactly on the cap still fits; a hair over does not.
    #[test]
    fn the_labor_day_boundary_is_inclusive() {
        assert!(super::labor_fits(0.0, 8.0, super::LABOR_DAY_CAP));
        assert!(super::labor_fits(7.5, 0.5, super::LABOR_DAY_CAP));
        assert!(!super::labor_fits(7.5, 0.6, super::LABOR_DAY_CAP));
        assert!(super::labor_fits(0.0, 0.0, super::LABOR_DAY_CAP));
    }

    #[test]
    fn the_boundary_fails_closed() {
        use serde_json::json;
        assert!(super::row_is_simulated(&json!({"simulated": true})));
        assert!(!super::row_is_simulated(&json!({"simulated": false})));
        assert!(!super::row_is_simulated(&json!({})), "absent means real");
        assert!(!super::row_is_simulated(&json!({"simulated": null})));
        assert!(
            !super::row_is_simulated(&json!({"simulated": "true"})),
            "a string is not a claim - fail closed on shape too"
        );
    }

    use super::*;

    #[test]
    fn service_url_maps_ports() {
        assert_eq!(
            service_url("direct://127.0.0.1", "/api/jobs/assignments"),
            "http://127.0.0.1:7900/api/jobs/assignments"
        );
        assert_eq!(
            service_url("scratch://h", "/api/inventory/items/X"),
            "http://h:8300/api/inventory/items/X"
        );
        assert_eq!(
            service_url("https://gw.example", "/api/clock/now"),
            "https://gw.example/api/clock/now"
        );
    }

    #[test]
    fn assignee_of_returns_assigned_skips_unassigned() {
        // Assigned -> Some (the workforce executes it, attributed to them).
        let assigned = json!({ "id": "s1", "assignee_id": "emp-aa-007", "status": "ready" });
        assert_eq!(assignee_of(&assigned), Some("emp-aa-007"));
        // Null / missing / empty -> None: unassigned, so the workforce
        // skips it (the dispatcher or a manager routes it).
        assert_eq!(
            assignee_of(&json!({ "id": "s2", "assignee_id": null })),
            None
        );
        assert_eq!(assignee_of(&json!({ "id": "s3" })), None);
        assert_eq!(assignee_of(&json!({ "id": "s4", "assignee_id": "" })), None);
    }

    #[test]
    fn workable_assignee_skips_operator_identities() {
        // The sim must never impersonate a real operator login. The
        // dispatcher may legitimately route a step to an operator
        // (platform-admin authority → emp-bootstrap-admin — the
        // design-review flow); the workforce's job is to leave it
        // sitting Ready for the human, not to complete their review
        // for them at warp speed (the 2026-07-14 incident: the sim
        // "reviewed" a design doc as emp-bootstrap-admin within
        // seconds, sealing broken step metadata behind
        // terminal-immutability).
        let excluded: std::collections::HashSet<String> =
            ["emp-bootstrap-admin".to_string()].into_iter().collect();

        let operator_step =
            json!({ "id": "s1", "assignee_id": "emp-bootstrap-admin", "status": "ready" });
        assert_eq!(workable_assignee(&operator_step, &excluded), None);

        // Normal roster employees are unaffected.
        let worker_step = json!({ "id": "s2", "assignee_id": "emp-aa-007", "status": "ready" });
        assert_eq!(
            workable_assignee(&worker_step, &excluded),
            Some("emp-aa-007")
        );

        // Empty exclusion set = pre-fix behavior, byte for byte.
        let none: std::collections::HashSet<String> = Default::default();
        assert_eq!(
            workable_assignee(&operator_step, &none),
            Some("emp-bootstrap-admin")
        );
        // Unassigned still skips regardless.
        assert_eq!(
            workable_assignee(&json!({ "id": "s3", "assignee_id": null }), &excluded),
            None
        );
    }

    #[test]
    fn completion_tally_feeds_actor_coverage() {
        use crate::actor_coverage::RoleStatus;
        let emp_roles: HashMap<String, String> = HashMap::from([
            ("emp-1".to_string(), "brewer".to_string()),
            ("emp-2".to_string(), "brewer".to_string()),
            ("emp-3".to_string(), "shipping-clerk".to_string()),
            ("emp-admin".to_string(), "platform-admin".to_string()),
        ]);
        let wf = Workforce::new("http://127.0.0.1:9", HashMap::new(), HashMap::new())
            .with_actor_telemetry(crate::api_activity::new_handle(), emp_roles)
            .with_excluded_assignees(["emp-admin".to_string()]);

        // Before any completion: both simulatable roles are dormant and
        // platform-admin reads operator — never dormant.
        let cov = wf.actor_coverage();
        assert_eq!(cov.roles_acting, 0);
        assert_eq!(cov.roles_dormant, 2);
        assert_eq!(cov.roles_operator, 1);
        assert_eq!(cov.employees_total, 4);
        assert_eq!(cov.employees_operator, 1);

        // Two completions by emp-1 + one by emp-2 → brewer acts:
        // 2 distinct people, 3 steps.
        wf.note_completion("emp-1");
        wf.note_completion("emp-1");
        wf.note_completion("emp-2");
        let cov = wf.actor_coverage();
        assert_eq!(cov.employees_acting, 2);
        let brewer = cov.roles.iter().find(|r| r.role == "brewer").unwrap();
        assert_eq!(brewer.acting, 2);
        assert_eq!(brewer.completions, 3);
        assert_eq!(brewer.status, RoleStatus::Acting);
        // The clerk roster still hasn't acted — visible dormant.
        let clerk = cov
            .roles
            .iter()
            .find(|r| r.role == "shipping-clerk")
            .unwrap();
        assert_eq!(clerk.status, RoleStatus::Dormant);
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2025-04-03T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn synth_field_value_is_type_valid_per_kind() {
        let now = fixed_now();
        let step = "step-1";
        // Free text -> a readable label derived from the field name.
        assert_eq!(
            synth_field_value("string", "document_title", step, now),
            json!("Document title")
        );
        // date-time -> RFC3339 (the validator requires len >= 19).
        assert!(
            synth_field_value("date-time", "scheduled_at", step, now)
                .as_str()
                .unwrap()
                .len()
                >= 19
        );
        // date -> YYYY-MM-DD (the validator requires len == 10).
        assert_eq!(
            synth_field_value("date", "due", step, now)
                .as_str()
                .unwrap()
                .len(),
            10
        );
        // enum -> one of the declared variants (which one is the step's
        // own deterministic draw; see enum_choice_* below).
        let picked = synth_field_value("pass|fail|conditional", "result", step, now);
        assert!(matches!(
            picked.as_str(),
            Some("pass" | "fail" | "conditional")
        ));
        // scalars are the right JSON shape.
        assert!(synth_field_value("integer", "n", step, now).is_i64());

        // NOT A CONSTANT, and deterministic. Before this, every
        // synthesized number was 1 — 86 of 130 keg-return packets
        // carried kegs_out = 1 (feedback 52f49cc7).
        let a = synth_field_value("integer", "kegs_out", "step-a", now);
        let b = synth_field_value("integer", "kegs_out", "step-b", now);
        let c = synth_field_value("integer", "kegs_lost", "step-a", now);
        assert_ne!(a, b, "the same field on different steps must spread");
        assert_ne!(
            a, c,
            "two integer fields on ONE step must draw independently, or \
             they move together and a relationship looks satisfied by accident"
        );
        assert_eq!(
            a,
            synth_field_value("integer", "kegs_out", "step-a", now),
            "same (step, field) must replay identically — a sim that picks \
             new numbers on replay is not reproducible"
        );
        for step in ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"] {
            let v = synth_field_value("integer", "n", step, now)
                .as_i64()
                .unwrap();
            assert!((1..=20).contains(&v), "{v} out of the stated range");
        }
        assert!(synth_field_value("boolean", "b", step, now).is_boolean());
        assert!(synth_field_value("array", "xs", step, now).is_array());
        assert!(synth_field_value("object", "o", step, now).is_object());
        assert!(synth_field_value("uri", "u", step, now).is_string());
    }

    /// Replay determinism: the same step always synthesizes the same
    /// enum value, on any process and at any clock reading. The choice
    /// is a function of the step's identity, nothing else.
    #[test]
    fn enum_choice_is_stable_for_a_step_id() {
        let now = fixed_now();
        let first = synth_field_value("release|hold", "verdict", "step-7f3a2b", now);
        assert!(matches!(first.as_str(), Some("release" | "hold")));
        for _ in 0..64 {
            assert_eq!(
                synth_field_value("release|hold", "verdict", "step-7f3a2b", now),
                first,
                "same step id must always yield the same choice"
            );
        }
        // The clock is not part of the key — a replay that lands on a
        // different sim instant must still make the same choice.
        assert_eq!(
            synth_field_value(
                "release|hold",
                "verdict",
                "step-7f3a2b",
                now + Duration::hours(37)
            ),
            first
        );
    }

    /// The defect this fixes: ten closed `tasting-panel` packets all
    /// carried `verdict: "release"`, so the hold rate read 0% by
    /// construction. A spread that never leaves the first variant is
    /// the same constant with extra steps.
    #[test]
    fn enum_choice_visits_every_variant_across_step_ids() {
        let now = fixed_now();
        for field_type in ["release|hold", "pass|fail|conditional", "a|b|c|d"] {
            let variants: Vec<&str> = field_type.split('|').collect();
            let mut counts: HashMap<String, usize> = HashMap::new();
            for i in 0..400 {
                let step_id = format!("01994a7f-{i:04}-7c1e-9d2b-6f0a1c3d5e7{}", i % 10);
                let v = synth_field_value(field_type, "verdict", &step_id, now);
                let s = v.as_str().expect("enum synthesizes a string").to_string();
                assert!(
                    variants.contains(&s.as_str()),
                    "{s} is not a declared variant"
                );
                *counts.entry(s).or_default() += 1;
            }
            assert_eq!(
                counts.len(),
                variants.len(),
                "{field_type}: only visited {:?}",
                counts.keys().collect::<Vec<_>>()
            );
            // Not merely surjective — a variant that shows up twice in
            // 400 draws still makes a rate measurement noise. Each must
            // carry at least half its uniform share.
            let floor = 400 / (variants.len() * 2);
            for (value, n) in &counts {
                assert!(*n >= floor, "{field_type}: {value} drew {n} of 400");
            }
        }
    }

    /// Two enum fields on one step must not move in lockstep — the field
    /// name is part of the key, so a step's `verdict` and its `route`
    /// are independent draws.
    #[test]
    fn enum_choice_is_per_field_not_only_per_step() {
        let now = fixed_now();
        let differ = (0..64).any(|i| {
            let step_id = format!("step-{i:03}");
            synth_field_value("release|hold", "verdict", &step_id, now)
                != synth_field_value("release|hold", "route", &step_id, now)
        });
        assert!(differ, "every step drew the same value for both fields");
    }

    /// The invariant-aware half of keg-fleet synthesis (93f936b9): a
    /// fleet's returned + lost must equal what went out — the faker
    /// fills only genuinely free fields, and the partition is derived.
    #[test]
    fn keg_return_split_conserves_and_replays() {
        for out in 1u64..=40 {
            for i in 0..8 {
                let step_id = format!("step-{i}");
                let (returned, lost) = keg_return_split(out, &step_id);
                assert_eq!(
                    returned + lost,
                    out,
                    "out={out} step={step_id}: partition must conserve"
                );
                // The loss band is 0..=ceil(out/10) — the industry's own
                // order of magnitude, not a precise claim.
                assert!(lost <= out.div_ceil(10), "out={out} lost={lost}");
                // Same (step, out) replays identically.
                assert_eq!((returned, lost), keg_return_split(out, &step_id));
            }
        }
        // Not a constant: across a population, losses actually occur.
        let some_loss = (0..64).any(|i| keg_return_split(20, &format!("s-{i}")).1 > 0);
        assert!(some_loss, "no fleet ever lost a keg across 64 draws");
    }

    #[test]
    fn fill_required_fields_fills_missing_keeps_existing() {
        let mut req = HashMap::new();
        req.insert(
            "acknowledgment".to_string(),
            vec![
                RequiredField {
                    name: "document_title".into(),
                    field_type: "string".into(),
                },
                RequiredField {
                    name: "acknowledged_at".into(),
                    field_type: "date-time".into(),
                },
            ],
        );
        let wf = Workforce::new("direct://127.0.0.1", HashMap::new(), req);
        let now = fixed_now();

        let mut md = serde_json::Map::new();
        md.insert("document_title".to_string(), json!("Q2 Safety Policy"));
        wf.fill_required_fields("acknowledgment", &mut md, "step-1", now);
        // Pre-set key is preserved (the Workflow's default wins).
        assert_eq!(
            md.get("document_title").unwrap(),
            &json!("Q2 Safety Policy")
        );
        // Missing required key is filled.
        assert!(md.get("acknowledged_at").is_some());

        // Unknown kind -> no-op.
        let mut empty = serde_json::Map::new();
        wf.fill_required_fields("not-a-kind", &mut empty, "step-2", now);
        assert!(empty.is_empty());
    }
}
