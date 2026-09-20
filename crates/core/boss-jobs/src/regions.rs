//! The IT system map's regions — `GET /api/yard/regions` (design
//! 0524fc95, decided 2026-09-19; this is car 1, the server read).
//!
//! WHY THIS EXISTS. The Train Yard's KPIs were computed on the client:
//! `apps/web/src/it/yard/yard.ts` read six endpoints and derived every
//! number the page showed, and `boss orient` derived the same numbers
//! again, separately, on the CLI — two definitions of "trains in
//! transit", "time at CI", "dock depth", with nothing holding them
//! equal (a surface may answer differently than its server). A map that
//! bubbles KPIs up needs ONE definition per number. This module is it:
//! a pure function over rows the handler reads, answering a list of
//! REGIONS — dock, gates, track, shed, arrivals, garage, receiving,
//! marshalling — each with a `count` (what is here), a `state`
//! (clear / busy / troubled, from the thresholds the yard and the
//! alarms already use) and a `trend` (this window versus the previous).
//!
//! THE JUDGEMENTS ARE THE YARD'S OWN. Where `crate::yard` already
//! decides something server-side — a train's block, a gate bay's
//! staleness, the dock's boarding threshold, the stranded and held
//! lanes — this reads that decision off [`YardStatus`] rather than
//! deciding again. The two predicates the yard decided only on the
//! client are ported here ONCE and pinned against the TypeScript by a
//! test: the train-gate wait (`train_gate_wait_reason` /
//! `train_gate_fallback`, `yard.ts::trainGateTroubled`) and the
//! receiving yard's age bands and pipeline set (`receiving.ts`).
//!
//! NO NUMBER THIS SURFACE MAKES UP. A region whose input could not be
//! read answers `count: null` and `troubled`, with the reason in `why`
//! — never an empty region that reads as "nothing here". A trend with
//! no samples is `null`, never zero.

use boss_core::job::{Job, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::WorkflowSpec;
use crate::yard::{Reading, YardStatus};

/// The eight regions, in map order. The count and the order are the
/// decision (0524fc95 Q2); a reader that finds a ninth name has an
/// older or newer server than it expects.
pub const REGIONS: [&str; 8] = [
    "dock",
    "gates",
    "track",
    "shed",
    "arrivals",
    "garage",
    "receiving",
    "marshalling",
];

/// The trend window when the caller names none: a day, the shortest
/// window every region has had a chance to move in.
pub const DEFAULT_WINDOW_HOURS: i64 = 24;
/// A week. The previous window is as long again, so the handler's
/// closed reads reach back 14 days — the same reach the yard's recent
/// tail already has (`http/yard.rs::RECENT_TRAIN_DAYS`).
pub const MAX_WINDOW_HOURS: i64 = 168;

/// `window=24h`, `window=7d`, `window=36` (hours) → hours, or the
/// sentence a 400 carries. Bounded to `1..=MAX_WINDOW_HOURS`: a window
/// the reads cannot reach would answer a smaller question than it was
/// asked (a limit is not a filter).
pub fn parse_window(raw: Option<&str>) -> Result<i64, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_WINDOW_HOURS);
    };
    let (digits, unit) = match raw.strip_suffix('h') {
        Some(d) => (d, 1),
        None => match raw.strip_suffix('d') {
            Some(d) => (d, 24),
            None => (raw, 1),
        },
    };
    let n: i64 = digits
        .parse()
        .map_err(|_| format!("window must be <n>h, <n>d or hours, e.g. 24h or 7d; got {raw:?}"))?;
    let hours = n * unit;
    if !(1..=MAX_WINDOW_HOURS).contains(&hours) {
        return Err(format!(
            "window must be between 1h and {MAX_WINDOW_HOURS}h ({}d); got {raw:?}",
            MAX_WINDOW_HOURS / 24
        ));
    }
    Ok(hours)
}

/// What a region's card says at a glance. `Clear` is room to spare and
/// nothing wrong; `Busy` is at a bound or holding work that waits on
/// the machine (a train due, a bay full, a queue formed); `Troubled` is
/// a threshold the yard or an alarm already enforces, crossed — or a
/// reading that could not be taken, which is refused like a failure
/// rather than drawn as clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionState {
    Clear,
    Busy,
    Troubled,
}

/// This window against the previous one, for the number the region's
/// floor already measures. Both halves are `None` when nothing in that
/// window could be measured — a rate nobody measured is not zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trend {
    /// What is measured — `dock wait`, `time at CI`, `arrivals`…
    pub metric: String,
    /// `hours`, `minutes` or `per day`.
    pub unit: String,
    pub current: Option<f64>,
    pub previous: Option<f64>,
    /// How many observations each half is computed from — a median of
    /// one is an anecdote, and the card can say so.
    pub samples: usize,
    pub previous_samples: usize,
}

/// One region's card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub name: String,
    /// What is here. `None` when the region could not be read — the
    /// state is then `troubled` and `why` names the read.
    pub count: Option<usize>,
    /// The bound the count is read against, where the region has one:
    /// the gate concurrency, the boarding depth, the single track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<usize>,
    pub state: RegionState,
    /// One sentence naming why the state is what it is. A verdict must
    /// name what failed (CLAUDE.md §Diagnosis).
    pub why: String,
    pub trend: Trend,
}

/// The whole map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regions {
    pub window_hours: i64,
    pub regions: Vec<Region>,
}

/// One station of the marshalling yard, as the handler read it:
/// depth and the open packets standing there (for a distinct count
/// across stations — a packet can match two predicates), and the
/// counted flow in each window, `None` where the cube is blind to the
/// station's predicate (`crate::station_flow::FlowBlind`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StationReading {
    pub name: String,
    pub over_limit: bool,
    pub members: Vec<String>,
    pub served: Option<i64>,
    pub previous_served: Option<i64>,
}

/// Everything [`regions`] reads, named — the same rows the yard status
/// is built from, plus the windowed tails the trends need.
#[derive(Debug, Clone)]
pub struct RegionInputs<'a> {
    /// The yard's judgements, built by `yard::build_status_for` from
    /// the same rows.
    pub status: &'a YardStatus,
    /// Whether the dock row was read (`http/yard.rs::dock_cars`).
    pub dock_reading: Reading,
    /// Open pr-trains with steps. The train-gate wait is a fact on the
    /// job's metadata that [`YardStatus`] does not carry.
    pub open_trains: &'a [(Job, Vec<Step>)],
    /// pr-trains closed within two windows, with steps (an arrival's
    /// `pr` and `ci` stamps are the time at CI).
    pub closed_trains: &'a [(Job, Vec<Step>)],
    /// ship-a-change cars: open, plus closed within two windows, with
    /// steps (the shed is the `proven` step; the dock wait is the
    /// `gate` stamp to the train's `collect` stamp).
    pub cars: &'a [(Job, Vec<Step>)],
    /// gate-runs open or closed within two windows — rows only; a run's
    /// `opened_at`, `closed_at` and `outcome` are on its metadata.
    pub gate_runs: &'a [Job],
    /// The receiving yard's inbound packets — open, plus closed within
    /// two windows — rows only. `None` when the workflow registry that
    /// says which kinds are inbound could not be read.
    pub inbound: Option<&'a [Job]>,
    /// Every station but the dock. `None` when the station registry
    /// could not be read.
    pub stations: Option<&'a [StationReading]>,
    pub now: chrono::DateTime<chrono::Utc>,
    pub window_hours: i64,
}

// ---------------------------------------------------------------------
// The two client-side predicates, ported once.
// ---------------------------------------------------------------------

/// WHY the conductor has not filed a train's gate yet, verbatim from
/// the last failed launch — cleared the pass it is filed (0d16df6f).
/// The conductor writes it (`boss-cli/src/train_gate.rs` names this
/// constant), the yard reads it (`yard.ts::readTrainGate`), and so does
/// the track region below.
pub const TRAIN_GATE_WAIT_REASON: &str = "train_gate_wait_reason";
/// The gate could not be filed and CI alone judged the train — stamped
/// loudly on the train. Same three readers.
pub const TRAIN_GATE_FALLBACK: &str = "train_gate_fallback";

