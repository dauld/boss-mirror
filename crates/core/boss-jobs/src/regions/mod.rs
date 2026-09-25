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
//! marshalling — each with a `count` (what is here, in a stated unit), a
//! `state` (clear / attention / troubled, each non-clear one naming the
//! declared band that decided it and how long it has held —
//! [`crate::region_states`], design 62de32ae), a `kpi` and a `trend`
//! (this window versus the previous).
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
//!
//! ONE FILE PER SEAM (backlog 07addeed, 2026-09-24). This module was one
//! 8,132-line file, and the file builder runs read most (182 reads on
//! 2026-09-24), so each read paid for all of it to see one region. It is
//! split along the seams it already had — pure moves, the same public
//! paths: everything a caller named as `crate::regions::X` is still
//! re-exported here as `X`.
//!
//! - this file — the answer's shape ([`Regions`], [`Region`],
//!   [`Machine`], [`Measure`]), what is read ([`RegionInputs`]), and
//!   [`regions`] itself, which asks one judge per region for its card;
//! - `judging` — stamps, windows, trends and the card every judge
//!   returns ([`region`], [`unread_settled`]): the shared judging;
//! - `partition` — every packet a region counts, it counts alone;
//! - one judge per region, in map order: `dock`, `gates`, `track`,
//!   `shed` (and `stale_proof`, whose wait a stale car is), `arrivals`,
//!   `garage`, `receiving`, `marshalling`, `shop_floor`, `publish`;
//! - `machinery` — the machines standing in each region, and the plant;
//! - `stuck` — what is stuck, per third.
//!
//! Each file's tests sit beside its code; the rows they build a yard
//! from are `fixtures`.

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::region_states::{self as bands, Finding, Settled, settle};
use crate::registry::WorkflowSpec;
use crate::yard::{ConductorHealth, Reading, YardStatus};

mod arrivals;
mod dock;
#[cfg(test)]
mod fixtures;
mod garage;
mod gates;
mod judging;
mod machinery;
mod marshalling;
mod partition;
mod publish;
mod receiving;
mod shed;
mod shop_floor;
mod stale_proof;
mod stuck;
mod track;

use self::arrivals::arrivals;
pub(crate) use self::arrivals::released_awaiting_repair;
use self::dock::dock;
pub(crate) use self::dock::dock_edges;
use self::garage::garage;
use self::gates::gates;
pub(crate) use self::judging::{
    Instant, Windows, closed_at, count_split, find_step, meta_instant, opened_at, parse_instant,
    plural, rate_trend, step_done_at,
};
use self::judging::{
    duration_trend, md_str, onset_of_count, region, split, stamp_instant, unread_settled,
};
pub use self::machinery::{
    CREW_IDLE_HOURS, OPS_RUNNER_ROLE, RunnerHost, SESSION_KIND, runner_hosts_of, runner_verbs,
};
use self::machinery::{host_runner_machines, machines_of, unjudged};
use self::marshalling::marshalling;
pub use self::partition::{Ids, TRIGGER_STEP_KIND, members, taken_in};
pub(crate) use self::partition::{marshalling_view, receiving_standing, taken_in_at};
use self::publish::publish;
pub use self::publish::{PUBLISH_KIND, PublishPr, STALLED_PUBLISH_HOURS, publish_prs};
use self::receiving::receiving;
pub use self::receiving::{AGING_DAYS, PIPELINE_KINDS, STALE_DAYS, inbound_kinds};
pub(crate) use self::shed::awaiting_proof;
use self::shed::shed;
pub use self::shed::{SHED_PLACES, ShedPlace, shed_place, shed_station};
pub use self::shop_floor::run_capacity;
use self::shop_floor::shop_floor;
pub use self::stale_proof::{NOT_YET_STARVED_HOURS, PROOF_STALE_HOURS};
pub(crate) use self::stale_proof::{StaleProof, stale_proof};
pub(crate) use self::stuck::stuck_ids;
pub use self::stuck::{THIRDS, ThirdStuck, stuck};
use self::track::track;
pub use self::track::{TRAIN_GATE_FALLBACK, TRAIN_GATE_WAIT_REASON, train_gate_troubled};

