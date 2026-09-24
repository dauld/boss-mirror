//! `TenantConfig` — TOML loader for the per-tenant sim config.
//!
//! Mirrors `examples/brewery/seeds/tenant.toml` exactly. Every
//! shape-driven sim run is parameterized by one of these files; the
//! file authors how often each Workflow fires, which Subjects birth
//! at what rate, and how aggressive anomalies should be. No Rust
//! changes needed to add a new tenant — just a new directory of
//! seeds with a tenant.toml inside.

use std::collections::HashMap;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TenantConfigError {
    #[error("reading tenant config {0}: {1}")]
    Io(String, String),
    #[error("parsing tenant config {0}: {1}")]
    Parse(String, String),
    #[error("validation: {0}")]
    Validation(String),
}

/// Top-level tenant config. Section ordering mirrors the TOML's
/// authored layout so the file is the doc.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TenantConfig {
    pub meta: TenantMeta,
    /// Per-Workflow creation rates. Key is the Workflow slug
    /// (`"morning-bake"`, `"wholesale-order"`, …).
    #[serde(default)]
    pub job_rates: HashMap<String, JobRate>,
    /// Per-SubjectKind birth rates. Key is the SubjectKind slug
    /// (`"account"`, `"vendor"`, …).
    #[serde(default)]
    pub subject_rates: HashMap<String, SubjectRate>,
    /// Per-Workflow anomaly probabilities. Key matches `job_rates`.
    /// Values are arbitrary `prob_name -> [0, 1]` pairs — sim code
    /// reads the keys it knows about.
    #[serde(default)]
    pub anomalies: HashMap<String, AnomalyRates>,
    /// CounterpartyEngine specs. Key is the spec name; the spec
    /// itself uses the same key as its `name` so flattened follow-up
    /// hops carry stable provenance. See
    /// `crates/boss-sim/src/engines/counterparty.rs`.
    #[serde(default)]
    pub counterparty: HashMap<String, CounterpartyToml>,
    /// Random shocks — quarterly probabilistic events that bump a
    /// Workflow's rate (or one Subject's draws within a Workflow) for
    /// a fixed window. Models real-world disruption: a viral bar
    /// doubles its order rate for a few weeks, a supply-chain hiccup
    /// dampens production for a month. Empty by default — tenants
    /// opt in.
    #[serde(default)]
    pub shock: Vec<ShockSpec>,
    /// PeriodicEngine specs. Key is the spec name; one row per
    /// cadence (daily / weekly / biweekly / monthly / quarterly /
    /// annually). Action shape mirrors `PeriodicAction`.
    #[serde(default)]
    pub periodic: HashMap<String, PeriodicToml>,
    // BatchEngine specs retired 2026-05-06 — see TODO.md
    // "Strip BatchEngine". The four [batch.*] rows that lived
    // here (payroll, sales-tax, payroll-941, income-tax) bypassed
    // the Workflow / Step / audit-trail by emitting canonical
    // events directly. Replaced by [periodic.*] specs that open
    // honest Workflows whose terminal step's side-effect handler
    // POSTs to the canonical /api/ledger/* endpoint. The field
    // stays as untyped values so tenant.tomls still carrying old
    // [batch.*] blocks parse without error during the migration
    // window — the engine ignores them entirely.
    #[serde(default)]
    pub batch: HashMap<String, toml::Value>,
}