/// `yard.ts::trainGateTroubled`, ported: a train whose gate could not
/// be filed (fallback) or is still waiting at the bound (wait reason)
/// wears the yard's trouble style. A blank string is no marker, as
/// `readTrainGate`'s `text()` reads it.
pub fn train_gate_troubled(md: &Value) -> bool {
    [TRAIN_GATE_FALLBACK, TRAIN_GATE_WAIT_REASON]
        .iter()
        .any(|k| {
            md.get(k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        })
}

/// The delivery pipeline's kinds — a packet of these is a car in
/// transit or the machinery moving it, never a request, so the
/// receiving yard leaves them to the train yard. `receiving.ts::
/// PIPELINE_KINDS`, ported; the test `the_pipeline_kinds_match_the_
/// receiving_yard` holds the two equal.
pub const PIPELINE_KINDS: [&str; 9] = [
    "pr-train",
    "gate-run",
    "ship-a-change",
    "ops-request",
    "park-a-job",
    "emergency-merge",
    "repair-a-train",
    "publish-request",
    "regenerate-deployment",
];

/// The receiving yard's age bands, in days from `opened_on`: triage
/// within 3 days, anything within 14 (`receiving.ts::AGE_THRESHOLDS`,
/// ported and pinned by the same test).
pub const AGING_DAYS: i64 = 3;
pub const STALE_DAYS: i64 = 14;

/// Which kinds are inbound: active platform-category kinds that are
/// neither chores (`maintenance-*`) nor the pipeline
/// (`receiving.ts::inboundKinds`). Derived from the registry, never
/// listed by kind.
pub fn inbound_kinds(workflows: &[WorkflowSpec]) -> Vec<String> {
    let mut kinds: Vec<String> = workflows
        .iter()
        .filter(|w| w.status == crate::registry::WorkflowStatus::Active)
        .filter(|w| w.category == "platform")
        .filter(|w| {
            !w.kind.starts_with("maintenance-") && !PIPELINE_KINDS.contains(&w.kind.as_str())
        })
        .map(|w| w.kind.clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

// ---------------------------------------------------------------------
// The shed's places — `boss orient`'s `shed_place`, moved here so the
// CLI and the read share one classification.
// ---------------------------------------------------------------------

/// Where a landed, unproven car stands — the yard's inspection shed
/// read in words (`apps/web/src/it/yard/yard-shed.ts`, `shedPlace`).
/// The three places are disjoint and total, the probe winning over an
/// event (a probe can be run; an event has to happen). `Unproven` is
/// the forgotten case, and the only troubled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShedPlace {
    /// A probe is recorded; `last` is the failed attempt's `why`, if
    /// any (a succeeding attempt completes the step and the car leaves).
    ProbePending { last: Option<String> },
    /// The probe ran and exited 75: NOT YET — early, not wrong.
    ProbeNotYet { said: String },
    /// Only an event is recorded: prose naming what has to happen.
    WaitingOn(String),
    /// Neither — no probe, no event; nothing mechanical can settle it.
    Unproven,
}

fn md_str<'a>(md: &'a Value, key: &str) -> &'a str {
    md.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Classify a car's metadata into its shed place.
pub fn shed_place(md: &Value) -> ShedPlace {
    let probe = md_str(md, crate::car::PROOF_PROBE);
    let event = md_str(md, crate::car::PROOF_EVENT);
    if !probe.is_empty() {
        let attempt = md.get("proof_attempt");
        let not_yet = attempt.is_some_and(|a| {
            a.get("not_yet").and_then(Value::as_bool) == Some(true)
                || a.get("exit").and_then(Value::as_i64) == Some(75)
        });
        let last = attempt
            .and_then(|a| a.get("why"))
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        if not_yet {
            return ShedPlace::ProbeNotYet {
                said: last.unwrap_or_else(|| "the probe said not yet".to_string()),
            };
        }
        return ShedPlace::ProbePending { last };
    }
    if !event.is_empty() {
        return ShedPlace::WaitingOn(event.to_string());
    }
    ShedPlace::Unproven
}

// ---------------------------------------------------------------------
// Stamps and windows.
// ---------------------------------------------------------------------

pub(crate) type Instant = chrono::DateTime<chrono::Utc>;

pub(crate) fn parse_instant(s: &str) -> Option<Instant> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

pub(crate) fn meta_instant(md: &Value, key: &str) -> Option<Instant> {
    md.get(key).and_then(Value::as_str).and_then(parse_instant)
}

/// A step by slug, with the title fallback the conductor's own
/// addressing uses.
pub(crate) fn find_step<'a>(steps: &'a [Step], slug: &str, title: &str) -> Option<&'a Step> {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug) || s.title == title)
}

/// When a step completed: the server's column first, then the
/// conductor's metadata stamp — the same two readers `yard.rs`'s
/// `verdict_instant` has, for the same reason (older rows carry only
/// the stamp).
pub(crate) fn step_done_at(step: Option<&Step>) -> Option<Instant> {
    let s = step?;
    if s.status != StepStatus::Completed {
        return None;
    }
    s.completed_at
        .or_else(|| meta_instant(&s.metadata, "completed_at"))
}

/// The instant a packet opened: the `opened_at` stamp every pipeline
/// packet carries, else its `opened_on` at midnight — coarser, and
/// still inside the right day.
pub(crate) fn opened_at(job: &Job) -> Option<Instant> {
    meta_instant(&job.metadata, "opened_at").or_else(|| {
        job.opened_on
            .and_hms_opt(0, 0, 0)
            .map(|t| chrono::DateTime::from_naive_utc_and_offset(t, chrono::Utc))
    })
}

/// The instant a packet closed: `closed_at`, which the server writes
/// when a declared terminal step completes.
pub(crate) fn closed_at(job: &Job) -> Option<Instant> {
    meta_instant(&job.metadata, "closed_at")
}

/// This window and the previous one, as half-open ranges ending at
/// `now`: `[since, now)` and `[before, since)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Windows {
    before: Instant,
    since: Instant,
    now: Instant,
    pub(crate) hours: i64,
}

impl Windows {
    pub(crate) fn of(now: Instant, hours: i64) -> Self {
        let w = chrono::Duration::hours(hours);
        Windows {
            before: now - w - w,
            since: now - w,
            now,
            hours,
        }
    }
    pub(crate) fn current(&self, t: Instant) -> bool {
        t >= self.since && t <= self.now
    }
    pub(crate) fn previous(&self, t: Instant) -> bool {
        t >= self.before && t < self.since
    }
    /// A count as a per-day rate over one window.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn per_day(&self, n: usize) -> f64 {
        n as f64 * 24.0 / self.hours as f64
    }
}

/// The middle observation of a sample — an observation, not an
/// interpolation, the convention the yard's `quantile` keeps so every
/// number on the map was read off a real packet.
fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    v.get(v.len() / 2).copied()
}

#[allow(clippy::cast_precision_loss)]
fn seconds_as(unit: &str, s: i64) -> f64 {
    match unit {
        "hours" => s as f64 / 3600.0,
        "minutes" => s as f64 / 60.0,
        _ => s as f64,
    }
}

/// A median-duration trend: the samples that fall in each window,
/// reduced to their middle observation in `unit`.
fn duration_trend(metric: &str, unit: &str, current: Vec<i64>, previous: Vec<i64>) -> Trend {
    Trend {
        metric: metric.to_string(),
        unit: unit.to_string(),
        samples: current.len(),
        previous_samples: previous.len(),
        current: median(current).map(|s| seconds_as(unit, s)),
        previous: median(previous).map(|s| seconds_as(unit, s)),
    }
}

/// A per-day rate trend from two counts. A count is a measurement even
/// when it is zero — an empty window is a rate of 0, not "unknown" —
/// so both halves are always `Some`.
pub(crate) fn rate_trend(metric: &str, w: &Windows, current: usize, previous: usize) -> Trend {
    Trend {
        metric: metric.to_string(),
        unit: "per day".to_string(),
        current: Some(w.per_day(current)),
        previous: Some(w.per_day(previous)),
        samples: current,
        previous_samples: previous,
    }
}

