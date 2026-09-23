//! `CounterpartyEngine` — listens to bus events, queues a deferred
//! emission per matched event with `delay` business-day offset and
//! `emit_probability` Bernoulli, drains due rows on each day-step.
//! Spec types, the queue, the day step, followup chains, and the
//! `scans` sugar all live here.
//!
//! Counterparty behavior is modeled actor behavior, sim-side: the
//! deferred emissions stand in for the HTTP API calls a real
//! counterparty (or their bank's ACH webhook) would make
//! (docs/architecture-decisions.md §Dispatcher — the event router).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_activity::ActorKind;
use crate::calendar::CalendarRegistry;
use crate::engines::{DayContext, SimBusEvent, SimEngine};

/// Tenant-supplied delay shape. `mean_days` is the expected delay;
/// `spread_days` is a uniform spread around it (sample = mean ±
/// spread, clamped to ≥ 0). When `business_calendar` is set, the
/// final emit date snaps forward to the next business day.
#[derive(Debug, Clone, Deserialize)]
pub struct DelaySpec {
    pub mean_days: f64,
    #[serde(default)]
    pub spread_days: f64,
    #[serde(default)]
    pub business_calendar: Option<String>,
}

impl DelaySpec {
    /// Sample a concrete number of days from the spec. Uses the
    /// passed RNG so determinism is preserved.
    pub fn sample_days(&self, rng: &mut crate::rng::Rng) -> i64 {
        let raw = if self.spread_days <= 0.0 {
            self.mean_days
        } else {
            let r = rng.next() * 2.0 - 1.0; // [-1, 1)
            self.mean_days + r * self.spread_days
        };
        raw.max(0.0).round() as i64
    }
}

/// Single hop in a counterparty interaction. `listens_to` matches on
/// exact topic. Followups (and the `scans` sugar) flatten into more
/// `CounterpartySpec` rows in the engine state, so internally there
/// is only one shape.
#[derive(Debug, Clone, Deserialize)]
pub struct CounterpartySpec {
    /// Stable identifier — bus events emitted by this counterparty
    /// carry `source = "counterparty:<name>"`.
    pub name: String,
    /// Real-world actor kind this chain represents — `account` | `vendor`
    /// | `bank` | `environment`. The cockpit groups by this, not the raw
    /// chain name. Set on the root `[counterparty.*]`; followups + scan
    /// stages inherit it via their qualified name's root segment. Absent →
    /// `environment` (the catch-all).
    #[serde(default)]
    pub actor_kind: Option<String>,
    /// Bus topic this hop listens to. Exact-string match.
    pub listens_to: String,
    pub delay: DelaySpec,
    /// Probability the trigger is honored. `1.0` = always emit,
    /// `0.985` = drop ~1.5% of triggers (e.g. card declines, lost
    /// shipments).
    #[serde(default = "default_probability")]
    pub emit_probability: f64,
    /// Topic to publish when this hop fires.
    pub emits: String,
    /// Optional alternate topic to publish when the
    /// `emit_probability` roll fails. Without this, a failed
    /// roll silently drops the trigger — fine for "30% of NSF
    /// failures stay invisible" semantics, but not for the
    /// mutually-exclusive case (paid vs past-due, on-time vs
    /// late, accept vs reject) where every trigger MUST take
    /// exactly one branch. Sharing the spec keeps the rolls
    /// disjoint by construction.
    #[serde(default)]
    pub emit_else: Option<String>,
    /// Static fields to merge into the emitted payload. The original
    /// trigger event's payload is folded in under `trigger`. Engines
    /// + tests can read both.
    #[serde(default)]
    pub payload: Value,
    /// Optional follow-up hops. Each fires off the topic this hop
    /// emits. Same shape recursively.
    #[serde(default)]
    pub followups: Vec<CounterpartySpec>,
    /// Optional sugar for a chain of N successive emissions (carrier
    /// scans, multi-stage AP three-way match, …). Mutually exclusive
    /// with `followups` — if both are non-empty, the spec is invalid;
    /// the engine panics at construction time so the misconfiguration
    /// is loud rather than silently ignored.
    ///
    /// Each scan flattens to one followup. Stage 0 listens to this
    /// spec's `listens_to` and emits `<emits>_<status>` where status
    /// is the scan's `status` field. Stage N>0 listens to stage N-1's
    /// emission. Each scan inherits the parent's `emit_probability`
    /// (no per-stage probability — if the carrier loses the package,
    /// the whole chain stops, not one scan).
    #[serde(default)]
    pub scans: Vec<ScanSpec>,
    /// Optional predicate over the trigger event's payload. When
    /// non-empty, every (key, value) pair must match the trigger
    /// payload at JSON-path `key`. Keys are dotted paths
    /// (`metadata.vendor.category`). Values compare via strict
    /// `serde_json::Value` equality. An array on the RHS means
    /// "any of": the payload value must equal one of the array's
    /// elements.
    ///
    /// Empty / absent (the default) means "match every event on
    /// this topic" — preserves the pre-filter behavior. Used to
    /// fan multiple counterparty specs off a single topic by
    /// payload metadata (per-vendor-category, per-region, …).
    ///
    /// Predicate is checked against the *original* trigger event's
    /// payload, not the static `payload` block on the spec. Not
    /// inherited by `followups` / `scans` children — those listen
    /// to topics this spec already emitted (after the predicate
    /// passed), so re-checking would be redundant.
    #[serde(default)]
    pub match_payload: serde_json::Map<String, Value>,
}

/// One stage in a `scans` chain. `status` becomes the suffix on the
/// emitted topic so subscribers can match the specific stage they
/// care about.
#[derive(Debug, Clone, Deserialize)]
pub struct ScanSpec {
    pub status: String,
    pub delay: DelaySpec,
}

fn default_probability() -> f64 {
    1.0
}