/// TOML side of `PeriodicSpec`. See
/// `crates/boss-sim/src/engines/periodic.rs`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PeriodicToml {
    pub cadence: crate::engines::Cadence,
    pub anchor_date: NaiveDate,
    #[serde(default)]
    pub business_calendar: Option<String>,
    pub action: PeriodicActionToml,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeriodicActionToml {
    OpenJob {
        workflow: String,
        #[serde(default)]
        subject_kind: Option<String>,
        #[serde(default)]
        subject_id: Option<String>,
    },
    EmitEvent {
        topic: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

/// TOML side of `CounterpartySpec`. Carries the same fields plus
/// `name` is implicit (the section key). Keeps the engine spec types
/// independent of the TOML loader.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CounterpartyToml {
    /// Real-world actor kind this chain represents — `account` |
    /// `vendor` | `bank` | `environment`. Drives the cockpit grouping.
    #[serde(default)]
    pub actor_kind: Option<String>,
    pub listens_to: String,
    pub delay: DelayToml,
    #[serde(default = "default_one")]
    pub emit_probability: f64,
    pub emits: String,
    /// Optional alternate emit on a failed `emit_probability`
    /// roll. See `engines::CounterpartySpec::emit_else`.
    #[serde(default)]
    pub emit_else: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub followups: Vec<NamedCounterpartyToml>,
    /// `[[counterparty.<name>.scans]]` blocks. Mutually exclusive
    /// with `followups`; loader doesn't validate (the engine panics
    /// at flatten time if both are populated).
    #[serde(default)]
    pub scans: Vec<ScanToml>,
    /// Optional payload predicate — see
    /// `engines::CounterpartySpec::match_payload` for the semantics.
    #[serde(default)]
    pub match_payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScanToml {
    pub status: String,
    pub delay: DelayToml,
}

fn default_one() -> f64 {
    1.0
}

/// A followup hop. Same as `CounterpartyToml` but with an explicit
/// `name` since followups live inside an array, not a map.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NamedCounterpartyToml {
    pub name: String,
    pub listens_to: String,
    pub delay: DelayToml,
    #[serde(default = "default_one")]
    pub emit_probability: f64,
    pub emits: String,
    #[serde(default)]
    pub emit_else: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub followups: Vec<NamedCounterpartyToml>,
    #[serde(default)]
    pub scans: Vec<ScanToml>,
    /// Optional `match_payload` predicate — see
    /// `engines::CounterpartySpec::match_payload` for the
    /// semantics. Authored as inline TOML, e.g.
    ///   match_payload = { "metadata.vendor.category" = "dairy" }
    #[serde(default)]
    pub match_payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DelayToml {
    pub mean_days: f64,
    #[serde(default)]
    pub spread_days: f64,
    #[serde(default)]
    pub business_calendar: Option<String>,
}

fn named_to_spec(n: &NamedCounterpartyToml) -> crate::engines::CounterpartySpec {
    crate::engines::CounterpartySpec {
        name: n.name.clone(),
        // Followups inherit the root chain's actor kind via the qualified
        // name's root segment, so leave this unset here.
        actor_kind: None,
        listens_to: n.listens_to.clone(),
        delay: crate::engines::DelaySpec {
            mean_days: n.delay.mean_days,
            spread_days: n.delay.spread_days,
            business_calendar: n.delay.business_calendar.clone(),
        },
        emit_probability: n.emit_probability,
        emits: n.emits.clone(),
        emit_else: n.emit_else.clone(),
        payload: n.payload.clone(),
        followups: n.followups.iter().map(named_to_spec).collect(),
        scans: n.scans.iter().map(scan_toml_to_spec).collect(),
        match_payload: n.match_payload.clone(),
    }
}

fn scan_toml_to_spec(s: &ScanToml) -> crate::engines::ScanSpec {
    crate::engines::ScanSpec {
        status: s.status.clone(),
        delay: crate::engines::DelaySpec {
            mean_days: s.delay.mean_days,
            spread_days: s.delay.spread_days,
            business_calendar: s.delay.business_calendar.clone(),
        },
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TenantMeta {
    pub tenant_id: String,
    pub display_name: String,
    /// PRNG seed — the sim is bit-deterministic for a given tenant
    /// + seed pair so rerunning produces the same output.
    pub seed: u32,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Operating days as kebab abbreviations (`mon`, `tue`, …). When
    /// empty, every day is an operating day. Sim skips Job creation
    /// on non-operating days (closed bakery → no morning bake).
    #[serde(default)]
    pub operating_days: Vec<String>,
    /// Per-weekday open/close windows for sub-day tick gating —
    /// Phase B-2 of the sub-day-tick rollout. When set, ticks
    /// whose hour-of-day falls outside the window for the day's
    /// weekday skip the shape-driven engine's Job-creation +
    /// step-advancement passes (people aren't working). When
    /// absent, every operating-day tick is in-hours.
    ///
    /// TOML shape:
    ///
    /// ```toml
    /// [meta.operating_hours]
    /// mon = "08:00-18:00"
    /// tue = "08:00-18:00"
    /// wed = "08:00-18:00"
    /// thu = "08:00-18:00"
    /// fri = "08:00-18:00"
    /// sat = "10:00-14:00"
    /// # sun absent → closed all day
    /// ```
    ///
    /// At day-tick (`tick_duration = "1d"`) this field has no
    /// observable effect: the day-tick covers `[0, 24)` and
    /// `is_operating_at_tick` returns true for any in-window
    /// HH:MM. At hourly ticks the engine fires only on the
    /// 10 in-hours ticks per workday instead of all 24.
    ///
    /// Counterparty / Periodic / Batch engines still fire on
    /// the first-of-day tick regardless of operating_hours
    /// (they're calendar-anchored, not human-scheduled). Only
    /// the HumanWorker layer (Job creation + step advancement)
    /// gates on the window.
    ///
    /// `operating_hours` is independent of `operating_days`:
    /// a tenant with no `operating_days` filter still observes
    /// `operating_hours` if set. Operators typically use one or
    /// the other; both is fine — a missing-or-empty window for
    /// a weekday means closed that day.
    #[serde(default)]
    pub operating_hours: HashMap<String, String>,
    /// Workforce execution speed — a multiplier on every step's typical
    /// duration. `<1` makes the workforce complete assigned steps faster
    /// (shorter durations), `>1` slower. `None`/absent = 1.0 (registry
    /// durations as-authored). A control-plane knob (the "Workforce
    /// execution" category); applied in `build_workforce`.
    #[serde(default)]
    pub step_speed_multiplier: Option<f64>,
    /// Sim-clock granularity — how much sim-time each tick advances.
    /// Accepted forms: `"1d"` (default; legacy day-tick behavior),
    /// `"1h"` (24 ticks/day), `"30m"` (48 ticks/day), `"15m"`
    /// (96 ticks/day), `"1m"` (1440 ticks/day). At sub-day
    /// granularity the same per-day expected volume holds — the
    /// tick-aware sampler scales lambda by `tick.day_fraction()`
    /// so 24×1h ticks fire roughly the same Job count as 1×1d
    /// (see `engines::tick::Tick` for the math).
    ///
    /// Both Phase B-1 (sub-day tick infrastructure) and Phase B-2
    /// (sub-day-aware gating) are shipped:
    /// - `operating_hours` gates the HumanWorker layer per tick
    ///   via `is_operating_at_tick`
    ///   (called at `shape_driven::engine::simulate_tick_with_handlers`)
    /// - `subject_cadence.time_of_day` anchors deterministic
    ///   per-Subject Job creation to the configured hour-of-day
    ///   via `SubjectCadence::fires_on_tick`
    ///   (called at `shape_driven::engine::simulate_tick_with_handlers`)
    /// - Sub-day periodic cadences (`Hourly`, `EveryNMinutes`)
    ///   fire on the right tick within the day via
    ///   `Cadence::fires_on_tick` (called at
    ///   `engines::periodic::PeriodicEngine::step`)
    ///
    /// Counterparty + Batch engines remain day-anchored — they
    /// fire on `tick.is_first_in_day` because their semantics are
    /// calendar-driven (clearing-day batches, payroll runs,
    /// quarterly tax filings). Day-anchored cadences (Daily /
    /// Weekly / Monthly / Quarterly / Annually) gate on
    /// `is_first_in_day` too — `fires_on_tick` returns false for
    /// non-first-in-day ticks so the per-day fire happens once.
    #[serde(default = "default_tick_duration")]
    pub tick_duration: String,
}

fn default_tick_duration() -> String {
    "1d".to_string()
}

impl TenantMeta {
    /// True iff `day`'s weekday matches `operating_days` (or the
    /// list is empty, meaning open every day).
    pub fn is_operating_day(&self, day: NaiveDate) -> bool {
        if self.operating_days.is_empty() {
            return true;
        }
        let abbrev = match day.format("%a").to_string().to_lowercase().as_str() {
            "mon" => "mon",
            "tue" => "tue",
            "wed" => "wed",
            "thu" => "thu",
            "fri" => "fri",
            "sat" => "sat",
            "sun" => "sun",
            _ => return false,
        };
        self.operating_days.iter().any(|d| d == abbrev)
    }

    /// True iff the tick's hour-of-day window overlaps the
    /// per-weekday open hours configured in `operating_hours`.
    /// When `operating_hours` is empty (the default), every
    /// in-operating-day tick is in-hours so this returns true
    /// regardless of tick. Phase B-2 of the sub-day-tick rollout.
    ///
    /// Window format: `"HH:MM-HH:MM"` (e.g. `"08:00-18:00"`).
    /// Open at start, closed at end (half-open interval). A
    /// missing weekday key = closed all day for that weekday.
    ///
    /// At day-tick the day-tick covers `[0, 24)` so any window
    /// entry overlaps → returns true if the day has any window.
    /// At sub-day ticks, only ticks that overlap the window
    /// return true.
    pub fn is_operating_at_tick(
        &self,
        day: chrono::NaiveDate,
        tick: &crate::engines::Tick,
    ) -> bool {
        if !self.is_operating_day(day) {
            return false;
        }
        if self.operating_hours.is_empty() {
            return true;
        }
        let abbrev = match day.format("%a").to_string().to_lowercase().as_str() {
            "mon" => "mon",
            "tue" => "tue",
            "wed" => "wed",
            "thu" => "thu",
            "fri" => "fri",
            "sat" => "sat",
            "sun" => "sun",
            _ => return false,
        };
        let Some(window) = self.operating_hours.get(abbrev) else {
            return false;
        };
        let parts: Vec<&str> = window.split('-').collect();
        if parts.len() != 2 {
            return false;
        }
        let open = match crate::engines::parse_hh_mm(parts[0]) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let close = match crate::engines::parse_hh_mm(parts[1]) {
            Ok(h) => h,
            Err(_) => return false,
        };
        // Tick window: [tick.start_hour, tick.start_hour + duration).
        // Operating window: [open, close).
        // They overlap iff tick.start < close AND tick.end > open.
        let tick_end = tick.start_hour_of_day + tick.duration_hours;
        tick.start_hour_of_day < close && tick_end > open
    }

    /// Number of ticks per sim-day from the `tick_duration` field.
    /// Returns 1 (legacy day-tick) when `tick_duration` is `"1d"`
    /// or absent. Returns 24 for `"1h"`, 96 for `"15m"`, etc.
    ///
    /// Per-tenant-engine callers (`boss-brewery-engine`,
    /// `boss-used-device-shop-engine`) pass this to
    /// `boss_sim::engines::run_ticks_with_handlers` so the sim
    /// loop ticks at the configured granularity. Invalid values
    /// surface during config parse via `parse_tick_duration` —
    /// not at engine-runtime — so the engine call site can
    /// trust the result.
    pub fn ticks_per_day(&self) -> u32 {
        parse_tick_duration(&self.tick_duration).unwrap_or(1)
    }
}

/// Parse `[meta] tick_duration` to ticks-per-day.
///
/// Accepts `"1d"`, `"1h"`, `"30m"`, `"15m"`, `"5m"`, `"1m"`. Returns
/// `Err` for malformed input or values that don't divide a sim-day
/// evenly (since the day_runner would emit irregular ticks).
pub fn parse_tick_duration(s: &str) -> Result<u32, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("tick_duration must not be empty".into());
    }
    let (num_part, unit) = trimmed.split_at(trimmed.len() - 1);
    let n: u32 = num_part
        .parse()
        .map_err(|_| format!("tick_duration `{trimmed}`: leading digits must parse as u32"))?;
    if n == 0 {
        return Err(format!("tick_duration `{trimmed}`: count must be > 0"));
    }
    match unit {
        "d" => {
            if n != 1 {
                return Err(format!(
                    "tick_duration `{trimmed}`: only `1d` supported (a tick can't span multiple sim-days)"
                ));
            }
            Ok(1)
        }
        "h" => {
            if 24 % n != 0 {
                return Err(format!(
                    "tick_duration `{trimmed}`: hourly count must divide 24 evenly"
                ));
            }
            Ok(24 / n)
        }
        "m" => {
            let total_minutes = 24 * 60;
            if total_minutes % n != 0 {
                return Err(format!(
                    "tick_duration `{trimmed}`: minute count must divide 1440 evenly"
                ));
            }
            Ok(total_minutes / n)
        }
        other => Err(format!(
            "tick_duration `{trimmed}`: unit `{other}` must be one of d / h / m"
        )),
    }
}

/// Per-Workflow rate spec. The base `rate` is jobs/day; ramps
/// override that rate from a given date onward; weekday/weekend
/// multipliers tilt the curve.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JobRate {
    /// Steady-state Jobs/day at the latest ramp point (or, if no
    /// ramps, throughout the sim).
    pub rate: f64,
    /// Ordered list of `{date, rate}` pairs. The active rate on a
    /// given day is the latest ramp whose `date <= day`, or `rate`
    /// if no ramps apply.
    #[serde(default)]
    pub ramp: Vec<RampPoint>,
    /// Multiplier applied on Mon–Fri. `None` ⇒ 1.0.
    #[serde(default)]
    pub weekday_multiplier: Option<f64>,
    /// Multiplier applied on Sat–Sun. `None` ⇒ 1.0.
    #[serde(default)]
    pub weekend_multiplier: Option<f64>,
    /// Optional per-month-of-year multipliers (key = "1" through
    /// "12"). Missing entries default to 1.0. Multiplies on top of
    /// the weekday/weekend multiplier so a December release surge
    /// can stack with the weekend skew. Useful for seasonal demand
    /// curves: a brewery sees more wholesale orders in summer +
    /// December, fewer in mid-winter.
    ///
    /// Keys are strings because TOML inline-table keys are always
    /// strings — `effective_rate_for_day` parses the current month
    /// to look up.
    #[serde(default)]
    pub month_multipliers: HashMap<String, f64>,
    /// Optional Subject distribution: when a Job of this kind is
    /// created, draw the Subject id from this probability map. Keys
    /// are Subject ids; values sum to ≤ 1.0. Empty ⇒ uniform over
    /// every active Subject of a matching kind.
    #[serde(default)]
    pub subject_distribution: HashMap<String, f64>,
    /// Per-Subject deterministic cadence overrides.
    ///
    /// Entries fire **in addition** to the Poisson draw — a real
    /// brewery has both ad-hoc orders (the Poisson noise) and
    /// regulars who order every Tuesday morning regardless. Tenants
    /// model the regulars as cadence rows and tune `rate` down to
    /// match the residual ad-hoc volume.
    ///
    /// Each row triggers a guaranteed Job for `subject_id` whenever
    /// the day matches `weekday` and the biweekly filter (if any)
    /// agrees. See `SubjectCadence` for details.
    #[serde(default)]
    pub subject_cadence: Vec<SubjectCadence>,
    /// Deterministic-creation flag. When `true`, the sampler stops
    /// Poisson-sampling this kind and instead creates the *rounded
    /// effective rate* number of Jobs once per sim-day — a fixed
    /// review that fires every working day, never zero by chance.
    ///
    /// Built for production-review Workflows (the five
    /// `morning-brew*` daily brews): a Poisson(λ) draw is 0 with
    /// probability `e^−λ`, so a low-rate brew can go many days
    /// without a single review — the cold-start drought that
    /// emptied Stout finished-goods ~day 28 in the 365-day regen.
    /// A daily deterministic review makes that drought impossible;
    /// the in-flight-aware demand gate (not the sampler) then
    /// decides whether each review brews or skips.
    ///
    /// The count is `effective_rate_for_day(jr, today).round()`,
    /// which already folds ramps, `weekday_multiplier`,
    /// `weekend_multiplier`, `month_multipliers`, and US-federal-
    /// holiday suppression (holidays read `weekend_multiplier`).
    /// So a brew configured `rate = 1.0`, `weekday_multiplier =
    /// 1.0`, `weekend_multiplier = 0.0` reviews exactly once each
    /// Mon–Fri and not at all on weekends or holidays — the
    /// weekend/holiday exclusion comes free, with no per-rate
    /// holiday list.
    ///
    /// Day-anchored: the whole day's count fires on the
    /// `is_first_in_day` tick (unscaled), so at sub-day tick
    /// granularity the daily total stays the rounded effective
    /// rate instead of multiplying by the tick count. Defaults
    /// `false` — every existing rate keeps Poisson behavior.
    #[serde(default)]
    pub deterministic: bool,
}

/// One declarative shock — rolled at every quarter start
/// (Jan 1 / Apr 1 / Jul 1 / Oct 1) with probability
/// `probability_per_quarter`; if it lands, the engine pushes an
/// `ActiveShock` onto `ShapeDrivenState.active_shocks` with an end
/// date `duration_weeks * 7` days out, and applies the multiplier
/// to the matching rate until the window closes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShockSpec {
    /// Authoring name; surfaces in logs + per-shock counters.
    pub name: String,
    /// Probability the shock fires at any given quarter boundary,
    /// in `[0.0, 1.0]`. 0.05 ≈ once a quarter on average over a
    /// 5-year run.
    pub probability_per_quarter: f64,
    /// What the multiplier applies to. See `ShockTarget`.
    pub target: ShockTarget,
    /// Multiplier applied to the matched rate(s). Values <1 dampen,
    /// >1 surge.
    pub rate_multiplier: f64,
    /// How long the shock stays active, in weeks.
    pub duration_weeks: u32,
}