/// Split instants into (current, previous) samples.
fn split<I>(w: &Windows, items: I) -> (Vec<i64>, Vec<i64>)
where
    I: IntoIterator<Item = (Instant, i64)>,
{
    let mut cur = Vec::new();
    let mut prev = Vec::new();
    for (at, sample) in items {
        if w.current(at) {
            cur.push(sample);
        } else if w.previous(at) {
            prev.push(sample);
        }
    }
    (cur, prev)
}

pub(crate) fn count_split<I>(w: &Windows, items: I) -> (usize, usize)
where
    I: IntoIterator<Item = Instant>,
{
    let (c, p) = split(w, items.into_iter().map(|t| (t, 0)));
    (c.len(), p.len())
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn region(
    name: &str,
    count: Option<usize>,
    bound: Option<usize>,
    state: RegionState,
    why: String,
    trend: Trend,
) -> Region {
    Region {
        name: name.to_string(),
        count,
        bound,
        state,
        why,
        trend,
    }
}

// ---------------------------------------------------------------------
// The regions.
// ---------------------------------------------------------------------

/// The map. Pure: rows in, eight cards out, in [`REGIONS`] order.
pub fn regions(inputs: &RegionInputs<'_>) -> Regions {
    let w = Windows::of(inputs.now, inputs.window_hours);
    Regions {
        window_hours: inputs.window_hours,
        regions: vec![
            dock(inputs, &w),
            gates(inputs, &w),
            track(inputs, &w),
            shed(inputs, &w),
            arrivals(inputs, &w),
            garage(inputs, &w),
            receiving(inputs, &w),
            marshalling(inputs, &w),
        ],
    }
}

/// THE DOCK: cars parked and boardable. Busy when the boarding depth
/// is met — a train is due — and troubled only when the dock row could
/// not be read (an unread dock is not an empty one, 52fed017). The
/// trend is the DOCK WAIT: how long the cars that boarded in the
/// window stood on the dock first, from the car's `gate` stamp to its
/// train's `collect` stamp.
fn dock(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let status = inputs.status;
    let boarded_at: std::collections::HashMap<String, Instant> = inputs
        .open_trains
        .iter()
        .chain(inputs.closed_trains.iter())
        .filter_map(|(j, s)| {
            let collect = step_done_at(find_step(s, "collect", "Collect what is ready to board"))?;
            Some((j.id.to_string(), collect))
        })
        .collect();
    let waits = inputs.cars.iter().filter_map(|(car, steps)| {
        let train = car.metadata.get("train").and_then(Value::as_str)?;
        let boarded = *boarded_at.get(train)?;
        let parked = step_done_at(find_step(steps, crate::car::GATE_SLUG, crate::car::GATE))?;
        let wait = (boarded - parked).num_seconds();
        (wait >= 0).then_some((boarded, wait))
    });
    let (cur, prev) = split(w, waits);
    let trend = duration_trend("dock wait", "hours", cur, prev);
    let bound = status
        .boarding
        .dock_threshold
        .and_then(|t| usize::try_from(t).ok());
    match inputs.dock_reading {
        Reading::Unread => region(
            "dock",
            None,
            bound,
            RegionState::Troubled,
            "the loading-dock station row could not be read".to_string(),
            trend,
        ),
        Reading::Read => {
            let depth = status.dock.len();
            let (state, why) = if status.boarding.threshold_met == Some(true) {
                (
                    RegionState::Busy,
                    format!(
                        "{} — the boarding depth is met, a train is due",
                        plural(depth, "car parked", "cars parked")
                    ),
                )
            } else {
                (
                    RegionState::Clear,
                    plural(depth, "car parked", "cars parked"),
                )
            };
            region("dock", Some(depth), bound, state, why, trend)
        }
    }
}

/// THE GATES: bays in use, of the policy's bound. Troubled when a bay
/// holds a corpse (`stale` — active past the gate Job's own deadline,
/// `yard::GATE_MAX_ACTIVE_HOURS`); busy at the bound or with a line
/// waiting for a slot. The trend is the gate duration, opened to
/// judged, for runs judged in each window.
fn gates(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let g = &inputs.status.gates;
    let capacity = usize::try_from(g.capacity).unwrap_or(0);
    let active = g.active.len();
    let durations = inputs.gate_runs.iter().filter_map(|run| {
        let opened = meta_instant(&run.metadata, "opened_at")?;
        let closed = closed_at(run)?;
        let d = (closed - opened).num_seconds();
        (d > 0).then_some((closed, d))
    });
    let (cur, prev) = split(w, durations);
    let trend = duration_trend("gate duration", "minutes", cur, prev);
    let stale = g.active.iter().filter(|a| a.stale).count();
    let (state, why) = if stale > 0 {
        (
            RegionState::Troubled,
            format!(
                "{} active past the gate deadline — a corpse holding a bay",
                plural(stale, "run", "runs")
            ),
        )
    } else if !g.queued.is_empty() {
        (
            RegionState::Busy,
            format!(
                "{active} of {capacity} bays in use, {} waiting for a slot",
                plural(g.queued.len(), "run", "runs")
            ),
        )
    } else if active >= capacity && capacity > 0 {
        (
            RegionState::Busy,
            format!("{active} of {capacity} bays in use — at the bound"),
        )
    } else {
        (
            RegionState::Clear,
            format!("{active} of {capacity} bays in use"),
        )
    };
    region("gates", Some(active), Some(capacity), state, why, trend)
}

/// THE TRACK: trains in transit. Troubled when the yard names a block
/// on any of them (`TrainStatus::block` — a red PR, a deploy refusal,
/// a converge overdue, a stall past the policy) or the conductor is
/// still waiting to file a train's gate ([`train_gate_troubled`]);
/// busy while a pre-merge train holds the single track. The trend is
/// the time at CI — the `pr` stamp to the `ci` verdict — for trains
/// that arrived in each window.
fn track(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let trains = &inputs.status.trains;
    let blocked: Vec<&str> = trains
        .iter()
        .filter(|t| t.block.is_some())
        .map(|t| t.title.as_str())
        .collect();
    let gate_waits: Vec<&str> = inputs
        .open_trains
        .iter()
        .filter(|(j, _)| train_gate_troubled(&j.metadata))
        .map(|(j, _)| j.title.as_str())
        .collect();
    // The single track's hold is the yard's own predicate — a train
    // before its merge; a merged one converging holds nothing.
    let on_track = inputs
        .open_trains
        .iter()
        .filter(|(_, s)| crate::yard::holds_the_track(s))
        .count();
    let ci_times = inputs.closed_trains.iter().filter_map(|(j, s)| {
        if j.metadata.get("outcome").and_then(Value::as_str) != Some("arrived") {
            return None;
        }
        let pr = step_done_at(find_step(s, "pr", "Open the batched PR"))?;
        let ci = step_done_at(find_step(s, "ci", "CI verdict"))?;
        let d = (ci - pr).num_seconds();
        (d >= 0).then_some((closed_at(j)?, d))
    });
    let (cur, prev) = split(w, ci_times);
    let trend = duration_trend("time at CI", "minutes", cur, prev);
    let (state, why) = if !blocked.is_empty() {
        (
            RegionState::Troubled,
            format!("blocked: {}", blocked.join(", ")),
        )
    } else if !gate_waits.is_empty() {
        (
            RegionState::Troubled,
            format!("gate not filed yet: {}", gate_waits.join(", ")),
        )
    } else if on_track > 0 {
        (
            RegionState::Busy,
            format!(
                "{} — a train holds the track",
                plural(trains.len(), "train in transit", "trains in transit")
            ),
        )
    } else {
        (
            RegionState::Clear,
            plural(trains.len(), "train in transit", "trains in transit"),
        )
    };
    region("track", Some(trains.len()), Some(1), state, why, trend)
}

/// The cars standing in the inspection shed: open cars whose live step
/// is `proven`. ONE predicate (CLAUDE.md §9a) — the shed region reads
/// it for its count, and the arrivals -> shed border
/// (`crate::borders`) reads it for what is waiting to cross.
pub(crate) fn awaiting_proof(cars: &[(Job, Vec<Step>)]) -> Vec<&(Job, Vec<Step>)> {
    cars.iter()
        .filter(|(j, s)| {
            j.status == boss_core::job::JobStatus::Open
                && s.iter()
                    .find(|st| matches!(st.status, StepStatus::Ready | StepStatus::Active))
                    .is_some_and(|st| st.spec_slug.as_deref() == Some("proven"))
        })
        .collect()
}

/// Trains cancelled in THIS window that released cars still
/// [`awaiting_repair`], each with the cars it left behind (a car the
/// read does not cover is named by its id — no evidence is not a
/// pass). The arrivals region's trouble and the track -> garage
/// border's queue are the same fact, so they read it here once.
pub(crate) fn released_awaiting_repair<'a>(
    closed_trains: &'a [(Job, Vec<Step>)],
    cars: &'a [(Job, Vec<Step>)],
    w: &Windows,
) -> Vec<(&'a str, Vec<&'a str>)> {
    let car_by_id: std::collections::HashMap<String, &Job> =
        cars.iter().map(|(j, _)| (j.id.to_string(), j)).collect();
    closed_trains
        .iter()
        .filter(|(j, _)| {
            j.metadata
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                != "arrived"
        })
        .filter(|(j, _)| closed_at(j).is_some_and(|t| w.current(t)))
        .filter_map(|(j, _)| {
            let waiting: Vec<&str> = j
                .metadata
                .get("boarded_jobs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|id| match car_by_id.get(id) {
                    Some(car) if awaiting_repair(car) => Some(
                        car.metadata
                            .get("branch")
                            .and_then(Value::as_str)
                            .unwrap_or(car.title.as_str()),
                    ),
                    Some(_) => None,
                    None => Some(id),
                })
                .collect();
            (!waiting.is_empty()).then_some((j.title.as_str(), waiting))
        })
        .collect()
}