impl CounterpartySpec {
    /// Walk this spec + every followup (or expanded `scans` chain),
    /// returning each hop with its fully-qualified name
    /// (parent.child). Used by the engine to flatten the recursive
    /// structure into a flat lookup table.
    pub fn flatten(&self, parent: Option<&str>) -> Vec<CounterpartySpec> {
        if !self.followups.is_empty() && !self.scans.is_empty() {
            panic!(
                "CounterpartySpec `{}` declares both `followups` and `scans` — pick one",
                self.name
            );
        }
        let qualified_name = match parent {
            Some(p) => format!("{p}.{}", self.name),
            None => self.name.clone(),
        };
        let mut out = Vec::new();

        if self.scans.is_empty() {
            // Standard path: one root + recursive followups. The
            // root carries `match_payload` so the engine can filter
            // before scheduling. Children listen to topics this
            // root already emitted (post-filter), so they get an
            // empty predicate — no redundant re-checking.
            out.push(CounterpartySpec {
                name: qualified_name.clone(),
                actor_kind: self.actor_kind.clone(),
                listens_to: self.listens_to.clone(),
                delay: self.delay.clone(),
                emit_probability: self.emit_probability,
                emits: self.emits.clone(),
                emit_else: self.emit_else.clone(),
                payload: self.payload.clone(),
                followups: vec![],
                scans: vec![],
                match_payload: self.match_payload.clone(),
            });
            for f in &self.followups {
                out.extend(f.flatten(Some(&qualified_name)));
            }
        } else {
            // Scans path: expand to a chain. Stage 0 listens to the
            // parent's `listens_to` and emits `<emits>_<status>`.
            // Stage N>0 listens to stage N-1's emission topic.
            let mut prev_emit = self.listens_to.clone();
            for (i, scan) in self.scans.iter().enumerate() {
                let stage_topic = format!("{}_{}", self.emits, sanitize_status(&scan.status));
                let stage_name = format!("{qualified_name}.{}", scan.status);
                out.push(CounterpartySpec {
                    name: stage_name,
                    actor_kind: self.actor_kind.clone(),
                    listens_to: prev_emit.clone(),
                    delay: scan.delay.clone(),
                    // Each stage inherits the parent's emit_probability.
                    // A failed roll on stage N stops the chain — the next
                    // stage never gets a trigger because nothing was
                    // emitted on this stage's topic.
                    emit_probability: self.emit_probability,
                    emits: stage_topic.clone(),
                    emit_else: None,
                    payload: stage_payload(&self.payload, &scan.status, i),
                    followups: vec![],
                    scans: vec![],
                    // Stage 0 inherits the parent's payload predicate
                    // so a payload-filtered scan chain (e.g. fedex
                    // scans on `step.done.shipment` only when the
                    // carrier metadata says fedex) gates correctly.
                    // Stages N>0 listen to topics stage N-1 already
                    // emitted post-filter, so they carry an empty
                    // predicate.
                    match_payload: if i == 0 {
                        self.match_payload.clone()
                    } else {
                        serde_json::Map::new()
                    },
                });
                prev_emit = stage_topic;
            }
        }
        out
    }
}

/// Walk `path` through `payload` as JSON dotted accessors.
/// Returns `None` if any segment is missing or traverses into a
/// non-object. Arrays do not get indexed — by contract, array-typed
/// fields are only matched whole-value.
fn lookup_path<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = payload;
    for segment in path.split('.') {
        cur = cur.as_object()?.get(segment)?;
    }
    Some(cur)
}

/// Evaluate a `match_payload` predicate against a trigger event's
/// payload. Returns `true` if every key in `predicate` resolves
/// against `payload` and matches its value:
/// - Scalar RHS  → strict `Value` equality.
/// - Array RHS   → "any of" — payload value must equal one of the
///   array's elements (also strict equality).
///
/// Empty predicate matches everything (caller can shortcut).
pub fn payload_matches(predicate: &serde_json::Map<String, Value>, payload: &Value) -> bool {
    for (key, expected) in predicate {
        let Some(actual) = lookup_path(payload, key) else {
            return false;
        };
        match expected {
            Value::Array(opts) => {
                if !opts.iter().any(|opt| opt == actual) {
                    return false;
                }
            }
            _ => {
                if actual != expected {
                    return false;
                }
            }
        }
    }
    true
}

/// Slug-safe status — replace anything that would be confusing in a
/// topic string with `_`. Topics are `<scope>.<action>`; status
/// becomes part of the action segment.
fn sanitize_status(status: &str) -> String {
    status
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Per-stage payload merging. Carries the parent's static payload +
/// the scan's `status` + the 0-based stage index for downstream
/// consumers that want to render "stage 2 of 3".
fn stage_payload(parent_payload: &Value, status: &str, stage_index: usize) -> Value {
    let mut map = match parent_payload {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    map.insert("status".to_string(), Value::String(status.to_string()));
    map.insert("stage_index".to_string(), Value::Number(stage_index.into()));
    Value::Object(map)
}

/// Single deferred emission scheduled for a future day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedEmission {
    pub counterparty: String,
    pub topic: String,
    pub payload: Value,
}

/// In-memory queue keyed by emit-date. Serializable so the
/// brewery-sim daemon can persist pending settlement emissions
/// across restarts. Without this, every restart drops every
/// queued future emission — AR collections scheduled 30 days out
/// vanish, vendor invoices queued at delay-snap never fire, and
/// the brewery's collected-cash projection collapses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CounterpartyState {
    pub queue: BTreeMap<NaiveDate, Vec<QueuedEmission>>,
    /// Sim-date the checkpoint was taken on. Epoch fingerprint: the
    /// demo rewinds its epoch to day one every few wall-hours, and a
    /// checkpoint stamped in the sim FUTURE was saved by a previous
    /// epoch — its queue references invoices/accounts the epoch reset
    /// wiped, so restoring it emits phantom settlements (the
    /// replay-divergence class, 2026-07-13). `None` = pre-stamp file
    /// format, same verdict. Warp-agnostic on purpose: wall-clock
    /// anchors move on warp changes, sim-dates don't.
    #[serde(default)]
    pub saved_on: Option<NaiveDate>,
}