/// What a shock multiplies. Four shapes:
///
/// - `kind = "..."` — multiplier applies to every Job of that
///   Workflow. Models a supply-chain shock dampening every brew, or
///   a launch surge boosting every direct-shop-order.
///
/// - `kind = "..."`, `subject_id = "..."` — multiplier applies only
///   to draws targeting that Subject within the Workflow. Models the
///   "viral bar" — one Account's wholesale-keg-order rate doubles
///   for a few weeks.
///
/// - `kind = "..."`, `subject_id_pattern = "acc-bigseed-*"` — same
///   shape but the engine picks one matching subject_id at random
///   from the pool when the shock activates. Lets a tenant author
///   "any random regular bar goes viral" without listing every
///   candidate.
///
/// - `counterparty = "..."` — multiplier applies to the sampled
///   delay of every emission from that CounterpartySpec (by name).
///   Models "supplier is slow this quarter", "carrier is slammed",
///   "ACH settling is degraded". Mutually exclusive with `kind`.
///   `rate_multiplier > 1.0` = longer delays, `< 1.0` = faster.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShockTarget {
    /// Workflow slug to apply against. Mutually exclusive with
    /// `counterparty`; exactly one must be set.
    #[serde(default)]
    pub kind: Option<String>,
    /// When set, multiplier only applies to that subject's draws.
    /// Mutually exclusive with `subject_id_pattern`; `kind`-only
    /// shocks (no subject) hit every draw. Only meaningful when
    /// `kind` is set.
    #[serde(default)]
    pub subject_id: Option<String>,
    /// Glob-ish pattern (only `*` suffix supported in v1) matched
    /// against the subject pool at activation time. Engine picks
    /// one match uniformly at random and locks it in for the
    /// shock's lifetime. Only meaningful when `kind` is set.
    #[serde(default)]
    pub subject_id_pattern: Option<String>,
    /// CounterpartySpec name. When set, the shock's
    /// `rate_multiplier` is applied to the sampled delay of every
    /// emission from that spec. Mutually exclusive with `kind`.
    #[serde(default)]
    pub counterparty: Option<String>,
}