/// THE SHED: landed cars awaiting proof — open cars whose live step is
/// `proven`. Troubled when one is UNPROVEN (no probe, no event: nothing
/// mechanical can settle it) or its probe is FAILING; busy while any
/// waits. The trend is cars proven per day.
fn shed(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let awaiting = awaiting_proof(inputs.cars);
    let mut unproven = Vec::new();
    let mut failing = Vec::new();
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        match shed_place(&j.metadata) {
            ShedPlace::Unproven => unproven.push(branch),
            ShedPlace::ProbePending { last: Some(_) } => failing.push(branch),
            _ => {}
        }
    }
    let proven = inputs
        .cars
        .iter()
        .filter_map(|(_, s)| step_done_at(find_step(s, "proven", "Proven in production")));
    let (cur, prev) = count_split(w, proven);
    let trend = rate_trend("proven", w, cur, prev);
    let n = awaiting.len();
    let (state, why) = if !unproven.is_empty() {
        (
            RegionState::Troubled,
            format!("UNPROVEN — no probe, no event: {}", unproven.join(", ")),
        )
    } else if !failing.is_empty() {
        (
            RegionState::Troubled,
            format!("probe FAILING: {}", failing.join(", ")),
        )
    } else if n > 0 {
        (
            RegionState::Busy,
            plural(n, "landed car awaiting proof", "landed cars awaiting proof"),
        )
    } else {
        (RegionState::Clear, "every landed car is proven".to_string())
    };
    region("shed", Some(n), None, state, why, trend)
}

/// A car a cancelled train released, judged: is it still waiting on
/// the repair the red asked for? Repaired means landed (the
/// conductor's `merged` marker or a closed `outcome=merged`, the one
/// definition in `car::is_landed`), re-gated (a fresh receipt rides
/// the car as `regate_receipt`), boarded again (`train` names a
/// train), or closed. A car released to the dock and not touched since
/// is still the red, live.
///
/// WHY (a106309c, 2026-09-19): train #461 was cancelled at 21:50Z with
/// two cars aboard; both re-parked and landed on #462 at 22:32Z, and
/// the arrivals card stayed troubled for 16 hours — the rule counted
/// that a red HAPPENED in the window, never asking what became of the
/// cars. A repaired red must stop looking troubled the way a troubled
/// packet must look troubled.
pub(crate) fn awaiting_repair(car: &Job) -> bool {
    let landed = serde_json::to_value(car).is_ok_and(|v| crate::car::is_landed(&v));
    let regated = car.metadata.get("regate_receipt").is_some();
    let reboarded = car
        .metadata
        .get("train")
        .and_then(Value::as_str)
        .is_some_and(|t| !t.is_empty());
    car.status == boss_core::job::JobStatus::Open && !landed && !regated && !reboarded
}

/// ARRIVALS: trains that arrived in the window. Troubled while a train
/// cancelled in the window WITH cars aboard — a red train, whose cars
/// went back to the dock (a board the consist check refused carries
/// none and is not trouble) — still has a car [`awaiting_repair`]; busy
/// while an arrival's siding is still converging. The trend is
/// arrivals per day; a cancellation stays in the record, not the state.
fn arrivals(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    fn outcome(j: &Job) -> &str {
        j.metadata
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
    }
    let arrived = inputs
        .closed_trains
        .iter()
        .filter(|(j, _)| outcome(j) == "arrived")
        .filter_map(|(j, _)| closed_at(j));
    let (cur, prev) = count_split(w, arrived);
    let trend = rate_trend("arrivals", w, cur, prev);
    // The cars read covers open cars and those closed within two
    // windows, so every car aboard a train cancelled in THIS window is
    // in it; one that is not is unread, and no evidence is not a pass —
    // it stays the red, named by its id. The predicate is shared with
    // the track -> garage border ([`released_awaiting_repair`]).
    let red: Vec<String> = released_awaiting_repair(inputs.closed_trains, inputs.cars, w)
        .into_iter()
        .map(|(train, cars)| format!("{train} ({})", cars.join(", ")))
        .collect();
    let converging = inputs
        .status
        .sidings
        .iter()
        .filter(|s| matches!(s.landing, crate::landing::Landing::Converging { .. }))
        .count();
    let (state, why) = if !red.is_empty() {
        (
            RegionState::Troubled,
            format!(
                "{} cancelled with cars aboard still awaiting repair: {}",
                plural(red.len(), "train", "trains"),
                red.join(", ")
            ),
        )
    } else if converging > 0 {
        (
            RegionState::Busy,
            format!(
                "{} in {}h, {} still converging",
                plural(cur, "arrival", "arrivals"),
                w.hours,
                plural(converging, "siding", "sidings")
            ),
        )
    } else {
        (
            RegionState::Clear,
            format!("{} in {}h", plural(cur, "arrival", "arrivals"), w.hours),
        )
    };
    region("arrivals", Some(cur), None, state, why, trend)
}

/// THE GARAGE: work off the main line — cars held on the dock, greens
/// held before parking, greens that never became a car (stranded),
/// reds awaiting rework, and runs the gate never judged (limbo).
/// Troubled when a green is stranded or a run sits in limbo: a green
/// no car claims is a fix about to be rebuilt blind, and "we do not
/// know" is not a verdict. Busy when the rest holds anything — a hold
/// is deliberate and a red is being worked. The trend is reds per day.
fn garage(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let s = inputs.status;
    let held = s.held_cars.len() + s.held.len();
    let stranded = s.stranded.len();
    let limbo = s.limbo.len();
    let red = s.garage.len();
    let count = held + stranded + limbo + red;
    let reds = inputs
        .gate_runs
        .iter()
        .filter(|r| r.metadata.get("outcome").and_then(Value::as_str) == Some("failed"))
        .filter_map(closed_at);
    let (cur, prev) = count_split(w, reds);
    let trend = rate_trend("reds", w, cur, prev);
    let (state, why) = if stranded > 0 || limbo > 0 {
        let mut parts = Vec::new();
        if stranded > 0 {
            parts.push(format!(
                "{} that never became a car",
                plural(stranded, "green gate", "green gates")
            ));
        }
        if limbo > 0 {
            parts.push(format!(
                "{} never judged",
                plural(limbo, "gate-run", "gate-runs")
            ));
        }
        (RegionState::Troubled, parts.join("; "))
    } else if count > 0 {
        (
            RegionState::Busy,
            format!("{held} held, {red} red awaiting rework"),
        )
    } else {
        (
            RegionState::Clear,
            "nothing held, stranded or red".to_string(),
        )
    };
    region("garage", Some(count), None, state, why, trend)
}