/// The ten regions, in map order. The count and the order are the
/// decision (0524fc95 Q2); a reader that finds an eleventh name has an
/// older or newer server than it expects. `shop-floor` is the ninth
/// (backlog 94c6ffd0): the region UPSTREAM of the dock, where a car is
/// still being built. `publish` is the tenth (design cb38d806, backlog
/// eee42416): the crossing OUT of the world, where what landed on main
/// is proposed to the public GitHub mirror. Both were appended rather
/// than inserted, so the names a client already knows keep their place.
pub const REGIONS: [&str; 10] = [
    "dock",
    "gates",
    "track",
    "shed",
    "arrivals",
    "garage",
    "receiving",
    "marshalling",
    "shop-floor",
    "publish",
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

/// What a region's card — or a border's rail — says at a glance: ONE
/// vocabulary everywhere (design 62de32ae, decision 1; the meanings and
/// the bands that decide them are [`crate::region_states`]).
///
/// `Clear` is flowing within its declared bounds, which INCLUDES busy
/// and healthy — a dock with a train due. `Attention` is a declared band
/// crossed. `Troubled` is ours and not moving, or a reading that could
/// not be taken, which is refused like a failure rather than drawn as
/// clear.
///
/// `busy` was the middle word until 2026-09-24, and it meant three
/// things at once — a train due, bays at their bound, and a week-old
/// intake backlog — so six of ten regions wore it and the colour said
/// nothing. It is gone rather than aliased: a reader that still expects
/// it fails its parse loudly (the map's `parseRegion` throws on an
/// unknown state) instead of drawing a new meaning under an old word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionState {
    Clear,
    Attention,
    Troubled,
}

/// What a region's bound IS (design 62de32ae, decision 5): the most a
/// place can hold (`capacity` — three gate bays, one track, the run
/// cap), or the depth at which something is due (`threshold` — the
/// dock's boarding depth). The review read "DOCK 6 / 1" as six cars in
/// a space for one; the same `n / bound` meant capacity on the gates
/// and a trigger on the dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoundKind {
    Capacity,
    Threshold,
}

/// ONE NUMBER OF A REGION'S KPI, with its unit (design 62de32ae,
/// decisions 5 and 9). `value` is `None` where it could not be measured
/// — never zero for "no reading" — and `text` is the sentence the map
/// and `boss orient` both print, written here so the two say it with
/// one voice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    /// What is measured: `oldest untriaged`, `cars that cannot board`.
    pub name: String,
    pub value: Option<f64>,
    /// `days`, `minutes`, `cars`, `trains`, `bays`, `runs`…
    pub unit: String,
    /// The measurement as a phrase: "oldest untriaged 7 days".
    pub text: String,
}

/// A measure whose text is `<name> <value> <unit>`, or `<name>: no
/// reading` when it could not be taken.
fn measure(name: &str, value: Option<f64>, unit: &str) -> Measure {
    let text = match value {
        Some(v) => format!("{name} {} {unit}", number_text(v)),
        None => format!("{name}: no reading"),
    };
    Measure {
        name: name.to_string(),
        value,
        unit: unit.to_string(),
        text,
    }
}

/// A measure whose sentence the region writes itself, because the
/// number reads better inside it ("3 of 3 bays in use").
fn measure_said(name: &str, value: Option<f64>, unit: &str, text: String) -> Measure {
    Measure {
        name: name.to_string(),
        value,
        unit: unit.to_string(),
        text,
    }
}

/// A count as a measure value.
#[allow(clippy::cast_precision_loss)]
fn count_value(n: usize) -> Option<f64> {
    Some(n as f64)
}

/// A number as a sentence prints it: whole where it is whole, one
/// decimal otherwise.
fn number_text(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r:.1}")
    }
}

/// WHAT A MACHINE IS DOING, in the record — a CLOSED set, so a glyph
/// exists for every value and no reading falls through to a default
/// that reads as calm (design d2154293, car 5).
///
/// `Unknown` is the load-bearing variant. A machine whose state cannot
/// be determined is NOT idle: idle is a reading ("it is here and it has
/// no work"), and it can only be taken where presence is observable
/// independently of work. The precedent is car 2's — silence is judged
/// ONLY where a machine declares a heartbeat, and everywhere else the
/// answer is "cannot tell", never "false". A confident idle drawn for
/// an unmeasurable machine is the false-empty class at its most
/// visible: the map would say the machinery is fine when nothing asked
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineState {
    /// Observed doing work right now.
    Running,
    /// Observed present, with no work in it. Only ever stated where
    /// presence is a fact this process holds — a bay the policy
    /// declares and this process counts, a station row in the registry,
    /// a runner whose last request it answered.
    Idle,
    /// A named failure. A failed machine troubles its territory.
    Failed,
    /// Nothing in the record says which. Drawn distinctly from idle.
    Unknown,
}