impl ShockTarget {
    /// True iff this target matches a draw of `kind` for
    /// `subject_id`. Used by the JobRate path. Counterparty-targeted
    /// shocks always return false here — they're matched via
    /// `matches_counterparty` instead.
    pub fn matches(&self, kind: &str, subject_id: &str) -> bool {
        let Some(self_kind) = self.kind.as_deref() else {
            return false;
        };
        if self_kind != kind {
            return false;
        }
        match (
            self.subject_id.as_deref(),
            self.subject_id_pattern.as_deref(),
        ) {
            (Some(want), _) => want == subject_id,
            (None, Some(pat)) => match pat.strip_suffix('*') {
                Some(prefix) => subject_id.starts_with(prefix),
                None => pat == subject_id,
            },
            (None, None) => true,
        }
    }

    /// True iff this target names the given counterparty spec.
    /// CounterpartyEngine consults this when scaling sampled delays.
    pub fn matches_counterparty(&self, name: &str) -> bool {
        self.counterparty.as_deref() == Some(name)
    }
}

/// A "this Account orders every Tuesday morning"-shaped row.
///
/// Drives realistic clustering: a brewery's wholesale-keg-order
/// flow shouldn't be uniform across accounts — Sierra Networks
/// orders Mon, Crown Pub orders Thu, etc. The Poisson sampler
/// above gives ad-hoc orders; this gives the deterministic
/// regulars.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubjectCadence {
    /// Subject id this cadence applies to. Matched against the
    /// SubjectKind declared in the Workflow's `subject_kinds[0]`
    /// (same convention the Poisson path uses).
    pub subject_id: String,
    /// Weekday name: `Mon` / `Tue` / `Wed` / `Thu` / `Fri` /
    /// `Sat` / `Sun` (or the long form). Case-insensitive.
    pub weekday: String,
    /// Biweekly filter — when set, only fire on weeks whose
    /// ISO-week-number `% 2 == biweekly_offset`. Default `None`
    /// fires every matching weekday. Use `0` for even-week
    /// regulars, `1` for odd-week.
    #[serde(default)]
    pub biweekly_offset: Option<u32>,
    /// Optional `"HH:MM"` anchor — at sub-day tick granularity,
    /// fire on the tick whose window covers this hour-of-day.
    /// `None` defaults to midnight (`"00:00"`), matching the
    /// pre-Phase-B-2 behavior where cadence rows always fired
    /// on tick 0 of the matching weekday. Phase B-2 of the
    /// sub-day-tick rollout — see TODO.md "Hourly tick
    /// granularity" + `engines::tick::parse_hh_mm`.
    ///
    /// At day-tick (`tick_duration = "1d"`) this field has no
    /// observable effect: the day tick covers `[0, 24)` so
    /// `Tick::covers_hour(target)` returns true for any HH:MM.
    /// At hourly ticks the cadence row fires only on the tick
    /// covering the configured hour ("Sierra Networks orders
    /// every Tuesday at 09:00" instead of "every Tuesday at
    /// midnight").
    #[serde(default)]
    pub time_of_day: Option<String>,
}

impl SubjectCadence {
    /// True iff this cadence row fires on `today` AND the
    /// configured hour-of-day falls within `tick`'s window.
    /// Phase B-2 — sub-day cadence anchoring.
    ///
    /// Day-granularity gates run first (weekday match + biweekly
    /// parity — cheap). At day-tick granularity every
    /// `time_of_day` lands inside the day's `[0, 24)` window, so
    /// the hour-check is identity. At hourly ticks the row fires
    /// once per matching weekday at the right hour.
    /// `time_of_day = None` (the common case) anchors at midnight
    /// — the day's first tick.
    pub fn fires_on_tick(&self, today: chrono::NaiveDate, tick: &crate::engines::Tick) -> bool {
        use chrono::{Datelike, Weekday};
        let want = match self.weekday.to_lowercase().as_str() {
            "mon" | "monday" => Weekday::Mon,
            "tue" | "tues" | "tuesday" => Weekday::Tue,
            "wed" | "weds" | "wednesday" => Weekday::Wed,
            "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
            "fri" | "friday" => Weekday::Fri,
            "sat" | "saturday" => Weekday::Sat,
            "sun" | "sunday" => Weekday::Sun,
            _ => return false,
        };
        if today.weekday() != want {
            return false;
        }
        if let Some(offset) = self.biweekly_offset {
            let week = today.iso_week().week();
            if week % 2 != offset % 2 {
                return false;
            }
        }
        let target_hour = match self.time_of_day.as_deref() {
            None => 0.0,
            Some(s) => crate::engines::parse_hh_mm(s).unwrap_or(0.0),
        };
        tick.covers_hour(target_hour)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RampPoint {
    pub date: NaiveDate,
    pub rate: f64,
}

/// Per-SubjectKind birth rate. New Subjects of `kind` appear at
/// `rate`/day on average (Poisson). When `created_event_kind` is
/// set the engine also emits that topic per birth so audit_log
/// rebuilders can reconstruct the synthesized Subject from
/// replay. Without it the pool grows silently — fine for tenants
/// who don't replay, but dangling-FK integrity scans will flag
/// any downstream events that reference the silent ids.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubjectRate {
    pub rate: f64,
    /// Optional ramps, same shape as `JobRate.ramp`.
    #[serde(default)]
    pub ramp: Vec<RampPoint>,
    /// Audit_log event kind to emit when a new Subject of this
    /// kind is born. Tenants set this so replay can reconstruct
    /// the synthesized id; leaving it `None` is the explicit
    /// opt-out for tenants that don't audit-source the kind.
    #[serde(default)]
    pub created_event_kind: Option<String>,
}

/// Anomaly probabilities for a single Workflow. The keys are
/// anomaly *names* defined by the StepType or Workflow authors;
/// values are probabilities in `[0, 1]`. The sim enumerates the
/// keys it understands and rolls each per Step transition.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AnomalyRates {
    pub probs: HashMap<String, f64>,
}