/// RECEIVING: inbound packets standing — feedback, alarms, findings,
/// design questions — from the moment they open until an actor takes
/// them in. The receiving yard's own bands decide the state: any
/// packet older than [`STALE_DAYS`] is troubled, older than
/// [`AGING_DAYS`] busy. The trend is inbound arrivals per day.
fn receiving(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let Some(inbound) = inputs.inbound else {
        return region(
            "receiving",
            None,
            None,
            RegionState::Troubled,
            "the workflow registry that names the inbound kinds could not be read".to_string(),
            Trend {
                metric: "inbound".to_string(),
                unit: "per day".to_string(),
                current: None,
                previous: None,
                samples: 0,
                previous_samples: 0,
            },
        );
    };
    let (cur, prev) = count_split(w, inbound.iter().filter_map(opened_at));
    let trend = rate_trend("inbound", w, cur, prev);
    let today = inputs.now.date_naive();
    let open: Vec<&Job> = inbound
        .iter()
        .filter(|j| j.status == boss_core::job::JobStatus::Open)
        .collect();
    let oldest = open
        .iter()
        .map(|j| (today - j.opened_on).num_days())
        .max()
        .unwrap_or(0);
    let n = open.len();
    let (state, why) = if oldest > STALE_DAYS {
        (
            RegionState::Troubled,
            format!(
                "{} standing, the oldest {oldest} days (past the {STALE_DAYS}-day band)",
                plural(n, "packet", "packets")
            ),
        )
    } else if oldest > AGING_DAYS {
        (
            RegionState::Busy,
            format!(
                "{} standing, the oldest {oldest} days (past the {AGING_DAYS}-day triage band)",
                plural(n, "packet", "packets")
            ),
        )
    } else {
        (
            RegionState::Clear,
            format!("{} standing", plural(n, "packet", "packets")),
        )
    };
    region("receiving", Some(n), None, state, why, trend)
}