/// One machine standing in a region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// Stable within the region, so a glyph keeps its place between
    /// reads: `gate-bay-1`, `conductor`, `station:design-review`.
    pub id: String,
    /// What it is, short enough to ride a glyph.
    pub name: String,
    pub state: MachineState,
    /// What the state was read from. A verdict must name what failed
    /// (CLAUDE.md §Diagnosis), and an `Unknown` names what was missing.
    pub why: String,
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
    /// What the bound is — a capacity or a threshold. Present exactly
    /// when `bound` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_kind: Option<BoundKind>,
    /// What the count counts, in words: `cars parked`, `bays in use`.
    /// A number without its unit is how "35 arrivals" (trains) came to
    /// sit beside "17 landed" (cars) with nothing saying which was which.
    /// Empty on an older payload.
    #[serde(default)]
    pub unit: String,
    pub state: RegionState,
    /// One sentence naming why the state is what it is. A verdict must
    /// name what failed (CLAUDE.md §Diagnosis).
    pub why: String,
    /// THE BAND THAT DECIDED A NON-CLEAR STATE (design 62de32ae,
    /// decisions 1 and 2): its id, the reading against it ("oldest 7d >
    /// the 3-day triage band"), the period it had to hold, and how long
    /// the record says it has held — "troubled for 6m". Absent when the
    /// region is clear, and on an older payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<crate::region_states::Decided>,
    pub trend: Trend,
    /// THE REGION'S KPI (design 62de32ae, decision 9): one or two
    /// measures, each with its unit, primary first. Empty on an older
    /// payload.
    #[serde(default)]
    pub kpi: Vec<Measure>,
    /// The machinery standing in this region — the gate bays, the
    /// conductor, the stations, the runners — each judged HERE, on the
    /// server, so one definition answers the map, the floor and the
    /// CLI. Empty for a region no machine of ours works in; absent on
    /// an older payload, which a client reads as empty.
    #[serde(default)]
    pub machines: Vec<Machine>,
    /// THE PLACES INSIDE THE REGION, each with how many of the region's
    /// OWN members stand there (design 62de32ae, the rest of decision 5;
    /// car E on backlog c3105b2a): marshalling's stations and the shed's
    /// three places. It is what lets an interior draw the head's count
    /// rather than a count of its own — on 2026-09-24 marshalling's
    /// platforms drew every station's full depth, 562 standings under a
    /// head of 236, because the station reads carry no partition and
    /// the only copy of it was here. A marshalling packet standing at two
    /// stations is counted at both, so the places may sum past the head
    /// by exactly the packets that stand twice; the shed's places are
    /// disjoint and sum to it. Empty for a region with no such places,
    /// and on an older payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<Place>,
}

/// One place inside a region and the region's members standing there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The place as its region's interior names it: a station's registry
    /// name, or the shed's `inspection-shed` / `siding-event` /
    /// `siding-no-probe` — the floor's own station names
    /// (`apps/web/src/it/yard/yard-shed.ts`).
    pub name: String,
    pub count: usize,
}