impl TenantConfig {
    /// Build a list of root `CounterpartySpec`s from the parsed
    /// `[counterparty.*]` blocks. Section keys become spec names,
    /// followup arrays are converted recursively. The engine
    /// flattens these at construction time.
    pub fn counterparty_specs(&self) -> Vec<crate::engines::CounterpartySpec> {
        let mut roots: Vec<(&String, &CounterpartyToml)> = self.counterparty.iter().collect();
        roots.sort_by(|a, b| a.0.cmp(b.0));
        roots
            .into_iter()
            .map(|(name, c)| crate::engines::CounterpartySpec {
                name: name.clone(),
                actor_kind: c.actor_kind.clone(),
                listens_to: c.listens_to.clone(),
                delay: crate::engines::DelaySpec {
                    mean_days: c.delay.mean_days,
                    spread_days: c.delay.spread_days,
                    business_calendar: c.delay.business_calendar.clone(),
                },
                emit_probability: c.emit_probability,
                emits: c.emits.clone(),
                emit_else: c.emit_else.clone(),
                payload: c.payload.clone(),
                followups: c.followups.iter().map(named_to_spec).collect(),
                scans: c.scans.iter().map(scan_toml_to_spec).collect(),
                match_payload: c.match_payload.clone(),
            })
            .collect()
    }

    // batch_toml_specs() retired 2026-05-06 alongside the
    // BatchEngine rip — see TODO.md "Strip BatchEngine".

    /// Build a list of `PeriodicSpec`s from the parsed
    /// `[periodic.*]` blocks. Section keys become spec names.
    pub fn periodic_specs(&self) -> Vec<crate::engines::PeriodicSpec> {
        let mut rows: Vec<(&String, &PeriodicToml)> = self.periodic.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        rows.into_iter()
            .map(|(name, p)| crate::engines::PeriodicSpec {
                name: name.clone(),
                cadence: p.cadence,
                anchor_date: p.anchor_date,
                business_calendar: p.business_calendar.clone(),
                action: match &p.action {
                    PeriodicActionToml::OpenJob {
                        workflow,
                        subject_kind,
                        subject_id,
                    } => crate::engines::PeriodicAction::OpenJob {
                        workflow: workflow.clone(),
                        subject_kind: subject_kind.clone(),
                        subject_id: subject_id.clone(),
                    },
                    PeriodicActionToml::EmitEvent { topic, payload } => {
                        crate::engines::PeriodicAction::EmitEvent {
                            topic: topic.clone(),
                            payload: payload.clone(),
                        }
                    }
                },
            })
            .collect()
    }