impl CounterpartyState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule(&mut self, day: NaiveDate, emission: QueuedEmission) {
        self.queue.entry(day).or_default().push(emission);
    }

    /// Drain rows whose date <= `today`, oldest-first.
    pub fn drain_due(&mut self, today: NaiveDate) -> Vec<(NaiveDate, QueuedEmission)> {
        let mut out = Vec::new();
        let due: Vec<NaiveDate> = self.queue.range(..=today).map(|(d, _)| *d).collect();
        for d in due {
            if let Some(rows) = self.queue.remove(&d) {
                for r in rows {
                    out.push((d, r));
                }
            }
        }
        out
    }

    pub fn pending_count(&self) -> usize {
        self.queue.values().map(|v| v.len()).sum()
    }

    /// True when this checkpoint cannot belong to the current epoch:
    /// stamped after `today` (the sim clock rewound beneath it — an
    /// epoch restart), or not stamped at all (pre-stamp format, so its
    /// provenance is unknowable). Callers discard stale checkpoints
    /// instead of restoring them; the queued emissions reference
    /// entities the epoch reset wiped. Residual accepted: a checkpoint
    /// from a previous epoch whose stamp the NEW epoch has already
    /// re-reached (daemon down for hours at high warp) reads as fresh —
    /// bounded, and the emissions 404 against the reset projections.
    pub fn stale_for(&self, today: NaiveDate) -> bool {
        self.saved_on.is_none_or(|d| d > today)
    }

    /// Persist the queue to a JSON file at `path`. Atomic via
    /// write-then-rename so a partial write can't corrupt the
    /// existing snapshot. Used by the brewery-sim daemon to keep
    /// settlement emissions alive across restarts.
    pub fn save_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Load a queue from a JSON file. Missing file returns an
    /// empty state — first boot, after a baseline reset, etc.
    pub fn load_from_file(path: &std::path::Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(path)?;
        let state: Self = serde_json::from_str(&body).map_err(std::io::Error::other)?;
        Ok(state)
    }
}

/// The engine. Holds a flattened lookup of every spec (and follow-up)
/// keyed by the topic it listens on, plus the calendar registry the
/// delay-snap step consults.
pub struct CounterpartyEngine {
    /// Flattened specs, indexed by `listens_to` for O(1) lookup
    /// during the day step. Multiple specs can share a topic — they
    /// all fire.
    specs: Vec<CounterpartySpec>,
    state: CounterpartyState,
    calendars: CalendarRegistry,
    /// Root chain name → real-world actor kind, for cockpit attribution.
    root_actor_kinds: BTreeMap<String, ActorKind>,
}

impl CounterpartyEngine {
    /// Build from a list of root specs. Each spec's followups
    /// recursively flatten into siblings.
    pub fn new(specs: Vec<CounterpartySpec>, calendars: CalendarRegistry) -> Self {
        // Root chain → actor kind, from the `actor_kind` data tag on each
        // `[counterparty.*]`. Followups + scan stages inherit via their
        // qualified name's root segment (`grain-supplier.ap-pay` →
        // `grain-supplier` → Vendor) at drain time.
        let root_actor_kinds = specs
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    ActorKind::from_tag(s.actor_kind.as_deref().unwrap_or("")),
                )
            })
            .collect();
        let flat: Vec<CounterpartySpec> = specs.iter().flat_map(|s| s.flatten(None)).collect();
        Self {
            specs: flat,
            state: CounterpartyState::new(),
            calendars,
            root_actor_kinds,
        }
    }

    /// Snapshot the pending queue for persistence, stamped with the
    /// sim-date of the checkpoint so a later boot can tell whether the
    /// file belongs to the current epoch (see
    /// [`CounterpartyState::stale_for`]).
    pub fn checkpoint(&self, saved_on: NaiveDate) -> CounterpartyState {
        CounterpartyState {
            queue: self.state.queue.clone(),
            saved_on: Some(saved_on),
        }
    }

    /// Pending queue length — handy for tests and debug.
    pub fn pending(&self) -> usize {
        self.state.pending_count()
    }

    /// Borrow the queue state — used by the brewery-sim daemon's
    /// persistence loop to checkpoint to disk between ticks.
    pub fn state(&self) -> &CounterpartyState {
        &self.state
    }

    /// Replace the queue with a restored snapshot. Called once at
    /// daemon boot from the persistent state file; later ticks
    /// extend the queue via the normal step() path.
    pub fn restore_state(&mut self, state: CounterpartyState) {
        self.state = state;
    }

    /// Internal: schedule one emission against `today`'s clock.
    /// `shocks` is the snapshot of active shocks for the day; any
    /// shock targeting this spec by name multiplies the sampled
    /// delay (>1 = slower, <1 = faster). Snapshot is passed in
    /// rather than re-borrowed so step() can hold the spec list
    /// + shock list without conflicting borrows on ctx.state.
    fn schedule(
        &mut self,
        today: NaiveDate,
        spec: &CounterpartySpec,
        trigger: &SimBusEvent,
        rng: &mut crate::rng::Rng,
        shocks: &[crate::shape_driven::state::ActiveShock],
    ) {
        // Decide which branch this trigger takes. A single roll
        // keeps the success / else paths disjoint by construction
        // — splitting across two specs would let independent
        // rolls produce both-or-neither outcomes.
        let topic = if spec.emit_probability >= 1.0 || rng.next() < spec.emit_probability {
            spec.emits.clone()
        } else if let Some(alt) = &spec.emit_else {
            alt.clone()
        } else {
            return;
        };
        let mut delta = spec.delay.sample_days(rng);
        let mult = active_counterparty_delay_multiplier(shocks, &spec.name);
        if mult != 1.0 {
            // Round half-away-from-zero, clamp at zero so a
            // dampening shock can't make delivery time negative.
            delta = ((delta as f64) * mult).round().max(0.0) as i64;
        }
        let cal = self.calendars.get(spec.delay.business_calendar.as_deref());
        let target = cal.add_business_days(today, delta);

        // Merge the static `payload` with the trigger payload so the
        // emission carries both contexts. `trigger` always available
        // under the `trigger` key for downstream specs to chain off.
        let mut payload = match &spec.payload {
            Value::Object(m) => Value::Object(m.clone()),
            _ => Value::Object(serde_json::Map::new()),
        };
        if let Value::Object(m) = &mut payload {
            m.insert("trigger".to_string(), trigger.payload.clone());
        }

        self.state.schedule(
            target,
            QueuedEmission {
                counterparty: spec.name.clone(),
                topic,
                payload,
            },
        );
    }
}

/// Mutate `payload` to include `_day: "YYYY-MM-DD"`. Mirrors the
/// helper in `engines::periodic` — duplicated rather than shared
/// across an `engines::common` module since it's three lines and
/// the engines are otherwise independent.
fn inject_day(payload: &mut Value, day: NaiveDate) {
    let day_str = day.format("%Y-%m-%d").to_string();
    if let Value::Object(map) = payload {
        map.insert("_day".to_string(), Value::String(day_str));
        return;
    }
    let original = std::mem::replace(payload, Value::Null);
    let mut map = serde_json::Map::new();
    map.insert("_day".to_string(), Value::String(day_str));
    if !original.is_null() {
        map.insert("_value".to_string(), original);
    }
    *payload = Value::Object(map);
}

impl SimEngine for CounterpartyEngine {
    fn name(&self) -> &str {
        "counterparty"
    }