/// MARSHALLING: packets standing at a station other than the dock,
/// counted once each. The marshalling yard's grammar decides the
/// state: a station over its WIP limit, or holding work that nothing
/// left in the window (not draining), is troubled; any station holding
/// work is busy. The trend is packets served per day, summed over the
/// stations whose flow the cube can count.
fn marshalling(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let Some(stations) = inputs.stations else {
        return region(
            "marshalling",
            None,
            None,
            RegionState::Troubled,
            "the station registry could not be read".to_string(),
            Trend {
                metric: "served".to_string(),
                unit: "per day".to_string(),
                current: None,
                previous: None,
                samples: 0,
                previous_samples: 0,
            },
        );
    };
    let distinct: std::collections::BTreeSet<&str> = stations
        .iter()
        .flat_map(|s| s.members.iter().map(String::as_str))
        .collect();
    let served: Option<i64> = stations
        .iter()
        .filter_map(|s| s.served)
        .reduce(|a, b| a + b);
    let previous: Option<i64> = stations
        .iter()
        .filter_map(|s| s.previous_served)
        .reduce(|a, b| a + b);
    let to_rate = |n: Option<i64>| {
        n.and_then(|n| usize::try_from(n).ok())
            .map(|n| w.per_day(n))
    };
    let trend = Trend {
        metric: "served".to_string(),
        unit: "per day".to_string(),
        current: to_rate(served),
        previous: to_rate(previous),
        samples: served.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
        previous_samples: previous.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
    };
    let over: Vec<&str> = stations
        .iter()
        .filter(|s| s.over_limit)
        .map(|s| s.name.as_str())
        .collect();
    let stuck: Vec<&str> = stations
        .iter()
        .filter(|s| !s.members.is_empty() && s.served == Some(0))
        .map(|s| s.name.as_str())
        .collect();
    let holding = stations.iter().filter(|s| !s.members.is_empty()).count();
    let n = distinct.len();
    let (state, why) = if !over.is_empty() {
        (
            RegionState::Troubled,
            format!("over the WIP limit: {}", over.join(", ")),
        )
    } else if !stuck.is_empty() {
        (
            RegionState::Troubled,
            format!(
                "not draining — nothing left in {}h: {}",
                w.hours,
                stuck.join(", ")
            ),
        )
    } else if holding > 0 {
        (
            RegionState::Busy,
            format!(
                "{} standing at {}",
                plural(n, "packet", "packets"),
                plural(holding, "station", "stations")
            ),
        )
    } else {
        (
            RegionState::Clear,
            "nothing waiting at any station".to_string(),
        )
    };
    region("marshalling", Some(n), None, state, why, trend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yard::{BoardingReadings, YardInputs, build_status_for};
    use boss_core::job::{JobId, JobStatus, Priority, StepId, Subject};
    use serde_json::json;

    const NOW: &str = "2026-09-19T12:00:00Z";

    fn t(s: &str) -> Instant {
        parse_instant(s).unwrap()
    }

    fn job(kind: &str, title: &str, status: JobStatus, metadata: Value) -> Job {
        Job {
            id: JobId::new(),
            kind: kind.into(),
            workflow_version: 1,
            subject: Subject::new("custom", "s"),
            title: title.into(),
            owner_id: "emp-david".into(),
            status,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
            due_on: None,
            closed_on: None,
            metadata,
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    fn step(job: &Job, slug: &str, status: StepStatus, done_at: Option<&str>) -> Step {
        let mut s = Step::new(job.id, "task", slug, 0);
        s.id = StepId::new();
        s.spec_slug = Some(slug.into());
        s.status = status;
        s.completed_at = done_at.map(t);
        s
    }

    /// An empty yard: every read answered, nothing in it.
    fn empty_status() -> YardStatus {
        build_status_for(
            YardInputs {
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        )
    }

    fn inputs<'a>(
        status: &'a YardStatus,
        open_trains: &'a [(Job, Vec<Step>)],
        closed_trains: &'a [(Job, Vec<Step>)],
        cars: &'a [(Job, Vec<Step>)],
        gate_runs: &'a [Job],
        inbound: Option<&'a [Job]>,
        stations: Option<&'a [StationReading]>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            status,
            dock_reading: Reading::Read,
            open_trains,
            closed_trains,
            cars,
            gate_runs,
            inbound,
            stations,
            now: t(NOW),
            window_hours: 24,
        }
    }

    fn by_name<'a>(r: &'a Regions, name: &str) -> &'a Region {
        r.regions.iter().find(|x| x.name == name).unwrap()
    }

    #[test]
    fn an_empty_yard_answers_eight_clear_regions_in_map_order() {
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let names: Vec<&str> = out.regions.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, REGIONS);
        for r in &out.regions {
            assert_eq!(r.state, RegionState::Clear, "{}: {}", r.name, r.why);
            assert_eq!(r.count, Some(0), "{}", r.name);
        }
        // A duration nobody measured is null, never zero; a count of
        // nothing is a rate of zero.
        assert_eq!(by_name(&out, "track").trend.current, None);
        assert_eq!(by_name(&out, "arrivals").trend.current, Some(0.0));
        assert_eq!(out.window_hours, 24);
    }

    #[test]
    fn an_unread_dock_and_unread_registries_are_troubled_not_empty() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], None, None);
        i.dock_reading = Reading::Unread;
        let out = regions(&i);
        for name in ["dock", "receiving", "marshalling"] {
            let r = by_name(&out, name);
            assert_eq!(r.state, RegionState::Troubled, "{name}");
            assert_eq!(r.count, None, "{name}");
            assert!(r.why.contains("could not be read"), "{name}: {}", r.why);
        }
    }

    /// A train the yard already judges blocked (a red PR) and one whose
    /// gate the conductor cannot file — both trouble on the track, from
    /// the yard's own predicate and the ported client one.
    #[test]
    fn a_blocked_train_or_an_unfiled_train_gate_troubles_the_track() {
        let train = job("pr-train", "train #470", JobStatus::Open, json!({}));
        let steps = vec![
            step(
                &train,
                "collect",
                StepStatus::Completed,
                Some("2026-09-19T11:00:00Z"),
            ),
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            {
                let mut s = step(
                    &train,
                    "ci",
                    StepStatus::Completed,
                    Some("2026-09-19T11:20:00Z"),
                );
                s.metadata = json!({ "result": "failing" });
                s
            },
            step(&train, "merged", StepStatus::Ready, None),
        ];
        let open = vec![(train, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        assert_eq!(track.count, Some(1));
        assert_eq!(track.state, RegionState::Troubled);
        assert!(track.why.contains("blocked: train #470"), "{}", track.why);

        // The same train, CI still pending, but the conductor waiting to
        // file its gate — trouble the client used to see alone.
        let waiting = job(
            "pr-train",
            "train #471",
            JobStatus::Open,
            json!({ TRAIN_GATE_WAIT_REASON: "bound: 3 running" }),
        );
        let steps = vec![
            step(
                &waiting,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&waiting, "ci", StepStatus::Ready, None),
            step(&waiting, "merged", StepStatus::Pending, None),
        ];
        let open = vec![(waiting, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        assert_eq!(track.state, RegionState::Troubled);
        assert!(
            track.why.contains("gate not filed yet: train #471"),
            "{}",
            track.why
        );
        // A blank marker is no marker (yard.ts `text()`).
        assert!(!train_gate_troubled(&json!({ TRAIN_GATE_WAIT_REASON: "" })));
        assert!(train_gate_troubled(
            &json!({ TRAIN_GATE_FALLBACK: "CI alone" })
        ));
    }

    /// A healthy pre-merge train holds the single track: busy, not
    /// troubled; a merged one converging holds nothing.
    #[test]
    fn a_pre_merge_train_holds_the_track_and_reads_busy() {
        let train = job("pr-train", "train #472", JobStatus::Open, json!({}));
        let steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&train, "ci", StepStatus::Ready, None),
            step(&train, "merged", StepStatus::Pending, None),
        ];
        let open = vec![(train, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "track").state, RegionState::Busy);
        assert_eq!(by_name(&out, "track").bound, Some(1));
    }

    /// Arrivals per day this window against the previous, and the time
    /// at CI as a median of the arrivals in each — read off the closed
    /// trains' stamps; a cancelled train with cars aboard troubles the
    /// arrivals region, one refused by the consist check does not.
    #[test]
    fn arrivals_count_the_window_and_the_track_trend_is_the_time_at_ci() {
        let arrived = |title: &str, pr: &str, ci: &str, closed: &str| {
            let j = job(
                "pr-train",
                title,
                JobStatus::Closed,
                json!({ "outcome": "arrived", "closed_at": closed, "boarded_jobs": ["a"] }),
            );
            let s = vec![
                step(&j, "pr", StepStatus::Completed, Some(pr)),
                step(&j, "ci", StepStatus::Completed, Some(ci)),
            ];
            (j, s)
        };
        let closed = vec![
            // today: CI took 10 and 20 minutes
            arrived(
                "t1",
                "2026-09-19T08:00:00Z",
                "2026-09-19T08:10:00Z",
                "2026-09-19T08:30:00Z",
            ),
            arrived(
                "t2",
                "2026-09-19T09:00:00Z",
                "2026-09-19T09:20:00Z",
                "2026-09-19T09:40:00Z",
            ),
            // yesterday: 40 minutes
            arrived(
                "t3",
                "2026-09-18T08:00:00Z",
                "2026-09-18T08:40:00Z",
                "2026-09-18T09:00:00Z",
            ),
            // older than two windows: not counted anywhere
            arrived(
                "t4",
                "2026-09-10T08:00:00Z",
                "2026-09-10T09:40:00Z",
                "2026-09-10T10:00:00Z",
            ),
            // a refused board today: no cars, no trouble
            (
                job(
                    "pr-train",
                    "refused",
                    JobStatus::Closed,
                    json!({ "outcome": "cancelled", "closed_at": "2026-09-19T10:00:00Z" }),
                ),
                vec![],
            ),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &closed,
            &[],
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let a = by_name(&out, "arrivals");
        assert_eq!(a.count, Some(2));
        assert_eq!(a.state, RegionState::Clear, "{}", a.why);
        assert_eq!(a.trend.current, Some(2.0));
        assert_eq!(a.trend.previous, Some(1.0));
        let track = by_name(&out, "track").trend.clone();
        assert_eq!(track.metric, "time at CI");
        assert_eq!(track.unit, "minutes");
        // Two samples: the middle observation is the upper one.
        assert_eq!(track.current, Some(20.0));
        assert_eq!(track.previous, Some(40.0));
        assert_eq!((track.samples, track.previous_samples), (2, 1));

        // A red train — cancelled with a car aboard — is trouble. The
        // car is not in the cars read here: unread is not repaired, so
        // the red stays and names the id (a106309c).
        let mut with_red = closed.clone();
        with_red.push((
            job(
                "pr-train",
                "train #469",
                JobStatus::Closed,
                json!({ "outcome": "cancelled", "closed_at": "2026-09-19T11:00:00Z", "boarded_jobs": ["c"] }),
            ),
            vec![],
        ));
        let out = regions(&inputs(
            &status,
            &[],
            &with_red,
            &[],
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let a = by_name(&out, "arrivals");
        assert_eq!(a.state, RegionState::Troubled);
        assert!(a.why.contains("train #469"), "{}", a.why);
    }

    /// A cancelled train troubles the arrivals only while a car it
    /// carried is still waiting on the repair (a106309c, measured
    /// 2026-09-19: train #461 cancelled 21:50Z, its two cars landed on
    /// #462 at 22:32Z, and the map stayed red for 16 hours — the rule
    /// measured that trouble HAPPENED, not that it exists). A car that
    /// since merged, was re-gated, boarded a newer train, or closed is
    /// repaired; one still sitting unregated on the dock is not.
    #[test]
    fn a_cancelled_train_stops_troubling_arrivals_once_its_cars_are_repaired() {
        let status = empty_status();
        let cancelled = |cars: &[&Job]| {
            let ids: Vec<String> = cars.iter().map(|c| c.id.to_string()).collect();
            (
                job(
                    "pr-train",
                    "train #461",
                    JobStatus::Closed,
                    json!({ "outcome": "cancelled", "closed_at": "2026-09-19T11:00:00Z", "boarded_jobs": ids }),
                ),
                vec![],
            )
        };
        let car = |branch: &str, status: JobStatus, md: Value| {
            let mut md = md;
            md["branch"] = json!(branch);
            (job("ship-a-change", branch, status, md), vec![])
        };
        let arrivals = |cars: Vec<(Job, Vec<Step>)>| {
            let closed = vec![cancelled(&cars.iter().map(|(j, _)| j).collect::<Vec<_>>())];
            let out = regions(&inputs(
                &status,
                &[],
                &closed,
                &cars,
                &[],
                Some(&[]),
                Some(&[]),
            ));
            by_name(&out, "arrivals").clone()
        };

        // Released to the dock and not touched since: the red is live.
        let a = arrivals(vec![car(
            "fix/a",
            JobStatus::Open,
            json!({ "skip_reason": "returned to dock: train cancelled (red)" }),
        )]);
        assert_eq!(a.state, RegionState::Troubled, "{}", a.why);
        assert!(a.why.contains("train #461"), "{}", a.why);

        // The packet's case: the car since merged (the conductor's
        // landing marker, before the dispatcher closes the Job).
        let a = arrivals(vec![car(
            "fix/a",
            JobStatus::Open,
            json!({ "merged": "true", "train": "a-newer-train" }),
        )]);
        assert_eq!(a.state, RegionState::Clear, "{}", a.why);
        // Cancellations stay in the record, not in the state: the count
        // and the trend are the arrivals', unchanged.
        assert_eq!(a.count, Some(0));

        // Re-gated (a fresh receipt rides the car), boarded a newer
        // train, or closed: each is the repair under way or done.
        let a = arrivals(vec![car(
            "fix/a",
            JobStatus::Open,
            json!({ "regate_receipt": "GATE_VERDICT=green" }),
        )]);
        assert_eq!(a.state, RegionState::Clear, "{}", a.why);
        let a = arrivals(vec![car(
            "fix/a",
            JobStatus::Open,
            json!({ "train": "a-newer-train" }),
        )]);
        assert_eq!(a.state, RegionState::Clear, "{}", a.why);
        let a = arrivals(vec![car(
            "fix/a",
            JobStatus::Closed,
            json!({ "outcome": "abandoned" }),
        )]);
        assert_eq!(a.state, RegionState::Clear, "{}", a.why);

        // Two cars aboard: one repaired, one not — still troubled, and
        // the why names the car still waiting.
        let a = arrivals(vec![
            car("fix/a", JobStatus::Open, json!({ "merged": "true" })),
            car("fix/b", JobStatus::Open, json!({})),
        ]);
        assert_eq!(a.state, RegionState::Troubled, "{}", a.why);
        assert!(a.why.contains("fix/b"), "{}", a.why);
        assert!(!a.why.contains("fix/a"), "{}", a.why);
    }

    /// The shed counts open cars at `proven`; an unproven one troubles
    /// it; the trend is cars proven per day.
    #[test]
    fn the_shed_counts_cars_awaiting_proof_and_an_unproven_one_troubles_it() {
        let car = |branch: &str, md: Value, proven_at: Option<&str>| {
            let mut md = md;
            md["branch"] = json!(branch);
            let open = proven_at.is_none();
            let j = job(
                "ship-a-change",
                branch,
                if open {
                    JobStatus::Open
                } else {
                    JobStatus::Closed
                },
                md,
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-19T06:00:00Z"),
                ),
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-19T07:00:00Z"),
                ),
                if let Some(at) = proven_at {
                    step(&j, "proven", StepStatus::Completed, Some(at))
                } else {
                    step(&j, "proven", StepStatus::Ready, None)
                },
            ];
            (j, s)
        };
        let cars = vec![
            car("fix/a", json!({ "proof_probe": "true" }), None),
            car(
                "fix/b",
                json!({ "proof_event": "the next red train" }),
                None,
            ),
            car("fix/c", json!({}), Some("2026-09-19T08:00:00Z")),
            car("fix/d", json!({}), Some("2026-09-18T08:00:00Z")),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(2));
        assert_eq!(shed.state, RegionState::Busy, "{}", shed.why);
        assert_eq!(shed.trend.metric, "proven");
        assert_eq!(shed.trend.current, Some(1.0));
        assert_eq!(shed.trend.previous, Some(1.0));

        let mut with_unproven = cars.clone();
        with_unproven.push(car("fix/e", json!({}), None));
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &with_unproven,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(3));
        assert_eq!(shed.state, RegionState::Troubled);
        assert!(
            shed.why.contains("UNPROVEN") && shed.why.contains("fix/e"),
            "{}",
            shed.why
        );
    }

    /// The dock wait: the car's gate stamp to its train's collect
    /// stamp, medianed over the cars that boarded in each window.
    #[test]
    fn the_dock_trend_is_the_wait_from_gate_green_to_boarding() {
        let train = job(
            "pr-train",
            "train",
            JobStatus::Closed,
            json!({ "outcome": "arrived" }),
        );
        let train_id = train.id.to_string();
        let trains = vec![(
            train.clone(),
            vec![step(
                &train,
                "collect",
                StepStatus::Completed,
                Some("2026-09-19T10:00:00Z"),
            )],
        )];
        let car = |gate_at: &str| {
            let j = job(
                "ship-a-change",
                "car",
                JobStatus::Closed,
                json!({ "branch": "fix/x", "train": train_id, "outcome": "merged" }),
            );
            let s = vec![step(&j, "gate", StepStatus::Completed, Some(gate_at))];
            (j, s)
        };
        let cars = vec![car("2026-09-19T08:00:00Z"), car("2026-09-19T09:30:00Z")];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &trains,
            &cars,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let dock = by_name(&out, "dock");
        assert_eq!(dock.trend.metric, "dock wait");
        assert_eq!(dock.trend.unit, "hours");
        assert_eq!(dock.trend.current, Some(2.0));
        assert_eq!(dock.trend.previous, None);
        assert_eq!(dock.trend.samples, 2);
    }

    /// The gates: a stale bay is trouble; a queue is busy; the trend is
    /// the run duration from the rows' own stamps.
    #[test]
    fn the_gates_read_the_bound_and_a_stale_bay_is_trouble() {
        let run = |branch: &str, opened: &str, closed: Option<&str>, outcome: &str| {
            let mut md = json!({ "branch": branch, "opened_at": opened });
            if let Some(c) = closed {
                md["closed_at"] = json!(c);
                md["outcome"] = json!(outcome);
            }
            job(
                "gate-run",
                branch,
                if closed.is_some() {
                    JobStatus::Closed
                } else {
                    JobStatus::Open
                },
                md,
            )
        };
        let runs = vec![
            run(
                "fix/a",
                "2026-09-19T09:00:00Z",
                Some("2026-09-19T09:30:00Z"),
                "completed",
            ),
            run(
                "fix/b",
                "2026-09-19T10:00:00Z",
                Some("2026-09-19T10:10:00Z"),
                "failed",
            ),
            run(
                "fix/c",
                "2026-09-18T10:00:00Z",
                Some("2026-09-18T11:00:00Z"),
                "failed",
            ),
            // A corpse: active since well past GATE_MAX_ACTIVE_HOURS.
            run("fix/d", "2026-09-19T01:00:00Z", None, ""),
        ];
        let with_steps: Vec<(Job, Vec<Step>)> = runs
            .iter()
            .map(|j| {
                (
                    j.clone(),
                    vec![step(j, "record-verdict", StepStatus::Ready, None)],
                )
            })
            .collect();
        let status = build_status_for(
            YardInputs {
                gate_runs: &with_steps,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &[], &[], &[], &runs, Some(&[]), Some(&[])));
        let gates = by_name(&out, "gates");
        assert_eq!(gates.count, Some(1));
        assert_eq!(gates.bound, Some(3));
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("corpse"), "{}", gates.why);
        assert_eq!(gates.trend.metric, "gate duration");
        assert_eq!(gates.trend.current, Some(30.0));
        assert_eq!(gates.trend.previous, Some(60.0));
        // The garage's trend: reds per day.
        let garage = by_name(&out, "garage");
        assert_eq!(garage.trend.metric, "reds");
        assert_eq!(garage.trend.current, Some(1.0));
        assert_eq!(garage.trend.previous, Some(1.0));
    }

    /// A stranded green — a green gate no car claims — troubles the
    /// garage; a held car alone is busy.
    #[test]
    fn a_stranded_green_troubles_the_garage() {
        let run = job(
            "gate-run",
            "fix/lost",
            JobStatus::Closed,
            json!({ "branch": "fix/lost", "opened_at": "2026-09-19T09:00:00Z", "closed_at": "2026-09-19T09:30:00Z", "outcome": "completed" }),
        );
        let mut verdict = step(
            &run,
            "record-verdict",
            StepStatus::Completed,
            Some("2026-09-19T09:30:00Z"),
        );
        verdict.metadata = json!({ "verdict": "green" });
        let runs = vec![(run.clone(), vec![verdict])];
        let status = build_status_for(
            YardInputs {
                gate_runs: &runs,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        assert_eq!(status.stranded.len(), 1, "the yard's own stranded lane");
        let rows = vec![run];
        let out = regions(&inputs(&status, &[], &[], &[], &rows, Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(garage.count, Some(1));
        assert_eq!(garage.state, RegionState::Troubled);
        assert!(garage.why.contains("never became a car"), "{}", garage.why);
    }

    /// Receiving: the age bands from `receiving.ts`, and arrivals per
    /// day from the packets' own open stamps.
    #[test]
    fn receiving_reads_the_age_bands_and_inbound_arrivals() {
        let mut fresh = job(
            "user-feedback",
            "fresh",
            JobStatus::Open,
            json!({ "opened_at": "2026-09-19T09:00:00Z" }),
        );
        fresh.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
        let mut aging = job("backlog-item", "aging", JobStatus::Open, json!({}));
        aging.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let mut stale = job("backlog-item", "stale", JobStatus::Open, json!({}));
        stale.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let mut yesterday = job(
            "user-feedback",
            "done",
            JobStatus::Closed,
            json!({ "opened_at": "2026-09-18T09:00:00Z" }),
        );
        yesterday.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let status = empty_status();
        let inbound = vec![fresh.clone(), aging.clone()];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(2));
        assert_eq!(r.state, RegionState::Busy, "{}", r.why);
        assert_eq!(r.trend.current, Some(1.0));
        assert_eq!(r.trend.previous, Some(0.0));

        let inbound = vec![fresh, aging, stale, yesterday];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(3));
        assert_eq!(r.state, RegionState::Troubled, "{}", r.why);
        assert!(r.why.contains("30 days"), "{}", r.why);
        assert_eq!(r.trend.previous, Some(1.0));
    }

    /// Marshalling: packets counted once across stations; a station
    /// nothing leaves is trouble; served per day sums the countable
    /// stations.
    #[test]
    fn marshalling_counts_packets_once_and_a_station_not_draining_is_trouble() {
        let stations = vec![
            StationReading {
                name: "q.platform-admin.task".into(),
                over_limit: false,
                members: vec!["p1".into(), "p2".into()],
                served: Some(4),
                previous_served: Some(2),
            },
            StationReading {
                name: "design-review".into(),
                over_limit: false,
                members: vec!["p2".into()],
                served: Some(1),
                previous_served: Some(0),
            },
            StationReading {
                name: "my-watchlist".into(),
                over_limit: false,
                members: vec![],
                served: None,
                previous_served: None,
            },
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stations),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(2), "p2 stands at two stations, counted once");
        assert_eq!(m.state, RegionState::Busy, "{}", m.why);
        assert_eq!(m.trend.current, Some(5.0));
        assert_eq!(m.trend.previous, Some(2.0));

        let mut stuck = stations.clone();
        stuck[1].served = Some(0);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&[]),
            Some(&stuck),
        ));
        let m = by_name(&out, "marshalling");
        assert_eq!(m.state, RegionState::Troubled);
        assert!(m.why.contains("design-review"), "{}", m.why);
    }

    #[test]
    fn the_window_parses_hours_days_and_bare_hours_and_refuses_the_rest() {
        assert_eq!(parse_window(None), Ok(24));
        assert_eq!(parse_window(Some("")), Ok(24));
        assert_eq!(parse_window(Some("48h")), Ok(48));
        assert_eq!(parse_window(Some("7d")), Ok(168));
        assert_eq!(parse_window(Some("36")), Ok(36));
        assert!(parse_window(Some("8d")).is_err());
        assert!(parse_window(Some("0h")).is_err());
        assert!(parse_window(Some("soon")).is_err());
    }

    /// The window is a query parameter: the same rows answer a wider
    /// trend when asked for one.
    #[test]
    fn a_wider_window_counts_more_of_the_same_rows() {
        let arrived = |closed: &str| {
            (
                job(
                    "pr-train",
                    "t",
                    JobStatus::Closed,
                    json!({ "outcome": "arrived", "closed_at": closed }),
                ),
                vec![],
            )
        };
        let closed = vec![
            arrived("2026-09-19T08:00:00Z"),
            arrived("2026-09-17T08:00:00Z"),
        ];
        let status = empty_status();
        let mut i = inputs(&status, &[], &closed, &[], &[], Some(&[]), Some(&[]));
        assert_eq!(by_name(&regions(&i), "arrivals").count, Some(1));
        i.window_hours = 72;
        let a = by_name(&regions(&i), "arrivals").clone();
        assert_eq!(a.count, Some(2));
        assert_eq!(a.trend.current, Some(2.0 / 3.0));
    }

    /// The shed classification is total and the probe wins over an
    /// event — the same rule `boss orient` prints by (it now calls this).
    #[test]
    fn the_shed_place_is_total_and_the_probe_wins() {
        assert_eq!(shed_place(&json!({})), ShedPlace::Unproven);
        assert_eq!(
            shed_place(&json!({ "proof_event": "next red train" })),
            ShedPlace::WaitingOn("next red train".into())
        );
        assert_eq!(
            shed_place(&json!({ "proof_probe": "true", "proof_event": "x" })),
            ShedPlace::ProbePending { last: None }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 75, "why": "not yet: no train" } })
            ),
            ShedPlace::ProbeNotYet {
                said: "not yet: no train".into()
            }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 1, "why": "FAILED" } })
            ),
            ShedPlace::ProbePending {
                last: Some("FAILED".into())
            }
        );
    }

    /// The two facts ported from the receiving yard's TypeScript live
    /// twice; this holds them equal (CLAUDE.md §9a). Reads the source
    /// the client is built from, so a kind added to one side and not
    /// the other names itself here.
    #[test]
    fn the_pipeline_kinds_and_age_bands_match_the_receiving_yard() {
        let src = std::fs::read_to_string(
            boss_testing::repo_root().join("apps/web/src/it/receiving/receiving.ts"),
        )
        .expect("receiving.ts is in the tree");
        let set = src
            .split("PIPELINE_KINDS: ReadonlySet<string> = new Set([")
            .nth(1)
            .and_then(|rest| rest.split("]);").next())
            .expect("receiving.ts declares PIPELINE_KINDS as a Set literal");
        let ts_kinds: Vec<&str> = set
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            ts_kinds, PIPELINE_KINDS,
            "receiving.ts::PIPELINE_KINDS drifted from regions.rs::PIPELINE_KINDS"
        );
        assert!(
            src.contains(&format!(
                "AGE_THRESHOLDS = {{ aging: {AGING_DAYS}, stale: {STALE_DAYS} }}"
            )),
            "receiving.ts::AGE_THRESHOLDS drifted from regions.rs::AGING_DAYS / STALE_DAYS"
        );
        // The train-gate keys the yard's `readTrainGate` reads.
        let yard =
            std::fs::read_to_string(boss_testing::repo_root().join("apps/web/src/it/yard/yard.ts"))
                .expect("yard.ts is in the tree");
        for key in [TRAIN_GATE_WAIT_REASON, TRAIN_GATE_FALLBACK] {
            assert!(
                yard.contains(&format!("md.{key}")),
                "yard.ts no longer reads {key}"
            );
        }
    }

    #[test]
    fn inbound_kinds_are_active_platform_kinds_minus_chores_and_the_pipeline() {
        let spec = |kind: &str, category: &str| WorkflowSpec {
            kind: kind.into(),
            version: 1,
            status: crate::registry::WorkflowStatus::Active,
            label: kind.into(),
            description: None,
            category: category.into(),
            subject_kinds: vec![],
            steps: vec![],
            metadata_schema: json!({}),
            entitlements: json!({}),
            metadata: json!({}),
            on_complete_create: vec![],
            owning_team: "it".into(),
            authoring_job_id: None,
            created_at: t(NOW),
        };
        let specs = vec![
            spec("user-feedback", "platform"),
            spec("backlog-item", "platform"),
            spec("maintenance-nightly", "platform"),
            spec("pr-train", "platform"),
            spec("sale", "commerce"),
        ];
        assert_eq!(inbound_kinds(&specs), vec!["backlog-item", "user-feedback"]);
    }
}