/// The whole map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regions {
    pub window_hours: i64,
    pub regions: Vec<Region>,
    /// WHAT IS STUCK, per third of the operator surface, in [`THIRDS`]
    /// order — [`stuck`]'s one definition, which the HUD and `boss
    /// orient` read rather than recompute (design cf820810 Q6). Absent on
    /// an older payload, which a reader takes as not yet answered.
    #[serde(default)]
    pub stuck: Vec<ThirdStuck>,
    /// THE HUD'S ROWS (design 00774ca8, decisions 1, 2 and 9): per third,
    /// in [`THIRDS`] order, its regions, its balance and its stuck
    /// reading — [`crate::thirds::thirds`]'s one definition, which the
    /// HUD frame and `boss orient` both print. Absent on an older
    /// payload, which a reader takes as not yet answered.
    #[serde(default)]
    pub thirds: Vec<crate::thirds::Third>,
    /// THE WHOLE SYSTEM'S MACHINES, counted once by state (decision 3) —
    /// the HUD's one machine cell. `None` on an older payload: absent is
    /// "not answered", never "no machines".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machines: Option<crate::thirds::MachineSummary>,
    /// THE PLANT (design 62de32ae, decision 11): machinery that serves
    /// every region and so belongs to none — the ops runner on each host
    /// the estate registry declares one on. They stood in receiving until
    /// car E, filed there because an ops-request is an inbound kind, and
    /// the review of 2026-09-24 read that as arbitrary: neither host
    /// runner moves a packet into receiving. The map draws them as a
    /// strip along its edge. A failed one troubles NO region — blaming
    /// the one it happened to be filed under is the mis-grouping this
    /// moves away from — so its failure is carried here, on the strip and
    /// in `boss orient`'s plant line — and it is counted in [`Self::machines`]
    /// under the region `plant`. Absent on an older payload.
    #[serde(default)]
    pub plant: Vec<Machine>,
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
    /// When each member opened, where the record holds it — the
    /// marshalling KPI's age (design 62de32ae, decision 9). Keyed by
    /// member rather than reduced to one oldest instant, because the
    /// partition ([`marshalling_view`]) narrows `members` to what is
    /// marshalling's, and the oldest of what REMAINS is the number.
    pub opened: std::collections::BTreeMap<String, Instant>,
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
    /// two windows, each with its steps. On an OPEN row they say whether
    /// the packet has been taken in ([`taken_in`]), which is where
    /// receiving ends and marshalling begins (design 62de32ae decision
    /// 4); on every row they say WHEN ([`taken_in_at`]), which is what
    /// the border between the two counts. `None` when the workflow
    /// registry that says which kinds are inbound could not be read.
    pub inbound: Option<&'a [(Job, Vec<Step>)]>,
    /// Every station but the dock. `None` when the station registry
    /// could not be read.
    pub stations: Option<&'a [StationReading]>,
    /// The conductor's own firing record, as `http/yard.rs` already
    /// built it for the yard status. `None` when this caller did not
    /// read it — which the conductor machine states as unknown, never
    /// as a healthy tick.
    pub conductor: Option<&'a ConductorHealth>,
    /// The newest ops-request of each verb [`runner_verbs`] names AND
    /// of each host [`runner_hosts`] names, with its steps — the
    /// evidence a runner machine is read from. `None` when the
    /// ops-request rows could not be read at all.
    pub ops_requests: Option<&'a [(Job, Vec<Step>)]>,
    /// THE SHOP FLOOR'S RUNS (backlog 94c6ffd0): `agent-run` packets
    /// open, plus those closed within two windows. Steps ride the OPEN
    /// ones only — a finished-but-unreported run is a `building` done
    /// over a `reported` still open — while the trend reads its two
    /// instants off the metadata. `None` when the read failed, which
    /// is a troubled floor and never a quiet one.
    pub agent_runs: Option<&'a [(Job, Vec<Step>)]>,
    /// The crews: open [`SESSION_KIND`] packets, rows only. `None` on a
    /// failed read — one unknown machine, never a floor with nobody
    /// standing on it.
    pub sessions: Option<&'a [Job]>,
    /// THE PUBLISH REGION'S PACKETS (design cb38d806): the newest
    /// [`PUBLISH_KIND`] packets, open and closed, with their steps. NOT
    /// windowed — a pull request nobody merged is exactly the thing
    /// this region exists to show, and it outlives every window; the
    /// handler caps the page instead. `None` on a failed read, which is
    /// a troubled region and never a mirror that reads as current.
    pub publish_packets: Option<&'a [(Job, Vec<Step>)]>,
    /// [`run_capacity`] over the agents registry, or `None` where no
    /// bound is declared or the registry could not be read.
    pub run_capacity: Option<usize>,
    /// The hosts the ESTATE REGISTRY says should be answering
    /// ops-requests — [`runner_hosts_of`] over `/api/estate/nodes`.
    /// `None` when the registry could not be read, which is one
    /// unknown machine and never an estate with no runners in it.
    pub runner_hosts: Option<&'a [RunnerHost]>,
    /// THE DOCK'S ORDERING EDGES (backlog 4142d821, design cf820810 Q7):
    /// the reading of every predecessor a parked car declares
    /// ([`crate::car::BOARDS_AFTER`]), keyed by the id it declares. Read
    /// BY ID, not out of `cars`, because a predecessor that landed a week
    /// ago is outside every window and still decides whether its
    /// successor boards. [`declared_edges`] names the ids to read, so the
    /// handler and the dock cannot disagree about the set; a declared id
    /// with no reading here is judged unreadable, never satisfied.
    pub predecessors: &'a [(String, crate::car::Predecessor)],
    pub now: chrono::DateTime<chrono::Utc>,
    pub window_hours: i64,
}

/// The predecessors the dock's parked cars declare, deduplicated, in dock
/// order — the ids the handler reads for [`RegionInputs::predecessors`].
/// A dock row whose car is not among `cars` declares nothing this pass
/// can see, the same as a car with no edge.
pub fn declared_edges(status: &YardStatus, cars: &[(Job, Vec<Step>)]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in status
        .dock
        .iter()
        .filter_map(|d| cars.iter().find(|(j, _)| j.id.to_string() == d.id))
        .filter_map(|(j, _)| crate::car::boards_after_of(&j.metadata))
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// A packet as the jobs API serves it — the row with its `steps` — which
/// is the shape `crate::car`'s predicates read, so a predecessor judged
/// here is judged exactly as the conductor judges the one it fetched.
pub fn packet_value(job: &Job, steps: &[Step]) -> Value {
    let mut v = serde_json::to_value(job).unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "steps".to_string(),
            serde_json::to_value(steps).unwrap_or_default(),
        );
    }
    v
}