    fn step(&mut self, ctx: &mut DayContext<'_>) -> anyhow::Result<()> {
        // 1. Drain rows due today. Emitting onto the bus + output
        //    happens here — followups already live in `self.specs`
        //    and will pick this emission up the same day if their
        //    `listens_to` matches in step 2. Inject the drain date
        //    as `_day` so LiveApi path templates can substitute it
        //    into the route URL (e.g. /api/sweep?as_of={_day}).
        let due = self.state.drain_due(ctx.day);
        for (_due_day, mut e) in due {
            inject_day(&mut e.payload, ctx.day);
            ctx.bus.publish(
                e.topic.clone(),
                format!("counterparty:{}", e.counterparty),
                e.payload.clone(),
            );
            // Attribute the API call to the real-world actor this chain
            // represents: the root segment of the qualified name carries
            // the `actor_kind` tag (followups/scans inherit it).
            let root = e.counterparty.split('.').next().unwrap_or(&e.counterparty);
            let kind = self
                .root_actor_kinds
                .get(root)
                .copied()
                .unwrap_or(ActorKind::Environment);
            ctx.output
                .emit_event(&e.topic, &e.payload, Some((kind, &e.counterparty)))?;
        }

        // 2. Consume today's bus events. For each event, find every
        //    spec listening on that topic and schedule a future
        //    emission. Snapshot the events first so we can roll the
        //    probability + delay without holding the bus borrow.
        //    Active shocks are also snapshotted so the schedule call
        //    can read them without re-borrowing ctx.state.
        let triggers: Vec<SimBusEvent> = ctx.bus.events().to_vec();
        let specs = self.specs.clone();
        let shocks = ctx.state.active_shocks.clone();
        for event in triggers {
            for spec in &specs {
                if spec.listens_to != event.topic {
                    continue;
                }
                if !spec.match_payload.is_empty()
                    && !payload_matches(&spec.match_payload, &event.payload)
                {
                    continue;
                }
                self.schedule(ctx.day, spec, &event, ctx.rng, &shocks);
            }
        }
        Ok(())
    }
}