    /// Load + validate a TOML file from disk.
    pub fn load(path: &Path) -> Result<Self, TenantConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TenantConfigError::Io(path.display().to_string(), e.to_string()))?;
        Self::from_toml_str(&text, path.display().to_string().as_str())
    }

    /// Parse a TOML string. `source` is shown in error messages.
    pub fn from_toml_str(text: &str, source: &str) -> Result<Self, TenantConfigError> {
        let cfg: Self = toml::from_str(text)
            .map_err(|e| TenantConfigError::Parse(source.to_string(), e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), TenantConfigError> {
        if self.meta.tenant_id.is_empty() {
            return Err(TenantConfigError::Validation(
                "meta.tenant_id must not be empty".into(),
            ));
        }
        if self.meta.start_date >= self.meta.end_date {
            return Err(TenantConfigError::Validation(format!(
                "meta.end_date ({}) must be after start_date ({})",
                self.meta.end_date, self.meta.start_date
            )));
        }
        // Surface bad tick_duration at config-load time, not at
        // engine-runtime. `ticks_per_day()` falls through to 1 on
        // unparseable values; this turns the silent fallback into a
        // loud error.
        if let Err(e) = parse_tick_duration(&self.meta.tick_duration) {
            return Err(TenantConfigError::Validation(format!(
                "meta.tick_duration: {e}"
            )));
        }
        // The workforce-execution knob is operator-editable via the control
        // plane; a 0 / negative / NaN or absurdly-large multiplier would
        // freeze the sim. Keep the validator's contract — turn the silent
        // build_workforce clamp into a loud config-load error. (The bound
        // rejects NaN and +inf too: both fail the `0 < m <= 100` test.)
        if let Some(m) = self.meta.step_speed_multiplier
            && !(0.0 < m && m <= 100.0)
        {
            return Err(TenantConfigError::Validation(format!(
                "meta.step_speed_multiplier must be in (0, 100] (got {m})"
            )));
        }
        for (kind, jr) in &self.job_rates {
            if jr.rate < 0.0 {
                return Err(TenantConfigError::Validation(format!(
                    "job_rates.{kind}.rate must be >= 0 (got {})",
                    jr.rate
                )));
            }
            for r in &jr.ramp {
                if r.rate < 0.0 {
                    return Err(TenantConfigError::Validation(format!(
                        "job_rates.{kind}.ramp[{}] rate must be >= 0",
                        r.date
                    )));
                }
            }
            let sum: f64 = jr.subject_distribution.values().sum();
            if sum > 1.0 + 1e-6 {
                return Err(TenantConfigError::Validation(format!(
                    "job_rates.{kind}.subject_distribution sums to {sum:.3} > 1.0"
                )));
            }
        }
        for (kind, sr) in &self.subject_rates {
            if sr.rate < 0.0 {
                return Err(TenantConfigError::Validation(format!(
                    "subject_rates.{kind}.rate must be >= 0"
                )));
            }
        }
        for (kind, ar) in &self.anomalies {
            for (name, p) in &ar.probs {
                if !(0.0..=1.0).contains(p) {
                    return Err(TenantConfigError::Validation(format!(
                        "anomalies.{kind}.{name} = {p}, must be in [0, 1]"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchor test — must successfully parse the in-tree
    /// brewery tenant.toml. If this regresses, the brewery seed
    /// bundle and the sim's parse logic have drifted. (Legacy
    /// name retained for git-blame continuity; the tenant
    /// itself rebranded bakery → brewery.)
    #[test]
    fn parses_bakery_tenant_toml() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let cfg = TenantConfig::load(&path).unwrap_or_else(|e| {
            panic!("failed to parse {}: {e}", path.display());
        });
        assert_eq!(cfg.meta.tenant_id, "brewery");
        assert_eq!(cfg.meta.display_name, "Algedonic Ales");
        assert!(cfg.job_rates.contains_key("morning-brew"));
        assert!(cfg.job_rates.contains_key("wholesale-keg-order"));
        assert!(cfg.subject_rates.contains_key("account"));
        // morning-brew is a deterministic daily production review
        // (2026-06-14 closed-loop rework): the rate is the brewhouse's
        // per-beer brew-SLOT capacity per working day, deterministic +
        // weekend_multiplier 0.0, and the in-flight gate fills each slot
        // from stock. High-volume beers (pale, ipa) run 2 slots — one
        // batch is under a working day's pull; the low-volume kinds
        // (stout/lager/hazy) run 1.
        assert_eq!(cfg.job_rates["morning-brew"].rate, 2.0);
        assert_eq!(cfg.job_rates["morning-brew-ipa"].rate, 4.0);
        assert_eq!(cfg.job_rates["morning-brew-stout"].rate, 1.0);
        assert!(cfg.job_rates["morning-brew"].deterministic);
        assert!(cfg.job_rates["morning-brew-stout"].deterministic);
        // wholesale-keg-order has 4 ramp points
        assert_eq!(cfg.job_rates["wholesale-keg-order"].ramp.len(), 4);
        // seasonal-release has weekday/weekend multipliers
        let sr = &cfg.job_rates["seasonal-release"];
        assert_eq!(sr.weekday_multiplier, Some(1.0));
        assert_eq!(sr.weekend_multiplier, Some(1.0));
    }

    /// The workforce-execution multiplier is operator-editable via the
    /// control plane (POST /config), so validate() must reject a value
    /// that would freeze the sim before it's persisted — turning the
    /// silent build_workforce clamp into a loud config-load error.
    #[test]
    fn validate_rejects_out_of_range_step_speed_multiplier() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let mut cfg = TenantConfig::load(&path).expect("brewery tenant.toml parses");

        // Sane values pass (None = registry durations as-authored).
        for ok in [None, Some(0.5), Some(1.0), Some(100.0)] {
            cfg.meta.step_speed_multiplier = ok;
            assert!(cfg.validate().is_ok(), "{ok:?} should be accepted");
        }
        // 0, negative, NaN, +inf, and absurdly-large all fail loudly.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e9] {
            cfg.meta.step_speed_multiplier = Some(bad);
            assert!(
                matches!(cfg.validate(), Err(TenantConfigError::Validation(_))),
                "step_speed_multiplier {bad} should be rejected"
            );
        }
    }

    #[test]
    fn shock_target_matches_kind_and_subject_id() {
        let exact = ShockTarget {
            kind: Some("wholesale-keg-order".into()),
            subject_id: Some("acc-1".into()),
            subject_id_pattern: None,
            counterparty: None,
        };
        assert!(exact.matches("wholesale-keg-order", "acc-1"));
        assert!(!exact.matches("wholesale-keg-order", "acc-2"));
        assert!(!exact.matches("morning-brew", "acc-1"));
    }

    #[test]
    fn shock_target_matches_pattern_prefix() {
        let pat = ShockTarget {
            kind: Some("wholesale-keg-order".into()),
            subject_id: None,
            subject_id_pattern: Some("acc-bigseed-*".into()),
            counterparty: None,
        };
        assert!(pat.matches("wholesale-keg-order", "acc-bigseed-0001"));
        assert!(pat.matches("wholesale-keg-order", "acc-bigseed-9999"));
        assert!(!pat.matches("wholesale-keg-order", "acc-other-0001"));
    }

    #[test]
    fn shock_target_kind_only_matches_every_subject() {
        let any = ShockTarget {
            kind: Some("morning-brew".into()),
            subject_id: None,
            subject_id_pattern: None,
            counterparty: None,
        };
        assert!(any.matches("morning-brew", "loc-1"));
        assert!(any.matches("morning-brew", "loc-9"));
        assert!(!any.matches("wholesale-keg-order", "loc-1"));
    }

    #[test]
    fn shock_target_counterparty_only_matches_by_name() {
        let cp = ShockTarget {
            kind: None,
            subject_id: None,
            subject_id_pattern: None,
            counterparty: Some("vendor-acme".into()),
        };
        assert!(cp.matches_counterparty("vendor-acme"));
        assert!(!cp.matches_counterparty("vendor-other"));
        // Counterparty-targeted shocks don't apply to JobRate paths.
        assert!(!cp.matches("wholesale-keg-order", "loc-1"));
    }

    #[test]
    fn shock_target_kind_targeted_does_not_match_counterparty() {
        let kind_only = ShockTarget {
            kind: Some("wholesale-keg-order".into()),
            subject_id: None,
            subject_id_pattern: None,
            counterparty: None,
        };
        assert!(!kind_only.matches_counterparty("vendor-acme"));
    }

    #[test]
    fn subject_cadence_fires_on_matching_weekday() {
        use crate::engines::Tick;
        // 2026-01-06 is a Tuesday.
        let tue = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let mon = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let c = SubjectCadence {
            subject_id: "acc-1".into(),
            weekday: "Tue".into(),
            biweekly_offset: None,
            time_of_day: None,
        };
        assert!(c.fires_on_tick(tue, &Tick::day()));
        assert!(!c.fires_on_tick(mon, &Tick::day()));
    }

    #[test]
    fn subject_cadence_weekday_is_case_insensitive() {
        use crate::engines::Tick;
        let tue = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        for label in ["tue", "TUE", "Tuesday", "tuesday", "Tues"] {
            let c = SubjectCadence {
                subject_id: "acc-1".into(),
                weekday: label.into(),
                biweekly_offset: None,
                time_of_day: None,
            };
            assert!(
                c.fires_on_tick(tue, &Tick::day()),
                "{label} should match Tuesday"
            );
        }
    }

    #[test]
    fn subject_cadence_unknown_weekday_never_fires() {
        use crate::engines::Tick;
        let any = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let c = SubjectCadence {
            subject_id: "acc-1".into(),
            weekday: "garbage".into(),
            biweekly_offset: None,
            time_of_day: None,
        };
        assert!(!c.fires_on_tick(any, &Tick::day()));
    }

    #[test]
    fn subject_cadence_biweekly_alternates_weeks() {
        use crate::engines::Tick;
        // 2026-01-05 is in ISO week 2 (even); 2026-01-12 is week 3 (odd).
        let week_2 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let week_3 = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
        let even = SubjectCadence {
            subject_id: "acc-1".into(),
            weekday: "Mon".into(),
            biweekly_offset: Some(0),
            time_of_day: None,
        };
        let odd = SubjectCadence {
            subject_id: "acc-1".into(),
            weekday: "Mon".into(),
            biweekly_offset: Some(1),
            time_of_day: None,
        };
        assert!(even.fires_on_tick(week_2, &Tick::day()));
        assert!(!even.fires_on_tick(week_3, &Tick::day()));
        assert!(!odd.fires_on_tick(week_2, &Tick::day()));
        assert!(odd.fires_on_tick(week_3, &Tick::day()));
    }

    /// Phase B-2 — `time_of_day` anchors a cadence row to a
    /// specific hour-of-day. At sub-day tick granularity, the
    /// row fires on the tick whose window covers that hour. At
    /// day-tick granularity (legacy), the day-tick covers the
    /// full day so the hour anchor is a no-op.
    #[test]
    fn subject_cadence_time_of_day_fires_on_matching_hourly_tick() {
        use crate::engines::Tick;
        // 2026-01-06 is a Tuesday.
        let tue = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let mon = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();

        let cadence_at_9 = SubjectCadence {
            subject_id: "acc-sierra".into(),
            weekday: "Tue".into(),
            biweekly_offset: None,
            time_of_day: Some("09:00".into()),
        };

        // Day-tick: fires on Tue regardless of time anchor (the
        // day tick covers [0, 24) which includes 09:00).
        assert!(cadence_at_9.fires_on_tick(tue, &Tick::day()));
        // Wrong day → never fires.
        assert!(!cadence_at_9.fires_on_tick(mon, &Tick::day()));

        // Hourly ticks on Tue:
        //   tick at 08:00 → does NOT cover 09:00.
        //   tick at 09:00 → covers 09:00.
        //   tick at 10:00 → does NOT cover 09:00.
        assert!(!cadence_at_9.fires_on_tick(tue, &Tick::hour(8.0, false)));
        assert!(cadence_at_9.fires_on_tick(tue, &Tick::hour(9.0, false)));
        assert!(!cadence_at_9.fires_on_tick(tue, &Tick::hour(10.0, false)));

        // 15-min ticks on Tue:
        //   tick at 09:00 (covers [9.0, 9.25)) → fires.
        //   tick at 09:15 (covers [9.25, 9.5)) → does NOT fire.
        let q1 = Tick::new(0.25, 9.0, false);
        let q2 = Tick::new(0.25, 9.25, false);
        assert!(cadence_at_9.fires_on_tick(tue, &q1));
        assert!(!cadence_at_9.fires_on_tick(tue, &q2));
    }

    /// Phase B-2 — `[meta.operating_hours]` per-weekday windows
    /// gate the HumanWorker layer at sub-day tick granularity.
    /// Day-tick is a no-op (the day-tick covers `[0, 24)` so any
    /// non-empty window overlaps).
    #[test]
    fn operating_hours_filter_at_sub_day_ticks() {
        use crate::engines::Tick;
        let mut hours = HashMap::new();
        hours.insert("mon".to_string(), "08:00-18:00".to_string());
        hours.insert("tue".to_string(), "08:00-18:00".to_string());
        // wed absent → closed all day
        let meta = TenantMeta {
            tenant_id: "x".into(),
            display_name: "X".into(),
            seed: 1,
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            operating_days: vec![],
            operating_hours: hours,
            tick_duration: "1d".into(),
            step_speed_multiplier: None,
        };
        let mon = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let wed = NaiveDate::from_ymd_opt(2026, 1, 7).unwrap();

        // Day-tick on Mon: window non-empty → in-hours.
        assert!(meta.is_operating_at_tick(mon, &Tick::day()));
        // Day-tick on Wed: no window → closed all day.
        assert!(!meta.is_operating_at_tick(wed, &Tick::day()));

        // Hourly ticks on Mon:
        //   06:00 (before open) → out-of-hours.
        //   08:00 (open) → in-hours.
        //   12:00 (mid-day) → in-hours.
        //   18:00 (close — half-open, so closed) → out-of-hours.
        //   22:00 (after close) → out-of-hours.
        assert!(!meta.is_operating_at_tick(mon, &Tick::hour(6.0, false)));
        assert!(meta.is_operating_at_tick(mon, &Tick::hour(8.0, false)));
        assert!(meta.is_operating_at_tick(mon, &Tick::hour(12.0, false)));
        assert!(!meta.is_operating_at_tick(mon, &Tick::hour(18.0, false)));
        assert!(!meta.is_operating_at_tick(mon, &Tick::hour(22.0, false)));

        // Wed is closed all day.
        assert!(!meta.is_operating_at_tick(wed, &Tick::hour(12.0, false)));
    }

    #[test]
    fn operating_hours_empty_means_always_in_hours() {
        use crate::engines::Tick;
        // No `[meta.operating_hours]` block → every operating-day
        // tick is in-hours regardless of HH:MM. Pre-Phase-B-2
        // back-compat.
        let meta = TenantMeta {
            tenant_id: "x".into(),
            display_name: "X".into(),
            seed: 1,
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            operating_days: vec![],
            operating_hours: HashMap::new(),
            tick_duration: "1h".into(),
            step_speed_multiplier: None,
        };
        let any_day = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        for h in 0..24 {
            assert!(meta.is_operating_at_tick(any_day, &Tick::hour(h as f64, h == 0)));
        }
    }

    #[test]
    fn operating_hours_overlap_window_partially() {
        use crate::engines::Tick;
        let mut hours = HashMap::new();
        hours.insert("mon".to_string(), "08:00-18:00".to_string());
        let meta = TenantMeta {
            tenant_id: "x".into(),
            display_name: "X".into(),
            seed: 1,
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            operating_days: vec![],
            operating_hours: hours,
            tick_duration: "30m".into(),
            step_speed_multiplier: None,
        };
        let mon = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 30-min tick at 07:30 covers [7.5, 8.0). Window starts
        // at 8.0. They DON'T overlap (half-open at the close
        // edge: 8.0 is in-window but the tick ends just before).
        let t = Tick::new(0.5, 7.5, false);
        assert!(!meta.is_operating_at_tick(mon, &t));
        // 30-min tick at 17:30 covers [17.5, 18.0). Open window
        // ends at 18.0. They overlap on [17.5, 18.0).
        let t = Tick::new(0.5, 17.5, false);
        assert!(meta.is_operating_at_tick(mon, &t));
    }

    #[test]
    fn subject_cadence_no_time_of_day_defaults_to_midnight() {
        use crate::engines::Tick;
        // No time_of_day → fires at hour 0 (midnight). Matches
        // the pre-Phase-B-2 behavior where cadence rows always
        // fired on tick 0 of the matching weekday.
        let tue = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let cadence = SubjectCadence {
            subject_id: "acc-1".into(),
            weekday: "Tue".into(),
            biweekly_offset: None,
            time_of_day: None,
        };

        // Day-tick fires (covers midnight).
        assert!(cadence.fires_on_tick(tue, &Tick::day()));
        // Hourly tick at midnight fires; 9:00 does not.
        assert!(cadence.fires_on_tick(tue, &Tick::hour(0.0, true)));
        assert!(!cadence.fires_on_tick(tue, &Tick::hour(9.0, false)));
    }

    #[test]
    fn operating_days_filter_works() {
        let meta = TenantMeta {
            tenant_id: "x".into(),
            display_name: "X".into(),
            seed: 1,
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            operating_days: vec![
                "mon".into(),
                "tue".into(),
                "wed".into(),
                "thu".into(),
                "fri".into(),
            ],
            tick_duration: "1d".into(),
            step_speed_multiplier: None,
            operating_hours: std::collections::HashMap::new(),
        };
        // Monday 2026-01-05
        assert!(meta.is_operating_day(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()));
        // Saturday 2026-01-03
        assert!(!meta.is_operating_day(NaiveDate::from_ymd_opt(2026, 1, 3).unwrap()));
    }

    #[test]
    fn empty_operating_days_means_always_open() {
        let meta = TenantMeta {
            tenant_id: "x".into(),
            display_name: "X".into(),
            seed: 1,
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            operating_days: vec![],
            tick_duration: "1d".into(),
            step_speed_multiplier: None,
            operating_hours: std::collections::HashMap::new(),
        };
        assert!(meta.is_operating_day(NaiveDate::from_ymd_opt(2026, 1, 3).unwrap()));
    }

    #[test]
    fn rejects_negative_rate() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"

[job_rates.bad]
rate = -0.5
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(matches!(err, TenantConfigError::Validation(_)));
        assert!(err.to_string().contains("must be >= 0"));
    }

    #[test]
    fn rejects_subject_distribution_sum_over_one() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"

[job_rates.bad]
rate = 1.0
subject_distribution = { "a" = 0.6, "b" = 0.6 }
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(err.to_string().contains("sums to"));
    }

    #[test]
    fn rejects_anomaly_probability_out_of_range() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"

[anomalies.foo]
delay_probability = 1.5
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(err.to_string().contains("must be in [0, 1]"));
    }

    // parses_bakery_batch_section retired 2026-05-06 alongside
    // the BatchEngine rip — see TODO.md "Strip BatchEngine".

    #[test]
    fn parses_bakery_periodic_section() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let cfg = TenantConfig::load(&path).unwrap();
        let specs = cfg.periodic_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        // `quarterly-sales-tax` was intentionally disabled in the
        // brewery tenant.toml (it remitted without an accrual flow);
        // re-add to this list when that periodic Workflow comes back.
        for required in &["daily-bank-sweep", "equipment-preventive-maintenance"] {
            assert!(
                names.contains(required),
                "expected {required}; got {names:?}"
            );
        }
        // Payroll moved to [batch.payroll] — no longer in periodic.
        assert!(
            !names.contains(&"payroll"),
            "payroll should have moved to [batch.payroll]; still in periodic? got {names:?}"
        );
        let sweep = specs.iter().find(|s| s.name == "daily-bank-sweep").unwrap();
        assert_eq!(sweep.cadence, crate::engines::Cadence::Daily);
        assert_eq!(sweep.business_calendar.as_deref(), Some("us-banking"));
        match &sweep.action {
            crate::engines::PeriodicAction::EmitEvent { topic, .. } => {
                assert_eq!(topic, "ledger.bank_sweep_request");
            }
            _ => panic!("expected EmitEvent action"),
        }
    }

    #[test]
    fn parses_bakery_counterparty_section() {
        // Legacy name retained for git-blame continuity; this
        // tests the brewery counterparty section after the
        // bakery → brewery rebrand.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let cfg = TenantConfig::load(&path).unwrap();
        let specs = cfg.counterparty_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        for required in &["bank-ach", "keg-courier", "ar-aging"] {
            assert!(
                names.contains(required),
                "expected {required}; got {names:?}"
            );
        }
        let bank = specs.iter().find(|s| s.name == "bank-ach").unwrap();
        // bank-ach chains off ar-aging's `commerce.invoice.paid`,
        // not directly off job.closed — so settlement only fires
        // for the paid branch (past_due never settles).
        assert_eq!(bank.listens_to, "commerce.invoice.paid");
        assert_eq!(bank.emits, "ledger.payment_settled");
        assert_eq!(bank.emit_probability, 0.985);
        assert_eq!(bank.delay.mean_days, 1.0);
        assert_eq!(bank.delay.business_calendar.as_deref(), Some("us-banking"));
        assert_eq!(bank.payload["channel"], "ach");

        // Supplier chains (grain/hops) are NO LONGER hand-authored in
        // tenant.toml — the simulator synthesizes one supplier
        // CounterpartySpec per vendor from each vendor's behavior at boot
        // (boss_brewery_engine::synthesize_vendor_specs). The static section
        // carries none.
        assert!(
            !names.contains(&"grain-supplier"),
            "grain-supplier is synthesized per-vendor now, not static"
        );
        assert!(
            !names.contains(&"hops-supplier"),
            "hops-supplier is synthesized per-vendor now, not static"
        );

        // Keg courier uses the `scans` sugar — 3 stages.
        let courier = specs.iter().find(|s| s.name == "keg-courier").unwrap();
        assert_eq!(courier.scans.len(), 3);
        assert_eq!(courier.scans[0].status, "in-transit");
        assert_eq!(courier.scans[1].status, "out-for-delivery");
        assert_eq!(courier.scans[2].status, "delivered");
        // Flatten produces 3 chained CounterpartySpecs.
        assert_eq!(courier.flatten(None).len(), 3);
    }

    #[test]
    fn rejects_inverted_date_range() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-12-31"
end_date = "2026-01-01"
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(err.to_string().contains("must be after start_date"));
    }

    /// Phase B-1: `[meta] tick_duration` parsing — accepted forms
    /// are `1d` / `Nh` / `Nm` where N divides 24 hours / 1440
    /// minutes evenly. Defaults to `"1d"` when absent so existing
    /// tenant.toml files keep their behavior.
    #[test]
    fn tick_duration_defaults_to_1d() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
"#;
        let cfg = TenantConfig::from_toml_str(toml, "<test>").unwrap();
        assert_eq!(cfg.meta.tick_duration, "1d");
        assert_eq!(cfg.meta.ticks_per_day(), 1);
    }

    #[test]
    fn tick_duration_hourly() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "1h"