/// The map. Pure: rows in, one card per [`REGIONS`] name out, in that
/// order.
pub fn regions(inputs: &RegionInputs<'_>) -> Regions {
    let w = Windows::of(inputs.now, inputs.window_hours);
    let regions = [
        dock(inputs, &w),
        gates(inputs, &w),
        track(inputs, &w),
        shed(inputs, &w),
        arrivals(inputs, &w),
        garage(inputs, &w),
        receiving(inputs, &w),
        marshalling(inputs, &w),
        shop_floor(inputs, &w),
        publish(inputs, &w),
    ]
    .into_iter()
    .map(|r| {
        let machines = machines_of(&r.name, inputs);
        with_machinery(r, machines, inputs.now)
    })
    .collect::<Vec<Region>>();
    let stuck = stuck(inputs);
    let plant = host_runner_machines(inputs);
    Regions {
        window_hours: inputs.window_hours,
        thirds: crate::thirds::thirds(inputs, &stuck),
        // The HUD's machine cell counts the plant too: the host runners
        // left the regions for the plant (decision 11), and a count over
        // the regions alone would drop them.
        machines: Some(crate::thirds::machine_summary(&regions, &plant)),
        regions,
        stuck,
        plant,
    }
}

/// Hang a region's machinery on it — and let a FAILED machine trouble
/// its territory, so the failure reads at world scale rather than only
/// when someone zooms in. The machine's own sentence leads the `why`:
/// a verdict must name what failed, and the region's existing reason
/// is kept behind it rather than overwritten.
fn with_machinery(region: Region, machines: Vec<Machine>, now: Instant) -> Region {
    let failed = machines
        .iter()
        .find(|m| m.state == MachineState::Failed)
        .map(|m| format!("{}: {}", m.name, m.why));
    match failed {
        // The shared band, at once: a machine's failure is already its
        // own judgement (a bay past its deadline, a station not
        // draining), and the record gives it no onset to hold against.
        Some(lead) if region.state != RegionState::Troubled => Region {
            state: RegionState::Troubled,
            band: Some(bands::decide(
                &Finding::new(bands::MACHINE_FAILED, None, String::new(), lead.clone()),
                now,
            )),
            why: format!("{lead} · {}", region.why),
            machines,
            ..region
        },
        _ => Region { machines, ..region },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    #[test]
    fn an_empty_yard_answers_a_clear_card_for_every_region_in_map_order() {
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

    /// EVERY KPI STATES ITS UNIT (design 62de32ae, decisions 5 and 9):
    /// each region names what its count counts, carries at least one
    /// measure with a unit and a sentence, and a bound says whether it is
    /// a capacity or a threshold — on an empty yard and on an unread one
    /// alike. And the middle word is spelled `attention` on the wire.
    #[test]
    fn every_region_states_its_unit_its_kpi_and_what_its_bound_is() {
        let status = empty_status();
        let read = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let mut unread = inputs(&status, &[], &[], &[], &[], None, None);
        unread.dock_reading = Reading::Unread;
        unread.agent_runs = None;
        unread.publish_packets = None;
        for out in [regions(&read), regions(&unread)] {
            for r in &out.regions {
                assert!(!r.unit.is_empty(), "{} names no unit for its count", r.name);
                assert!(!r.kpi.is_empty(), "{} carries no KPI", r.name);
                for m in &r.kpi {
                    assert!(
                        !m.unit.is_empty() && !m.text.is_empty(),
                        "{}: {m:?}",
                        r.name
                    );
                }
                assert_eq!(
                    r.bound.is_some(),
                    r.bound_kind.is_some(),
                    "{}: a bound says what it is",
                    r.name
                );
                assert_eq!(
                    r.band.is_some(),
                    r.state != RegionState::Clear,
                    "{}: every non-clear state names its band, and a clear one none — {}",
                    r.name,
                    r.why
                );
            }
        }
        assert_eq!(
            serde_json::to_value(RegionState::Attention).unwrap(),
            json!("attention")
        );
        // The unread regions name the shared band that decided them.
        let out = regions(&unread);
        assert_eq!(by_name(&out, "dock").band.as_ref().unwrap().id, "unread");
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
}