/// Resolve the delay multiplier for a CounterpartySpec by walking
/// the active shock list. Returns the product of every shock whose
/// target names this counterparty. `1.0` = no active shock.
///
/// Stacks multiplicatively, mirroring the JobRate-shock convention:
/// two concurrent "supplier slow" + "carrier slammed" shocks on the
/// same vendor compound into a 4x delay.
fn active_counterparty_delay_multiplier(
    shocks: &[crate::shape_driven::state::ActiveShock],
    counterparty_name: &str,
) -> f64 {
    let mut mult = 1.0;
    for shock in shocks {
        if shock.target.matches_counterparty(counterparty_name) {
            mult *= shock.rate_multiplier;
        }
    }
    mult
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::SimEventBus;
    use crate::output::InMemoryOutput;
    use crate::rng::Rng;
    use crate::shape_driven::ShapeDrivenState;
    use serde_json::json;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn ach_spec() -> CounterpartySpec {
        CounterpartySpec {
            actor_kind: None,
            name: "bank-ach".into(),
            listens_to: "ledger.payment_instructed".into(),
            delay: DelaySpec {
                mean_days: 1.0,
                spread_days: 0.0,
                business_calendar: Some("us-banking".into()),
            },
            emit_probability: 1.0,
            emits: "ledger.payment_settled".into(),
            payload: json!({"channel": "ach"}),
            followups: vec![],
            scans: vec![],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        }
    }

    fn step_engine(
        engine: &mut CounterpartyEngine,
        day: NaiveDate,
        rng: &mut Rng,
    ) -> (InMemoryOutput, Vec<SimBusEvent>) {
        let mut state = ShapeDrivenState::new();
        let mut bus = SimEventBus::new();
        let mut output = InMemoryOutput::default();
        let mut ctx = DayContext {
            tick: crate::engines::Tick::day(),
            day,
            rng,
            state: &mut state,
            output: &mut output,
            bus: &mut bus,
        };
        engine.step(&mut ctx).unwrap();
        (output, bus.events().to_vec())
    }

    fn step_engine_with_event(
        engine: &mut CounterpartyEngine,
        day: NaiveDate,
        rng: &mut Rng,
        event: SimBusEvent,
    ) -> (InMemoryOutput, Vec<SimBusEvent>) {
        let mut state = ShapeDrivenState::new();
        let mut bus = SimEventBus::new();
        bus.emit(event);
        let mut output = InMemoryOutput::default();
        let mut ctx = DayContext {
            tick: crate::engines::Tick::day(),
            day,
            rng,
            state: &mut state,
            output: &mut output,
            bus: &mut bus,
        };
        engine.step(&mut ctx).unwrap();
        (output, bus.events().to_vec())
    }

    fn step_engine_with_event_and_shocks(
        engine: &mut CounterpartyEngine,
        day: NaiveDate,
        rng: &mut Rng,
        event: SimBusEvent,
        shocks: Vec<crate::shape_driven::state::ActiveShock>,
    ) -> (InMemoryOutput, Vec<SimBusEvent>) {
        let mut state = ShapeDrivenState::new();
        state.active_shocks = shocks;
        let mut bus = SimEventBus::new();
        bus.emit(event);
        let mut output = InMemoryOutput::default();
        let mut ctx = DayContext {
            tick: crate::engines::Tick::day(),
            day,
            rng,
            state: &mut state,
            output: &mut output,
            bus: &mut bus,
        };
        engine.step(&mut ctx).unwrap();
        (output, bus.events().to_vec())
    }

    #[test]
    fn flatten_collects_followups_with_qualified_names() {
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "vendor-x".into(),
            listens_to: "po.sent".into(),
            delay: DelaySpec {
                mean_days: 3.0,
                spread_days: 1.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "vendor.invoice_received".into(),
            payload: Value::Null,
            followups: vec![CounterpartySpec {
                actor_kind: None,
                name: "ack".into(),
                listens_to: "ap.invoice_matched".into(),
                delay: DelaySpec {
                    mean_days: 5.0,
                    spread_days: 0.0,
                    business_calendar: None,
                },
                emit_probability: 1.0,
                emits: "ap.payment_acknowledged".into(),
                payload: Value::Null,
                followups: vec![],
                scans: vec![],
                emit_else: None,
                match_payload: serde_json::Map::new(),
            }],
            scans: vec![],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let flat = spec.flatten(None);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].name, "vendor-x");
        assert_eq!(flat[1].name, "vendor-x.ack");
    }

    #[test]
    fn scans_flatten_into_chained_followups() {
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "fedex".into(),
            listens_to: "shipping.handed_off".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "shipping.tracking".into(),
            payload: json!({"carrier": "fedex"}),
            followups: vec![],
            scans: vec![
                ScanSpec {
                    status: "in-transit".into(),
                    delay: DelaySpec {
                        mean_days: 1.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
                ScanSpec {
                    status: "out-for-delivery".into(),
                    delay: DelaySpec {
                        mean_days: 2.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
                ScanSpec {
                    status: "delivered".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
            ],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let flat = spec.flatten(None);
        assert_eq!(flat.len(), 3, "scans flatten to one row per stage");
        assert_eq!(flat[0].name, "fedex.in-transit");
        assert_eq!(flat[0].listens_to, "shipping.handed_off");
        assert_eq!(flat[0].emits, "shipping.tracking_in_transit");
        assert_eq!(flat[0].payload["status"], "in-transit");
        assert_eq!(flat[0].payload["stage_index"], 0);
        // Stage 1 listens for stage 0's emission.
        assert_eq!(flat[1].listens_to, "shipping.tracking_in_transit");
        assert_eq!(flat[1].emits, "shipping.tracking_out_for_delivery");
        assert_eq!(flat[1].payload["stage_index"], 1);
        // Stage 2 listens for stage 1's emission.
        assert_eq!(flat[2].listens_to, "shipping.tracking_out_for_delivery");
        assert_eq!(flat[2].emits, "shipping.tracking_delivered");
        assert_eq!(flat[2].payload["status"], "delivered");
    }

    #[test]
    #[should_panic(expected = "declares both `followups` and `scans`")]
    fn scans_and_followups_are_mutually_exclusive() {
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "bad".into(),
            listens_to: "x.y".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "z.w".into(),
            payload: Value::Null,
            followups: vec![CounterpartySpec {
                actor_kind: None,
                name: "f".into(),
                listens_to: "z.w".into(),
                delay: DelaySpec {
                    mean_days: 0.0,
                    spread_days: 0.0,
                    business_calendar: None,
                },
                emit_probability: 1.0,
                emits: "f.f".into(),
                payload: Value::Null,
                followups: vec![],
                scans: vec![],
                emit_else: None,
                match_payload: serde_json::Map::new(),
            }],
            scans: vec![ScanSpec {
                status: "stage-1".into(),
                delay: DelaySpec {
                    mean_days: 0.0,
                    spread_days: 0.0,
                    business_calendar: None,
                },
            }],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let _ = spec.flatten(None);
    }

    #[test]
    fn scans_chain_fires_each_stage_in_sequence() {
        // 3-stage carrier chain with 0-day delays so every stage
        // can drain on a single sim day and keep the test compact.
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "fast-carrier".into(),
            listens_to: "shipping.handed_off".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "shipping.tracking".into(),
            payload: json!({"carrier": "fast"}),
            followups: vec![],
            scans: vec![
                ScanSpec {
                    status: "in-transit".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
                ScanSpec {
                    status: "out-for-delivery".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
                ScanSpec {
                    status: "delivered".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
            ],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0x5CA75);

        // Day 0: trigger lands. Stage 0 queues for tomorrow.
        let _ = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new(
                "shipping.handed_off",
                "human-worker",
                json!({"shipment_id": "shp-1"}),
            ),
        );
        // Run subsequent days; each day drains stage N and queues stage N+1.
        let mut all_events: Vec<(String, Value)> = Vec::new();
        for offset in 1..=3 {
            let (out, _bus) = step_engine(
                &mut engine,
                d(2026, 4, 27) + chrono::Duration::days(offset),
                &mut rng,
            );
            all_events.extend(out.events);
        }

        let topics: Vec<&str> = all_events.iter().map(|(t, _)| t.as_str()).collect();
        assert!(topics.contains(&"shipping.tracking_in_transit"));
        assert!(topics.contains(&"shipping.tracking_out_for_delivery"));
        assert!(topics.contains(&"shipping.tracking_delivered"));
        // After all 3 stages drain, the queue is empty.
        assert_eq!(engine.pending(), 0);
    }

    #[test]
    fn matching_event_schedules_emission_after_delay() {
        let mut engine = CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xACE);
        // Trigger on a Monday.
        let monday = d(2026, 4, 27);
        let (_out, _bus) = step_engine_with_event(
            &mut engine,
            monday,
            &mut rng,
            SimBusEvent::new(
                "ledger.payment_instructed",
                "human-worker",
                json!({"invoice_id": "inv-1"}),
            ),
        );
        // 1 row queued for Tuesday (1 business day after Monday).
        assert_eq!(engine.pending(), 1);
        let next_day = engine.state.queue.keys().next().copied().unwrap();
        assert_eq!(next_day, d(2026, 4, 28));
    }

    #[test]
    fn weekend_delay_snaps_forward_to_monday() {
        let mut engine = CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xACE);
        // Trigger on a Friday; 1 business day later = Monday, not Saturday.
        let friday = d(2026, 4, 24);
        let (_o, _b) = step_engine_with_event(
            &mut engine,
            friday,
            &mut rng,
            SimBusEvent::new("ledger.payment_instructed", "human-worker", json!({})),
        );
        let queued = engine.state.queue.keys().next().copied().unwrap();
        assert_eq!(queued, d(2026, 4, 27));
    }

    #[test]
    fn drain_emits_on_target_day_and_clears_queue() {
        let mut engine = CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xACE);
        // Trigger Monday; emission queued Tuesday.
        let _ = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new("ledger.payment_instructed", "human-worker", json!({})),
        );
        // Run the next day with no new triggers.
        let (out, bus) = step_engine(&mut engine, d(2026, 4, 28), &mut rng);
        assert_eq!(engine.pending(), 0, "queue should be empty after drain");
        // emit_event called once with the spec's `emits` topic.
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].0, "ledger.payment_settled");
        // Bus carries the emitted event for downstream specs.
        assert!(bus.iter().any(|e| e.topic == "ledger.payment_settled"));
    }

    #[test]
    fn emit_probability_drops_some_triggers() {
        // emit_probability = 0 means nothing should ever queue.
        let mut spec = ach_spec();
        spec.emit_probability = 0.0;
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xDEAD);
        for _ in 0..50 {
            let _ = step_engine_with_event(
                &mut engine,
                d(2026, 4, 27),
                &mut rng,
                SimBusEvent::new("ledger.payment_instructed", "human-worker", json!({})),
            );
        }
        assert_eq!(
            engine.pending(),
            0,
            "0.0 probability should drop every trigger"
        );
    }

    #[test]
    fn followup_chains_off_first_hop() {
        // PO → vendor invoice (3 days) → AP payment (5 days). When
        // the first hop's emission lands on the bus, the second hop
        // (which listens to that topic) schedules its own emission.
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "vendor".into(),
            listens_to: "po.sent".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "vendor.invoice_received".into(),
            payload: Value::Null,
            followups: vec![CounterpartySpec {
                actor_kind: None,
                name: "ack".into(),
                listens_to: "vendor.invoice_received".into(),
                delay: DelaySpec {
                    mean_days: 0.0,
                    spread_days: 0.0,
                    business_calendar: None,
                },
                emit_probability: 1.0,
                emits: "ap.payment_acknowledged".into(),
                payload: Value::Null,
                followups: vec![],
                scans: vec![],
                emit_else: None,
                match_payload: serde_json::Map::new(),
            }],
            scans: vec![],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xC0FFEE);
        // Monday: trigger.
        let _ = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new("po.sent", "human-worker", json!({})),
        );
        // Same day, run again — the queued vendor.invoice_received
        // emission lands on Mon (delay=0 = next business day),
        // followup queues for the same Monday.
        // Tuesday: drain. The first emission fires, lands on bus,
        // followup picks it up and schedules itself.
        let (out, _bus) = step_engine(&mut engine, d(2026, 4, 27), &mut rng);
        // First emission fires today.
        assert!(
            out.events
                .iter()
                .any(|(t, _)| t == "vendor.invoice_received")
        );
        // Followup queued.
        assert!(engine.pending() >= 1);
        // Wednesday: followup drains.
        let (out2, _bus2) = step_engine(&mut engine, d(2026, 4, 28), &mut rng);
        assert!(
            out2.events
                .iter()
                .any(|(t, _)| t == "ap.payment_acknowledged")
        );
    }

    #[test]
    fn deterministic_run_matches_under_same_seed() {
        let make = || {
            (
                CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests()),
                Rng::new(0xFEED),
            )
        };
        let (mut a, mut rng_a) = make();
        let (mut b, mut rng_b) = make();
        for offset in 0..15 {
            let day = d(2026, 4, 27) + chrono::Duration::days(offset);
            let event = SimBusEvent::new(
                "ledger.payment_instructed",
                "human-worker",
                json!({"i": offset}),
            );
            let (out_a, _) = step_engine_with_event(&mut a, day, &mut rng_a, event.clone());
            let (out_b, _) = step_engine_with_event(&mut b, day, &mut rng_b, event);
            assert_eq!(out_a.events.len(), out_b.events.len());
        }
        assert_eq!(a.pending(), b.pending());
    }

    // -----------------------------------------------------------------
    // match_payload predicate
    // -----------------------------------------------------------------

    fn pred(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        m
    }

    #[test]
    fn payload_matches_empty_predicate_matches_anything() {
        let p = serde_json::Map::new();
        assert!(payload_matches(&p, &json!({"anything": "goes"})));
        assert!(payload_matches(&p, &Value::Null));
    }

    #[test]
    fn payload_matches_flat_key_equality() {
        let p = pred(&[("kind", json!("procurement"))]);
        assert!(payload_matches(&p, &json!({"kind": "procurement"})));
        assert!(!payload_matches(&p, &json!({"kind": "shipment"})));
        assert!(!payload_matches(&p, &json!({})));
    }

    #[test]
    fn payload_matches_dotted_path() {
        let p = pred(&[("metadata.vendor.category", json!("dairy"))]);
        let yes = json!({"metadata": {"vendor": {"category": "dairy"}}});
        let no_value = json!({"metadata": {"vendor": {"category": "flour-mill"}}});
        let no_path = json!({"metadata": {"vendor": {}}});
        assert!(payload_matches(&p, &yes));
        assert!(!payload_matches(&p, &no_value));
        assert!(!payload_matches(&p, &no_path));
    }

    #[test]
    fn payload_matches_array_rhs_means_any_of() {
        let p = pred(&[("metadata.vendor.category", json!(["dairy", "produce"]))]);
        let dairy = json!({"metadata": {"vendor": {"category": "dairy"}}});
        let produce = json!({"metadata": {"vendor": {"category": "produce"}}});
        let flour = json!({"metadata": {"vendor": {"category": "flour-mill"}}});
        assert!(payload_matches(&p, &dairy));
        assert!(payload_matches(&p, &produce));
        assert!(!payload_matches(&p, &flour));
    }

    #[test]
    fn payload_matches_traverses_into_arrays_returns_false() {
        // By contract, arrays aren't indexed; payload-side arrays
        // can only be matched whole-value (not what this predicate
        // is doing).
        let p = pred(&[("items.0.sku", json!("flour-50lb"))]);
        let pl = json!({"items": [{"sku": "flour-50lb"}]});
        assert!(!payload_matches(&p, &pl));
    }

    #[test]
    fn payload_matches_all_keys_must_match() {
        let p = pred(&[
            ("kind", json!("procurement")),
            ("metadata.vendor.category", json!("dairy")),
        ]);
        let both = json!({"kind": "procurement",
                          "metadata": {"vendor": {"category": "dairy"}}});
        let only_kind = json!({"kind": "procurement",
                               "metadata": {"vendor": {"category": "flour"}}});
        assert!(payload_matches(&p, &both));
        assert!(!payload_matches(&p, &only_kind));
    }

    #[test]
    fn engine_step_routes_event_to_only_matching_spec() {
        // Two specs on the same topic, different predicates. The
        // event payload matches dairy's predicate but not flour's,
        // so only dairy queues an emission. We assert via the
        // pending queue (not output) because step() drains BEFORE
        // it consumes new events; same-day-due rows wait for the
        // next step to drain.
        let mk = |name: &str, category: &str, emits: &str| CounterpartySpec {
            actor_kind: None,
            name: name.into(),
            listens_to: "step.done.procurement".into(),
            delay: DelaySpec {
                mean_days: 1.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: emits.into(),
            payload: Value::Null,
            followups: vec![],
            scans: vec![],
            emit_else: None,
            match_payload: pred(&[("metadata.vendor.category", json!(category))]),
        };
        let mut engine = CounterpartyEngine::new(
            vec![
                mk("flour", "flour-mill", "ap.invoice.flour"),
                mk("dairy", "dairy", "ap.invoice.dairy"),
            ],
            CalendarRegistry::for_tests(),
        );
        let mut rng = Rng::new(0x77);
        let _ = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new(
                "step.done.procurement",
                "human-worker",
                json!({"kind": "procurement",
                       "metadata": {"vendor": {"category": "dairy"}}}),
            ),
        );
        assert_eq!(engine.pending(), 1, "only the matching spec queues");
        // Drain the next day to confirm the matching spec emits.
        let (out, _) = step_engine(&mut engine, d(2026, 4, 28), &mut rng);
        let topics: Vec<&str> = out.events.iter().map(|(t, _)| t.as_str()).collect();
        assert!(topics.contains(&"ap.invoice.dairy"));
        assert!(!topics.contains(&"ap.invoice.flour"));
        assert_eq!(engine.pending(), 0);
    }

    #[test]
    fn engine_step_no_predicate_matches_every_event() {
        // Empty match_payload preserves the pre-filter behavior:
        // every event on the topic schedules an emission.
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "open-handler".into(),
            listens_to: "step.done.procurement".into(),
            delay: DelaySpec {
                mean_days: 1.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "downstream".into(),
            payload: Value::Null,
            followups: vec![],
            scans: vec![],
            emit_else: None,
            match_payload: serde_json::Map::new(),
        };
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0x88);
        let _ = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new(
                "step.done.procurement",
                "human-worker",
                json!({"kind": "procurement",
                       "metadata": {"vendor": {"category": "anything"}}}),
            ),
        );
        assert_eq!(engine.pending(), 1, "empty predicate matches the event");
    }

    #[test]
    fn engine_step_predicate_miss_schedules_nothing() {
        // The predicate doesn't match → no scheduling, no draining.
        let spec = CounterpartySpec {
            actor_kind: None,
            name: "dairy".into(),
            listens_to: "step.done.procurement".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "ap.invoice.dairy".into(),
            payload: Value::Null,
            followups: vec![],
            scans: vec![],
            emit_else: None,
            match_payload: pred(&[("metadata.vendor.category", json!("dairy"))]),
        };
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0x99);
        let (out, _) = step_engine_with_event(
            &mut engine,
            d(2026, 4, 27),
            &mut rng,
            SimBusEvent::new(
                "step.done.procurement",
                "human-worker",
                json!({"kind": "procurement",
                       "metadata": {"vendor": {"category": "flour-mill"}}}),
            ),
        );
        assert!(out.events.is_empty(), "predicate-miss event must not fire");
        assert_eq!(engine.pending(), 0, "predicate-miss must not queue either");
    }

    #[test]
    fn flatten_propagates_match_payload_to_root_only() {
        // The root spec keeps its predicate; followups inherit an
        // empty predicate (children listen post-filter, no need).
        // Stage-0 of a `scans` chain inherits the parent predicate
        // so the whole chain gates correctly.
        let spec_with_followup = CounterpartySpec {
            actor_kind: None,
            name: "vendor".into(),
            listens_to: "step.done.procurement".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "ap.invoice".into(),
            payload: Value::Null,
            followups: vec![CounterpartySpec {
                actor_kind: None,
                name: "ack".into(),
                listens_to: "ap.invoice".into(),
                delay: DelaySpec {
                    mean_days: 0.0,
                    spread_days: 0.0,
                    business_calendar: None,
                },
                emit_probability: 1.0,
                emits: "ap.ack".into(),
                payload: Value::Null,
                followups: vec![],
                scans: vec![],
                emit_else: None,
                match_payload: serde_json::Map::new(),
            }],
            scans: vec![],
            emit_else: None,
            match_payload: pred(&[("metadata.vendor.category", json!("dairy"))]),
        };
        let flat = spec_with_followup.flatten(None);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].name, "vendor");
        assert!(
            !flat[0].match_payload.is_empty(),
            "root carries the predicate"
        );
        assert_eq!(flat[1].name, "vendor.ack");
        assert!(
            flat[1].match_payload.is_empty(),
            "follow-up children carry an empty predicate"
        );

        // scans path: stage 0 inherits, later stages don't.
        let spec_with_scans = CounterpartySpec {
            actor_kind: None,
            name: "carrier".into(),
            listens_to: "step.done.shipment".into(),
            delay: DelaySpec {
                mean_days: 0.0,
                spread_days: 0.0,
                business_calendar: None,
            },
            emit_probability: 1.0,
            emits: "shipping.scan".into(),
            payload: Value::Null,
            followups: vec![],
            scans: vec![
                ScanSpec {
                    status: "in-transit".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
                ScanSpec {
                    status: "delivered".into(),
                    delay: DelaySpec {
                        mean_days: 0.0,
                        spread_days: 0.0,
                        business_calendar: None,
                    },
                },
            ],
            emit_else: None,
            match_payload: pred(&[("metadata.carrier", json!("fedex"))]),
        };
        let flat_scan = spec_with_scans.flatten(None);
        assert_eq!(flat_scan.len(), 2);
        assert!(
            !flat_scan[0].match_payload.is_empty(),
            "stage 0 inherits the parent predicate"
        );
        assert!(
            flat_scan[1].match_payload.is_empty(),
            "stage 1+ listens post-filter; carries empty predicate"
        );
    }

    fn shock_for_counterparty(name: &str, mult: f64) -> crate::shape_driven::state::ActiveShock {
        crate::shape_driven::state::ActiveShock {
            name: format!("test-{name}-shock"),
            target: crate::shape_driven::tenant::ShockTarget {
                kind: None,
                subject_id: None,
                subject_id_pattern: None,
                counterparty: Some(name.into()),
            },
            rate_multiplier: mult,
            end_date: NaiveDate::from_ymd_opt(2099, 12, 31).unwrap(),
        }
    }

    /// Without a shock, ach_spec emits one business day after the
    /// trigger. With a 3x delay shock targeting the spec by name,
    /// the same trigger emits ~3 business days later.
    #[test]
    fn counterparty_shock_multiplies_sampled_delay() {
        let _engine = CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xc0ffeeu32);
        let day = d(2026, 5, 4); // Mon
        let trigger = SimBusEvent::new(
            "ledger.payment_instructed".to_string(),
            "test".to_string(),
            json!({"amount": 100}),
        );
        // Use a non-banking calendar to avoid weekend snapping noise.
        let mut spec = ach_spec();
        spec.delay.business_calendar = None;
        spec.delay.mean_days = 2.0; // base 2 days
        let mut engine_no_cal = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());

        // No shock: schedules out 2 days.
        let _ = step_engine_with_event(&mut engine_no_cal, day, &mut rng, trigger.clone());
        assert_eq!(engine_no_cal.pending(), 1);
        // Drain at day+2 should release it.
        let _ = step_engine(
            &mut engine_no_cal,
            day + chrono::Duration::days(2),
            &mut rng,
        );
        assert_eq!(engine_no_cal.pending(), 0);

        // With a 3x shock: same base delay + same RNG seed, scheduled
        // 6 days out instead of 2.
        let mut spec2 = ach_spec();
        spec2.delay.business_calendar = None;
        spec2.delay.mean_days = 2.0;
        let mut engine_shocked =
            CounterpartyEngine::new(vec![spec2], CalendarRegistry::for_tests());
        let mut rng2 = Rng::new(0xc0ffeeu32);
        let _ = step_engine_with_event_and_shocks(
            &mut engine_shocked,
            day,
            &mut rng2,
            trigger.clone(),
            vec![shock_for_counterparty("bank-ach", 3.0)],
        );
        assert_eq!(engine_shocked.pending(), 1);
        // Should still be pending at day+5, drain at day+6.
        let _ = step_engine(
            &mut engine_shocked,
            day + chrono::Duration::days(5),
            &mut rng2,
        );
        assert_eq!(
            engine_shocked.pending(),
            1,
            "3x delay still pending at day+5"
        );
        let _ = step_engine(
            &mut engine_shocked,
            day + chrono::Duration::days(6),
            &mut rng2,
        );
        assert_eq!(engine_shocked.pending(), 0, "3x delay drains at day+6");
    }

    // --- checkpoint epoch-staleness ------------------------------------
    //
    // The demo rewinds its epoch every ~9 wall-hours (warp 1000): the DB
    // resets to baseline but the daemon's on-disk checkpoint used to
    // survive and re-import the PREVIOUS epoch's pending settlements —
    // emissions against invoices the reset wiped (the phantom-account /
    // replay-divergence class, 2026-07-13). A checkpoint therefore
    // carries the sim-date it was saved on; a stamp in the sim FUTURE
    // (or missing — pre-stamp format) marks it as another epoch's.

    #[test]
    fn checkpoint_stamps_saved_on_and_roundtrips() {
        let mut engine = CounterpartyEngine::new(vec![ach_spec()], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0x5eedu32);
        let day = d(2026, 5, 4);
        let trigger = SimBusEvent::new(
            "ledger.payment_instructed".to_string(),
            "test".to_string(),
            json!({"amount": 100}),
        );
        let _ = step_engine_with_event(&mut engine, day, &mut rng, trigger);
        assert_eq!(engine.pending(), 1);

        let snapshot = engine.checkpoint(day);
        assert_eq!(snapshot.saved_on, Some(day));
        assert_eq!(snapshot.pending_count(), 1);

        let dir = boss_testing::scratch_dir("cp-stamp");
        let path = dir.join("queue.json");
        snapshot.save_to_file(&path).unwrap();
        let loaded = CounterpartyState::load_from_file(&path).unwrap();
        assert_eq!(loaded.saved_on, Some(day));
        assert_eq!(loaded.pending_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_from_the_sim_future_is_stale() {
        // Saved late in the previous epoch; the new epoch rewound the
        // clock beneath it.
        let mut st = CounterpartyState::new();
        st.saved_on = Some(d(2026, 1, 15));
        assert!(st.stale_for(d(2025, 3, 31)));
    }

    #[test]
    fn checkpoint_saved_today_or_earlier_is_fresh() {
        // Mid-epoch daemon restart (deploy): the checkpoint predates or
        // equals the clock — keep it.
        let mut st = CounterpartyState::new();
        st.saved_on = Some(d(2025, 6, 1));
        assert!(!st.stale_for(d(2025, 6, 1)));
        assert!(!st.stale_for(d(2025, 6, 2)));
    }

    #[test]
    fn unstamped_checkpoint_is_stale() {
        // Pre-stamp file format: provenance unknown → treat as another
        // epoch's rather than risk phantom settlements.
        let st = CounterpartyState::new();
        assert_eq!(st.saved_on, None);
        assert!(st.stale_for(d(2025, 6, 1)));
    }

    /// A shock targeting a different counterparty leaves this spec's
    /// delay untouched.
    #[test]
    fn counterparty_shock_targeting_different_name_is_inert() {
        let mut spec = ach_spec();
        spec.delay.business_calendar = None;
        spec.delay.mean_days = 2.0;
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xb0a7u32);
        let trigger = SimBusEvent::new(
            "ledger.payment_instructed".to_string(),
            "test".to_string(),
            json!({}),
        );
        let day = d(2026, 5, 4);
        let _ = step_engine_with_event_and_shocks(
            &mut engine,
            day,
            &mut rng,
            trigger,
            vec![shock_for_counterparty("vendor-x", 5.0)],
        );
        // Untouched: drain at day+2.
        let _ = step_engine(&mut engine, day + chrono::Duration::days(2), &mut rng);
        assert_eq!(engine.pending(), 0);
    }

    /// Two stacking shocks on the same counterparty multiply, matching
    /// the JobRate-shock convention where two viral-bar shocks compound.
    #[test]
    fn counterparty_shocks_stack_multiplicatively() {
        let mut spec = ach_spec();
        spec.delay.business_calendar = None;
        spec.delay.mean_days = 2.0;
        let mut engine = CounterpartyEngine::new(vec![spec], CalendarRegistry::for_tests());
        let mut rng = Rng::new(0xfacadeu32);
        let trigger = SimBusEvent::new(
            "ledger.payment_instructed".to_string(),
            "test".to_string(),
            json!({}),
        );
        let day = d(2026, 5, 4);
        // 2x * 3x = 6x → 12 days.
        let _ = step_engine_with_event_and_shocks(
            &mut engine,
            day,
            &mut rng,
            trigger,
            vec![
                shock_for_counterparty("bank-ach", 2.0),
                shock_for_counterparty("bank-ach", 3.0),
            ],
        );
        let _ = step_engine(&mut engine, day + chrono::Duration::days(11), &mut rng);
        assert_eq!(engine.pending(), 1, "compound 6x still pending at day+11");
        let _ = step_engine(&mut engine, day + chrono::Duration::days(12), &mut rng);
        assert_eq!(engine.pending(), 0, "compound 6x drains at day+12");
    }
}