"#;
        let cfg = TenantConfig::from_toml_str(toml, "<test>").unwrap();
        assert_eq!(cfg.meta.ticks_per_day(), 24);
    }

    #[test]
    fn tick_duration_15m() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "15m"
"#;
        let cfg = TenantConfig::from_toml_str(toml, "<test>").unwrap();
        assert_eq!(cfg.meta.ticks_per_day(), 96);
    }

    #[test]
    fn tick_duration_1m() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "1m"
"#;
        let cfg = TenantConfig::from_toml_str(toml, "<test>").unwrap();
        assert_eq!(cfg.meta.ticks_per_day(), 1440);
    }

    #[test]
    fn tick_duration_rejects_uneven_split() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "5h"
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(
            err.to_string().contains("divide 24 evenly"),
            "expected even-divide error, got {err}"
        );
    }

    #[test]
    fn tick_duration_rejects_unknown_unit() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "1y"
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(
            err.to_string().contains("must be one of d / h / m"),
            "expected unit error, got {err}"
        );
    }

    #[test]
    fn tick_duration_rejects_zero() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "0h"
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(err.to_string().contains("count must be > 0"));
    }

    #[test]
    fn tick_duration_rejects_multi_day() {
        let toml = r#"
[meta]
tenant_id = "x"
display_name = "X"
seed = 1
start_date = "2026-01-01"
end_date = "2026-12-31"
tick_duration = "2d"
"#;
        let err = TenantConfig::from_toml_str(toml, "<test>").unwrap_err();
        assert!(
            err.to_string().contains("only `1d` supported"),
            "expected single-day error, got {err}"
        );
    }

    #[test]
    fn parse_tick_duration_unit_function() {
        // Direct API tests for the parser. Engine call sites that
        // build a TenantMeta inline (not via TOML) get the same
        // validation through TenantMeta::ticks_per_day().
        assert_eq!(parse_tick_duration("1d").unwrap(), 1);
        assert_eq!(parse_tick_duration("1h").unwrap(), 24);
        assert_eq!(parse_tick_duration("2h").unwrap(), 12);
        assert_eq!(parse_tick_duration("3h").unwrap(), 8);
        assert_eq!(parse_tick_duration("4h").unwrap(), 6);
        assert_eq!(parse_tick_duration("6h").unwrap(), 4);
        assert_eq!(parse_tick_duration("12h").unwrap(), 2);
        assert_eq!(parse_tick_duration("30m").unwrap(), 48);
        assert_eq!(parse_tick_duration("15m").unwrap(), 96);
        assert_eq!(parse_tick_duration("5m").unwrap(), 288);
        assert_eq!(parse_tick_duration("1m").unwrap(), 1440);
        assert!(parse_tick_duration("").is_err());
        assert!(parse_tick_duration("1").is_err());
        assert!(parse_tick_duration("h").is_err());
        assert!(parse_tick_duration("7h").is_err()); // doesn't divide 24
        assert!(parse_tick_duration("13m").is_err()); // doesn't divide 1440
    }
}
