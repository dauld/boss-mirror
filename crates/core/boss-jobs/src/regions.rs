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

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::WorkflowSpec;
use crate::yard::{ConductorHealth, Reading, YardStatus};

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
    pub state: RegionState,
    /// One sentence naming why the state is what it is. A verdict must
    /// name what failed (CLAUDE.md §Diagnosis).
    pub why: String,
    pub trend: Trend,
    /// The machinery standing in this region — the gate bays, the
    /// conductor, the stations, the runners — each judged HERE, on the
    /// server, so one definition answers the map, the floor and the
    /// CLI. Empty for a region no machine of ours works in; absent on
    /// an older payload, which a client reads as empty.
    #[serde(default)]
    pub machines: Vec<Machine>,
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

/// The role a node declares when it answers ops-request packets — a
/// Class of `node`, joined through `node_roles` (202609120300), the
/// same vocabulary `cluster-operator` lives in. The tree declares it
/// per machine in infra/estate/estate.toml and the launcher publishes
/// it on every start; this constant is how the map asks the registry
/// which hosts SHOULD have a runner.
pub const OPS_RUNNER_ROLE: &str = "ops-runner";

/// THE CREWS' PACKET KIND (design 511fa7d4 car 2b). One open
/// `work-session` is one crew standing on the shop floor: the
/// SessionStart hook files it and the prompt hook heartbeats it.
pub const SESSION_KIND: &str = "work-session";

/// Silent this long and a crew is drawn IDLE rather than at work — the
/// crew board's own `IDLE_AFTER_MS` (`apps/web/src/it/crew/crew.ts`),
/// ported here so the map and the board stop calling a session
/// "working" at the same moment, and pinned equal by `crew.test.ts`
/// (CLAUDE.md §9a). The session's own silence rule ends it at six
/// hours; this is only where the floor stops crediting it with work.
pub const CREW_IDLE_HOURS: i64 = 1;

/// THE PUBLISH PACKET'S KIND (design cb38d806). One `publish-to-github`
/// packet is one day's measurement of the drift between the forge and
/// the public mirror; the ones that found drift opened a pull request.
pub const PUBLISH_KIND: &str = "publish-to-github";

/// How long a mirror pull request may stand before the publish is
/// STALLED. Design cb38d806 §4, decided by David 2026-09-19: "a stalled
/// publish (open PR older than 24 h, or a red reading unjudged) looks
/// troubled on the surface". The target it serves is §1 — a publish PR
/// every day there is drift, sized like a day of trains — which a PR
/// left standing defeats: #239 carried 1319 files in one commit because
/// the publishes before it had never been merged, and the scan's own
/// footnote says a change that large reads as all-new code.
pub const STALLED_PUBLISH_HOURS: i64 = 24;

/// ONE MIRROR PULL REQUEST, as the publish packet recorded it — the
/// `open-pr` step's own fields, the `read-checks` reading, whether
/// `judge-checks` judged it, and what GitHub last answered about the
/// PR's state (`pr_state`). NOTHING HERE IS FETCHED FROM THE MIRROR:
/// the readback car (backlog 321f1409) put the scan on the packet, and
/// `publish-github-pr.sh --measure` puts the PR's state there (backlog
/// a5d4322c), precisely so a surface could read both with no
/// credential and no second opinion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishPr {
    /// The pull request, as the verb recorded it.
    pub url: String,
    /// The snapshot commit the PR proposes.
    pub snapshot: String,
    /// When `open-pr` completed: the instant the PR was opened.
    pub opened: Instant,
    /// The scan's conclusion as the mirror spells it (`success`,
    /// `failure`, `absent`…). EMPTY when `read-checks` has not
    /// completed — which is not a pass, and is why [`Self::unjudged_red`]
    /// asks for a conclusion that was actually read.
    pub conclusion: String,
    /// The reading's own numbers, as the step recorded them — the
    /// region says the reading, never a symptom (design cb38d806 §4).
    pub alerts: String,
    pub rules: String,
    /// `judge-checks` completed: a disposition per rule is on the
    /// packet. A SKIPPED judge is not a judgement — the workflow skips
    /// it only when the scan concluded `success`, which the conclusion
    /// already says.
    pub judged: bool,
    /// GitHub, asked, said this PR merged.
    pub merged: bool,
    /// GitHub, asked, said this PR is closed — merged or not.
    pub closed: bool,
    /// When GitHub was last asked about this PR. EMPTY when it never
    /// was, which is not "open": the region then says the state was
    /// never read rather than how long the PR has been open.
    pub state_read_at: String,
}

impl PublishPr {
    /// Still standing as far as the record knows: GitHub has not been
    /// read saying it closed. An UNREAD PR counts here — an unasked
    /// question is not a pass — and the region's sentence says which of
    /// the two it is.
    pub fn awaiting(&self) -> bool {
        !self.closed
    }

    /// A RED READING NOBODY JUDGED: the scan was read, it did not
    /// conclude `success`, and no disposition was recorded. This is
    /// design cb38d806 §2's defect exactly — "a red badge on the mirror
    /// with no reading in the system of record is the defect" — and PR
    /// #238 was merged over 64 unread alerts because nothing said so.
    pub fn unjudged_red(&self) -> bool {
        !self.conclusion.is_empty() && self.conclusion != "success" && !self.judged
    }

    /// The READING, in the words the region carries. An alarm that
    /// reports a symptom sends a human to re-derive what the system
    /// already recorded (CLAUDE.md §Diagnosis), so the sentence names
    /// the conclusion and both counts the step wrote down.
    pub fn reading(&self) -> String {
        format!(
            "the scan read {} — {} alert(s) over {} rule(s), no disposition recorded",
            self.conclusion, self.alerts, self.rules
        )
    }
}

/// The pull requests the publish packets opened, newest first — one per
/// packet that reached `open-pr`, with its reading and its state.
///
/// THE STATE IS WHAT GITHUB ANSWERED, never an inference (backlog
/// a5d4322c). It rides the packet as `pr_state`, written by the daily
/// `publish-github-pr.sh --measure` from GitHub's public pulls API, and
/// is taken only when its `pr_url` is this PR's. Until 2026-09-23 the
/// merge was inferred from a later mirror head equalling the PR's
/// snapshot commit — true only of a fast-forward, which GitHub's merge
/// never makes (#239 merged as merge commit b27382e5, #241 was squashed
/// to a7061022) — so every publish PR read open forever, and on
/// 2026-09-22 the region called #239 open for 86 hours, three days
/// after it merged.
pub fn publish_prs(packets: &[(Job, Vec<Step>)]) -> Vec<PublishPr> {
    let mut prs: Vec<PublishPr> = packets
        .iter()
        .filter_map(|(job, steps)| {
            let open_pr = find_step(steps, "open-pr", "open-pr")?;
            let opened = step_done_at(Some(open_pr))?;
            let url = md_str(&open_pr.metadata, "pr_url").to_string();
            if url.is_empty() {
                return None;
            }
            let snapshot = md_str(&open_pr.metadata, "snapshot_commit").to_string();
            let checks = find_step(steps, "read-checks", "read-checks");
            let judge = find_step(steps, "judge-checks", "judge-checks");
            let read = |key: &str| {
                checks
                    .map(|s| md_str(&s.metadata, key).to_string())
                    .unwrap_or_default()
            };
            let observed = job
                .metadata
                .get("pr_state")
                .filter(|o| md_str(o, "pr_url") == url);
            Some(PublishPr {
                merged: observed
                    .and_then(|o| o.get("merged"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                closed: observed.is_some_and(|o| md_str(o, "state") == "closed"),
                state_read_at: observed
                    .map(|o| md_str(o, "read_at").to_string())
                    .unwrap_or_default(),
                url,
                snapshot,
                opened,
                conclusion: read("conclusion"),
                alerts: read("alerts"),
                rules: read("rules"),
                judged: judge.is_some_and(|s| s.status == StepStatus::Completed),
            })
        })
        .collect();
    prs.sort_by_key(|p| std::cmp::Reverse(p.opened));
    prs
}

/// THE HELD PACKET (backlog f49ae66d, the surface half of e1b6ddf7;
/// design cb38d806 §4). The oldest publish packet still OPEN that has
/// opened no pull request, held past [`STALLED_PUBLISH_HOURS`], with
/// what it has been held for, the step it waits at and the drift the
/// daily `--measure` wrote onto it. None when there is no such packet.
///
/// The PR conditions cannot see this case: a packet held before its
/// sign-off opens no PR, and while it is open the daily cadence spawns
/// nothing (`NOT open_publish_exists`), so a quiet region is exactly
/// what a hold used to look like — the 2026-09-17 -> 09-18 hold skipped
/// the week and #239 arrived as 1319 files. The drift reading on the
/// packet (`drift_refresh`, rewritten daily by
/// `publish-github-pr.sh --measure`) is what makes the sentence a
/// reading rather than an age.
fn held_publish(packets: &[(Job, Vec<Step>)], now: Instant) -> Option<String> {
    let (job, steps, since) = packets
        .iter()
        .filter(|(job, steps)| {
            job.status == JobStatus::Open
                && step_done_at(find_step(steps, "open-pr", "open-pr")).is_none()
        })
        .filter_map(|(job, steps)| Some((job, steps, job.opened_at.or_else(|| opened_at(job))?)))
        .filter(|(_, _, since)| (now - *since).num_hours() >= STALLED_PUBLISH_HOURS)
        .min_by_key(|(_, _, since)| *since)?;
    let waiting_at = steps
        .iter()
        .find(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
        .map(|s| s.spec_slug.clone().unwrap_or_else(|| s.title.clone()))
        .unwrap_or_else(|| "no ready step".to_string());
    let drift = match job.metadata.get("drift_refresh") {
        Some(d) => format!(
            "the drift read {} commit(s) / {} file(s) ahead of the mirror at {}",
            md_str(d, "commits_ahead"),
            md_str(d, "files_changed"),
            md_str(d, "measured_at"),
        ),
        None => "no drift measurement is on the packet".to_string(),
    };
    Some(format!(
        "the publish packet opened {} has been held {} at {waiting_at} with no pull request — past the {STALLED_PUBLISH_HOURS}h a publish may stand, and no day behind it is published; {drift}",
        since.format("%Y-%m-%d"),
        plural(
            usize::try_from((now - since).num_hours()).unwrap_or(0),
            "hour",
            "hours"
        ),
    ))
}

/// THE FLOOR'S BOUND: how many runs may be in flight at once, summed
/// over the agents registry's `max_concurrent_runs`. `None` when ANY
/// row declares no cap — an agent without one is unbounded
/// (`agent_budget`'s own rule), so a total that ignored it would draw
/// a bound the claim door does not enforce — and `None` for an empty
/// registry, which bounds nothing either.
pub fn run_capacity(rows: &[crate::agents::AgentRow]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    rows.iter()
        .map(|r| r.max_concurrent_runs.and_then(|n| usize::try_from(n).ok()))
        .sum()
}

/// A host the registry expects an ops-runner on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerHost {
    /// The estate node id — what an ops-request carries as
    /// `metadata.host` and what the runner presents as `HOST_ID`.
    pub id: String,
    pub label: String,
}

/// The hosts a runner is EXPECTED on, from the estate registry's rows.
/// A retired machine is not expected to answer; a node declaring no
/// roles is not a runner host. One definition, read by the handler
/// that fetches the evidence and by the drawing below (CLAUDE.md §9a).
pub fn runner_hosts_of(nodes: &[crate::port::EstateNode]) -> Vec<RunnerHost> {
    nodes
        .iter()
        .filter(|n| !n.retired)
        .filter(|n| n.roles.iter().any(|r| r == OPS_RUNNER_ROLE))
        .map(|n| RunnerHost {
            id: n.id.clone(),
            label: n.label.clone(),
        })
        .collect()
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
        let not_yet = attempt.is_some_and(crate::car::attempt_said_not_yet);
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
        // Attached in one pass in `regions` below, so each region
        // function stays about its own count and trend.
        machines: Vec::new(),
    }
}

// ---------------------------------------------------------------------
// The machinery (design d2154293, car 5).
// ---------------------------------------------------------------------

/// The runners the map draws, one per region: the verb whose
/// ops-requests are the only evidence there is, the region it works
/// in, and what to call it. `http/regions.rs` reads the verbs from
/// [`RUNNER_VERBS`], derived from this — one list, so the read and the
/// drawing cannot drift (CLAUDE.md §9a).
const RUNNERS: [(&str, &str, &str); 2] = [
    ("converge", "arrivals", "converge runner"),
    ("run-car-probe", "shed", "probe runner"),
];

/// The ops-request verbs the handler fetches the newest request of.
pub fn runner_verbs() -> Vec<&'static str> {
    RUNNERS.iter().map(|(verb, _, _)| *verb).collect()
}

fn machine(id: String, name: &str, state: MachineState, why: String) -> Machine {
    Machine {
        id,
        name: name.to_string(),
        state,
        why,
    }
}

/// THE GATE BAYS. The policy declares how many there are and this
/// process counts what occupies them, so a bay nothing is in is
/// OBSERVED empty — the one machine here that can honestly be idle. A
/// bay whose run is `stale` holds a corpse, which is the yard's own
/// judgement (`yard::GATE_MAX_ACTIVE_HOURS`), read rather than redone.
fn gate_bays(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let g = &inputs.status.gates;
    let capacity = usize::try_from(g.capacity).unwrap_or(0);
    // Never fewer bays than there are runs standing in them: a run the
    // policy has no slot for is still occupying something.
    (0..capacity.max(g.active.len()))
        .map(|i| {
            let id = format!("gate-bay-{}", i + 1);
            let name = format!("bay {}", i + 1);
            match g.active.get(i) {
                Some(a) if a.stale => machine(
                    id,
                    &name,
                    MachineState::Failed,
                    format!(
                        "{} has been active past the gate deadline — a corpse holding the bay",
                        a.branch
                    ),
                ),
                Some(a) => machine(
                    id,
                    &name,
                    MachineState::Running,
                    format!("gating {} since {}", a.branch, a.since),
                ),
                None => machine(id, &name, MachineState::Idle, "free".to_string()),
            }
        })
        .collect()
}

/// THE CONDUCTOR. It declares its heartbeat in the cadence registry,
/// so silence IS judgeable here — and only here. Without that declared
/// interval `silent` can never be true (`yard::conductor_health`), so
/// the machine says unknown rather than inheriting the permissive
/// answer.
fn conductor_machine(h: Option<&ConductorHealth>) -> Machine {
    let id = "conductor".to_string();
    let name = "conductor";
    let Some(h) = h else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "the conductor's firing record was not read".to_string(),
        );
    };
    if h.silent {
        let since = match h.silent_for_minutes {
            Some(m) => format!("{m}m since it last fired"),
            None => "past its declared heartbeat".to_string(),
        };
        return machine(
            id,
            name,
            MachineState::Failed,
            format!("SILENT — {since}; every train's truth is last-known-good"),
        );
    }
    if let Some(rc) = h.last_rc.filter(|rc| *rc != 0) {
        let verb = h.last_verb.as_deref().unwrap_or("its last pass");
        return machine(
            id,
            name,
            MachineState::Failed,
            format!("{verb} exited {rc} on its last pass"),
        );
    }
    if h.expected_every_minutes.is_none() {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "no heartbeat is declared in the cadence registry, so silence cannot be judged"
                .to_string(),
        );
    }
    match h.silent_for_minutes {
        Some(m) => machine(
            id,
            name,
            MachineState::Running,
            format!(
                "{} {m}m ago, within its declared heartbeat",
                h.last_verb.as_deref().unwrap_or("fired")
            ),
        ),
        None => machine(
            id,
            name,
            MachineState::Unknown,
            "no firing on record to read a tick from".to_string(),
        ),
    }
}

/// THE STATIONS. Each is a registry row this process evaluated, so its
/// presence is a fact and an empty one is genuinely idle. Flow the cube
/// is blind to is unknown, not zero: a station holding work whose
/// movement nobody can count is exactly the case an idle glyph would
/// lie about.
fn station_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(rows) = inputs.stations else {
        return vec![machine(
            "stations".to_string(),
            "the stations",
            MachineState::Unknown,
            "the station registry could not be read".to_string(),
        )];
    };
    rows.iter()
        .map(|s| {
            let held = s.members.len();
            let id = format!("station:{}", s.name);
            let (state, why) = match (s.over_limit, s.served) {
                (true, _) => (
                    MachineState::Failed,
                    format!("{held} standing — over its WIP limit"),
                ),
                (false, None) => (
                    MachineState::Unknown,
                    format!(
                        "{held} standing — the flow cube is blind to this station's predicate, so whether it moves cannot be told"
                    ),
                ),
                (false, Some(0)) if held > 0 => (
                    MachineState::Failed,
                    format!("{held} standing and nothing served in the window — not draining"),
                ),
                (false, Some(n)) if n > 0 => (
                    MachineState::Running,
                    format!("{n} served in the window, {held} standing"),
                ),
                (false, Some(_)) => (
                    MachineState::Idle,
                    "nothing standing, nothing served in the window".to_string(),
                ),
            };
            machine(id, &s.name, state, why)
        })
        .collect()
}

/// A RUNNER, from its ops-requests — and ONLY from them. A runner
/// polls; it declares no heartbeat anywhere this process can read, so
/// silence says nothing and is never a failure here.
///
/// The machine reports the RUNNER, not the verb's verdict. A verb that
/// answered with a non-zero exit ran fine — `run-car-probe` answers 75
/// ("not yet") on most passes, and a probe judged false is the car's
/// business, which the shed region already reads. What IS the runner's
/// failure: a refusal, and a request closed with no answer recorded at
/// all.
fn runner_machine(inputs: &RegionInputs<'_>, verb: &str, name: &str) -> Machine {
    let id = format!("runner:{verb}");
    let Some(rows) = inputs.ops_requests else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "the ops-request rows could not be read".to_string(),
        );
    };
    let newest = rows
        .iter()
        .filter(|(j, _)| j.metadata.get("verb").and_then(Value::as_str) == Some(verb))
        .max_by_key(|(j, _)| opened_at(j));
    let Some((job, steps)) = newest else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            format!(
                "no {verb} request in the window read — this runner declares no heartbeat, so its silence cannot be judged"
            ),
        );
    };
    judge_request(id, name, verb, job, steps)
}

/// What ONE ops-request says about the runner that took it — shared by
/// the verb runners above and the per-host runners below, so a
/// disposition means the same thing whichever glyph reads it.
fn judge_request(id: String, name: &str, verb: &str, job: &Job, steps: &[Step]) -> Machine {
    if job.status != boss_core::job::JobStatus::Closed {
        return machine(
            id,
            name,
            MachineState::Running,
            format!("a {verb} request is in flight"),
        );
    }
    let execute = find_step(steps, "execute", "Execute the verb");
    let disposition = execute
        .map(|s| md_str(&s.metadata, "disposition"))
        .unwrap_or("");
    let exit = execute
        .map(|s| md_str(&s.metadata, "exit_code"))
        .unwrap_or("");
    match disposition {
        "answered" => machine(
            id,
            name,
            MachineState::Idle,
            format!(
                "last {verb} answered{}{}",
                if exit.is_empty() {
                    String::new()
                } else {
                    format!(" exit {exit}")
                },
                match closed_at(job) {
                    Some(at) => format!(" at {}", at.format("%H:%MZ")),
                    None => String::new(),
                }
            ),
        ),
        "refused" => machine(
            id,
            name,
            MachineState::Failed,
            format!("the last {verb} request was refused — outside the allowlist"),
        ),
        _ => machine(
            id,
            name,
            MachineState::Failed,
            format!("the last {verb} request closed with no answer recorded"),
        ),
    }
}

/// THE HOSTS THE REGISTRY EXPECTS A RUNNER ON (backlog 49ed87b4).
///
/// The verb runners above are named by the requests they HAPPEN to
/// have answered, which is exactly the reading that cannot see a dead
/// host: a machine that answers nothing is drawn nowhere, and an
/// absent glyph is indistinguishable from a runner that does not
/// exist. The estate registry is the independent statement of which
/// hosts SHOULD be answering, so every declared host stands on the map
/// whether or not it has said anything — the false-empty class closed
/// at its most consequential point.
///
/// They stand in RECEIVING because that is where the packets they
/// serve stand: an ops-request is an inbound platform kind
/// ([`inbound_kinds`]), so the region counting them is the region
/// whose machinery answers them.
///
/// A declared host with no request in the window is UNKNOWN, never
/// idle: a runner still declares no poll interval anywhere the system
/// of record can read, so its silence remains unjudgeable. That is the
/// heartbeat half of 49ed87b4 and it is deliberately not claimed here
/// — this half makes the silence VISIBLE, not readable.
fn host_runner_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(hosts) = inputs.runner_hosts else {
        return vec![machine(
            "runner:hosts".to_string(),
            "the ops runners",
            MachineState::Unknown,
            "the estate registry could not be read, so which hosts should have a runner is unknown"
                .to_string(),
        )];
    };
    hosts
        .iter()
        .map(|host| {
            let id = format!("runner:host:{}", host.id);
            let name = format!("{} runner", host.label);
            let Some(rows) = inputs.ops_requests else {
                return machine(
                    id,
                    &name,
                    MachineState::Unknown,
                    "the ops-request rows could not be read".to_string(),
                );
            };
            // ANY verb it answered is evidence the runner polled — a
            // `df` answer proves the loop is alive exactly as a
            // `converge` does.
            let newest = rows
                .iter()
                .filter(|(j, _)| md_str(&j.metadata, "host") == host.id)
                .max_by_key(|(j, _)| opened_at(j));
            match newest {
                Some((job, steps)) => {
                    let verb = md_str(&job.metadata, "verb");
                    let verb = if verb.is_empty() { "ops" } else { verb };
                    judge_request(id, &name, verb, job, steps)
                }
                None => machine(
                    id,
                    &name,
                    MachineState::Unknown,
                    format!(
                        "the estate registry declares an ops-runner on {}, and no request it answered is in the window; a runner declares no poll interval the record can read, so its silence cannot be judged",
                        host.id
                    ),
                ),
            }
        })
        .collect()
}

/// The machinery of one region, by name. A region this answers nothing
/// for has no machine of ours in it — which is a fact, not a gap.
/// THE CREWS ON THE FLOOR — one machine per open session (design
/// 511fa7d4 car 2b, backlog 94c6ffd0). A crew is the only machinery on
/// the map that is mostly a HUMAN or an agent's own session rather than
/// a loop of ours, and it is read the same way: `running` while the
/// heartbeat is fresh, `idle` past [`CREW_IDLE_HOURS`] of silence, and
/// `unknown` where nothing measured it.
///
/// A session that has never prompted is UNKNOWN, not idle. Idle is a
/// reading — "it is here and it has no work" — and the only thing that
/// can take it is the heartbeat the prompt hook writes. Before the
/// first prompt there is no such reading, and a confident idle would
/// say the operator walked away when nothing asked.
fn crew_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(sessions) = inputs.sessions else {
        return vec![machine(
            "crews".to_string(),
            "crews",
            MachineState::Unknown,
            "the work-session packets could not be read".to_string(),
        )];
    };
    let runs_of = |id: &str| {
        inputs
            .agent_runs
            .unwrap_or(&[])
            .iter()
            .filter(|(j, _)| j.status == JobStatus::Open)
            .filter(|(j, _)| md_str(&j.metadata, "session") == id)
            .count()
    };
    sessions
        .iter()
        .map(|s| {
            let id = s.id.to_string();
            let name = {
                let actor = md_str(&s.metadata, "actor");
                if actor.is_empty() {
                    s.title.clone()
                } else {
                    actor.to_string()
                }
            };
            let working = plural(runs_of(&id), "run in flight", "runs in flight");
            let (state, why) = match meta_instant(&s.metadata, "last_active_at") {
                None => (
                    MachineState::Unknown,
                    format!(
                        "no heartbeat on the packet — nothing says whether anyone is here; {working}"
                    ),
                ),
                Some(beat) => {
                    let silent = (inputs.now - beat).num_minutes().max(0);
                    if silent > CREW_IDLE_HOURS * 60 {
                        (
                            MachineState::Idle,
                            format!("silent for {silent} min; {working}"),
                        )
                    } else {
                        (
                            MachineState::Running,
                            format!("last prompt {silent} min ago; {working}"),
                        )
                    }
                }
            };
            machine(format!("session:{id}"), &name, state, why)
        })
        .collect()
}

fn machines_of(name: &str, inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let mut out = match name {
        "shop-floor" => crew_machines(inputs),
        "gates" => gate_bays(inputs),
        "track" => vec![conductor_machine(inputs.conductor)],
        "marshalling" => station_machines(inputs),
        "receiving" => host_runner_machines(inputs),
        _ => Vec::new(),
    };
    out.extend(
        RUNNERS
            .iter()
            .filter(|(_, region, _)| *region == name)
            .map(|(verb, _, label)| runner_machine(inputs, verb, label)),
    );
    out
}

// ---------------------------------------------------------------------
// The regions.
// ---------------------------------------------------------------------

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
        with_machinery(r, machines)
    })
    .collect();
    Regions {
        window_hours: inputs.window_hours,
        regions,
    }
}

/// Hang a region's machinery on it — and let a FAILED machine trouble
/// its territory, so the failure reads at world scale rather than only
/// when someone zooms in. The machine's own sentence leads the `why`:
/// a verdict must name what failed, and the region's existing reason
/// is kept behind it rather than overwritten.
fn with_machinery(region: Region, machines: Vec<Machine>) -> Region {
    let failed = machines
        .iter()
        .find(|m| m.state == MachineState::Failed)
        .map(|m| format!("{}: {}", m.name, m.why));
    match failed {
        Some(lead) if region.state != RegionState::Troubled => Region {
            state: RegionState::Troubled,
            why: format!("{lead} · {}", region.why),
            machines,
            ..region
        },
        _ => Region { machines, ..region },
    }
}

/// Each parked car that declares an ordering edge, with the conductor's
/// OWN judgement of it — `car::boards_after_outcome`, the function the
/// conductor boards by — so the dock and the conductor cannot disagree
/// about whether a car can board (backlog 4142d821, design cf820810 Q7).
/// A car that declares no edge is not listed; a declared predecessor with
/// no reading in [`RegionInputs::predecessors`] is judged unreadable,
/// which boards (fail-open, as the conductor is) and is said.
pub(crate) fn dock_edges<'a>(
    inputs: &RegionInputs<'a>,
) -> Vec<(&'a crate::yard::DockCar, crate::car::EdgeOutcome)> {
    use crate::car::{Predecessor, boards_after_of, boards_after_outcome};
    let status: &'a YardStatus = inputs.status;
    status
        .dock
        .iter()
        .filter_map(|d| {
            let (job, _) = inputs.cars.iter().find(|(j, _)| j.id.to_string() == d.id)?;
            let declared = boards_after_of(&job.metadata)?;
            let pred = inputs
                .predecessors
                .iter()
                .find(|(id, _)| *id == declared)
                .map(|(_, p)| p.clone())
                .unwrap_or_else(|| {
                    Predecessor::Unreadable("the predecessor was not read on this pass".into())
                });
            Some((d, boards_after_outcome(&declared, &pred)))
        })
        .collect()
}

/// The predecessors the held cars wait behind, of one hold kind, each
/// named once however many cars wait behind it.
fn behind_of(holds: &[&crate::car::EdgeHold], kind: &str) -> Vec<String> {
    holds
        .iter()
        .filter(|h| h.kind == kind)
        .fold(Vec::new(), |mut names: Vec<String>, h| {
            if !names.contains(&h.behind) {
                names.push(h.behind.clone());
            }
            names
        })
}

/// THE DOCK: cars parked, and whether they can board. Busy when the
/// boarding depth is met — a train is due — and troubled when the dock
/// row could not be read (an unread dock is not an empty one, 52fed017).
/// The trend is the DOCK WAIT: how long the cars that boarded in the
/// window stood on the dock first, from the car's `gate` stamp to its
/// train's `collect` stamp.
///
/// A CAR THE CONDUCTOR WILL REFUSE IS NOT A TRAIN DUE (backlog 4142d821,
/// design cf820810 Q7). Three cars once sat unable to board for nine and
/// a half hours while this region said "the boarding depth is met, a
/// train is due" — the sentence it says two minutes after a healthy
/// departure — because only the conductor ever asked whether a car
/// could board. So the parked cars' declared ordering edges are judged
/// here by the conductor's own function ([`dock_edges`]), and while any
/// car is held on one the region says "N parked, M cannot board (waiting
/// behind X)" and stays busy rather than promising a train. An edge that
/// can NEVER clear — an abandoned or missing predecessor — troubles the
/// region: the conductor refuses it identically every window until a
/// person acts, and a troubled thing must look troubled.
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
            use crate::car::{EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome};
            let depth = status.dock.len();
            let parked = plural(depth, "car parked", "cars parked");
            let edges = dock_edges(inputs);
            let holds: Vec<&crate::car::EdgeHold> = edges
                .iter()
                .filter_map(|(_, o)| match o {
                    EdgeOutcome::Hold(h) => Some(h),
                    _ => None,
                })
                .collect();
            let unjudged = edges
                .iter()
                .filter(|(_, o)| matches!(o, EdgeOutcome::BoardUnjudged(_)))
                .count();
            let waiting = behind_of(&holds, EDGE_HOLD_WAITING);
            let stuck = behind_of(&holds, EDGE_HOLD_NEEDS_HUMAN);
            let (state, why) = if !holds.is_empty() {
                let clauses: Vec<String> = [
                    (!waiting.is_empty()).then(|| format!("waiting behind {}", waiting.join(", "))),
                    (!stuck.is_empty()).then(|| {
                        format!(
                            "stuck behind {}, an edge that can never be satisfied — a human \
                             must clear it",
                            stuck.join(", ")
                        )
                    }),
                ]
                .into_iter()
                .flatten()
                .collect();
                (
                    if stuck.is_empty() {
                        RegionState::Busy
                    } else {
                        RegionState::Troubled
                    },
                    format!(
                        "{parked}, {} cannot board ({})",
                        holds.len(),
                        clauses.join("; ")
                    ),
                )
            } else if status.boarding.threshold_met == Some(true) {
                (
                    RegionState::Busy,
                    format!("{parked} — the boarding depth is met, a train is due"),
                )
            } else {
                (RegionState::Clear, parked)
            };
            // Unknown is not zero: an edge nobody could judge boards (the
            // conductor fails open on it too), and the region says so.
            let why = if unjudged > 0 {
                format!(
                    "{why} · {} could not be read — the conductor boards {} anyway",
                    plural(unjudged, "ordering edge", "ordering edges"),
                    if unjudged == 1 {
                        "its car"
                    } else {
                        "their cars"
                    }
                )
            } else {
                why
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

/// How long a landed car may stand unproven before the shed says so.
///
/// WHY A BOUND AT ALL (backlog 488d42e6). The shed troubled itself only
/// for a car with NO way to settle (`Unproven`) or a FAILING probe. A
/// car whose probe answers `not yet` — early, not wrong — was never
/// troubled however long it said so, and `not yet` is by far the
/// commonest place a car stops. Measured 2026-09-22: nine cars standing
/// at `proven`, every one `merged = true`, aged 9.0h to **105.7h**, and
/// the shed read `9 landed cars awaiting proof` — the same words it
/// prints ten minutes after a landing. The packet measured 58.3h as the
/// worst case two days earlier, so the age roughly doubled while the
/// COUNT fell from twelve to nine: proofs drain, just slower than they
/// accumulate. That is a rate to be seen, not a queue to be chased.
///
/// WHY 24 HOURS. Most probes are rechecked hourly and most of the
/// events they wait on happen daily, so a car that has not settled
/// inside a day is waiting on something that is not coming on its own.
/// It is a threshold for LOOKING, not a deadline: the car is still
/// correct, still landed, still retrying.
///
/// THE CLOCK IS THE CAR'S OWN `opened_at`, not a landing time, because
/// no landing time is recorded on the car — `merged` is a boolean and
/// the `merged` STEP cannot complete until `proven` does, which is the
/// very thing being waited for. So this over-reports by however long
/// the car took to build and land, and the wording says "open" rather
/// than "landed" for that reason.
pub const PROOF_STALE_HOURS: i64 = 24;

/// How long a probe may answer `not yet` WITHOUT A BREAK before the shed
/// stops calling it the world's move (backlog adef5ddf).
///
/// WHY A SECOND BOUND. [`PROOF_STALE_HOURS`] times the CAR; this times
/// the ANSWER, read from the streak each attempt carries
/// (`boss_jobs::car::not_yet_streak`). A probe that can never pass —
/// b8c4267f greps a literal a later car deliberately removed, 52e0287e
/// reads the wrong occurrence of a call — exits 75 exactly like a
/// patient one, and the shed said "waiting on the world, not on us" of
/// both.
///
/// WHY 72 HOURS, measured over the 1812 ops-requests on record
/// (2026-09-17 to 09-23). Of 37 not-yet streaks that ended in a pass,
/// the longest under the hourly recheck spanned 61h (a4a2118e, 46 runs)
/// and the longest at all 75h (8c4f8ed9, a weekly Stripe event, under
/// the old daily recheck). The six open cars still answering not-yet
/// were at 97h to 134h. So a day would have named six of the honest
/// waits that later passed, and three days names one (that 75h daily-
/// recheck wait) while every stuck car clears it. Like the bound above it is a threshold for LOOKING:
/// the probe is still rechecked hourly and may still pass. What changes
/// is whose move the sentence says it is.
///
/// ONLY FOR A CAR THAT NEVER SAID WHAT IT WAITS ON (backlog b461341d):
/// the triage of the six cars this bound measured found all six honest
/// waits on the world, so a car declaring `waits_on` is judged by
/// whether its declared event has been SEEN instead — see
/// `boss_jobs::car::starved`, the one predicate the shed and orient read.
pub const NOT_YET_STARVED_HOURS: i64 = 72;

/// THE SHED: landed cars awaiting proof — open cars whose live step is
/// `proven`. Troubled when one is UNPROVEN (no probe, no event: nothing
/// mechanical can settle it) or its probe is FAILING, or when a car
/// past [`PROOF_STALE_HOURS`] waits on something that is OURS; busy
/// while any waits and none is ours. The trend is cars proven per day.
///
/// TROUBLED MEANS OURS (backlog 3881f5c9). Until 2026-09-23 every stale
/// car troubled the shed, so it read red while its own words said
/// "waiting on the world, not on us" — six honest waits (a Stripe
/// charge, a release David opens, his destructive prune, a tenant
/// publish, a red crawl, a failed publish) painted exactly like a broken
/// probe. A stale wait is someone else's move only when the car
/// DECLARES it, something OBSERVES it, and it names the OWNER
/// (`boss_jobs::car::owned_wait`); it stays theirs until the event is
/// seen while the probe still says not yet, or it outlives a max wait
/// the car itself declared.
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
    // STALE: awaiting proof for longer than a day, whatever its place.
    // Collected over every awaiting car rather than only the `not yet`
    // ones, so an old car is named even when its place would otherwise
    // read as healthy progress.
    let mut stale: Vec<(i64, &str)> = Vec::new();
    // Of the stale ones, how many have NEVER had their probe run. A
    // probe that ran and said `not yet` is the world answering; a probe
    // with no attempt on record is us not asking. Same age, opposite
    // meaning, and until 2026-09-22 the same sentence.
    let mut never_probed = 0usize;
    // Of the stale ones that were told not yet, those told so without a
    // break for longer than [`NOT_YET_STARVED_HOURS`] — a probe that
    // cannot pass looks exactly like this, so it is ours to read, not
    // the world's to answer (adef5ddf). Longest streak first.
    let mut starved: Vec<(crate::car::NotYetStreak, &str)> = Vec::new();
    // Of those told not yet, the ones whose OWN declared wait is already
    // in the record (b461341d) — the probe cannot see what the car said
    // it was waiting for, which is ours at any streak length. A declared
    // wait not yet seen lands in neither list: its patience is stated.
    let mut seen: Vec<(String, &str)> = Vec::new();
    // Of those told not yet, the ones that DECLARED a wait with no
    // `seen` check (e9b164a1). The declaration exempts them from the
    // streak bound and hands the judgement to that check — so without
    // one nothing can ever say the event arrived, and the exemption is
    // an escape hatch that silences the label forever. Writing the
    // check is ours, so they are counted, not folded into the world's.
    let mut unobserved: Vec<&str> = Vec::new();
    // WHOSE MOVE (3881f5c9): of those told not yet, the ones with NO
    // declared wait (not yet starved — nothing on the car says whose
    // move it is), the ones that declared and observe a wait but name no
    // owner, the ones past the max wait they declared, and — the only
    // ones that are not ours — those waiting on their declared owner.
    let mut undeclared: Vec<&str> = Vec::new();
    let mut prose_only: Vec<&str> = Vec::new();
    let mut unowned: Vec<&str> = Vec::new();
    let mut overdue: Vec<(crate::car::OwnedWait, i64, &str)> = Vec::new();
    let mut theirs: Vec<(crate::car::OwnedWait, &str)> = Vec::new();
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        let Some(opened) = j
            .metadata
            .get("opened_at")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        else {
            continue;
        };
        let hours = (inputs.now - opened.with_timezone(&chrono::Utc)).num_hours();
        if hours >= PROOF_STALE_HOURS {
            stale.push((hours, branch));
            match shed_place(&j.metadata) {
                ShedPlace::ProbePending { last: None } => never_probed += 1,
                // Only prose names what it waits on, and no probe runs,
                // so no `seen` check ever runs either: nothing observes it.
                ShedPlace::WaitingOn(_) => prose_only.push(branch),
                _ => match crate::car::starved(&j.metadata) {
                    Some(crate::car::Starved::Undeclared(streak)) => starved.push((streak, branch)),
                    Some(crate::car::Starved::SeenWhileNotYet { on, .. }) => {
                        seen.push((on, branch))
                    }
                    None => match crate::car::waits_on(&j.metadata) {
                        None => undeclared.push(branch),
                        Some(w) if w.seen.is_none() => unobserved.push(branch),
                        Some(_) => match crate::car::owned_wait(&j.metadata) {
                            Some(o) if o.overdue(hours) => overdue.push((o, hours, branch)),
                            Some(o) => theirs.push((o, branch)),
                            None => unowned.push(branch),
                        },
                    },
                },
            }
        }
    }
    stale.sort_by_key(|(hours, _)| std::cmp::Reverse(*hours));
    starved.sort_by_key(|(s, _)| std::cmp::Reverse(s.hours));

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
    } else if let Some((oldest, branch)) = stale.first().copied() {
        // The age is the finding, so it leads — and the oldest car is
        // named, because "9 awaiting proof" sends a reader to a list
        // while "105h, fix/x" sends them to a car.
        // WHOSE MOVE IS IT. The age says something is stuck; this says
        // whether anyone here can unstick it, and since 3881f5c9 it
        // also decides the colour: only a declared, observed, owned
        // wait is someone else's move, and a shed of nothing else is
        // busy, not troubled. Every other answer is ours, each named
        // for what there is to do — run the probe, read one that cannot
        // pass (adef5ddf), read one blind to its own event (b461341d),
        // write the seen check (e9b164a1), or declare whose move it is.
        let mut ours: Vec<String> = Vec::new();
        if never_probed > 0 {
            ours.push(format!(
                "{never_probed} never probed — nothing has run their proof, which is on us"
            ));
        }
        if let Some((longest, branch)) = starved.first() {
            ours.push(format!(
                "{} told not yet without a break past {NOT_YET_STARVED_HOURS}h — longest {}h \
                 over {} runs, {branch}; a probe that cannot pass says exactly this, so it is \
                 ours to read, not the world's",
                starved.len(),
                longest.hours,
                longest.runs
            ));
        }
        if let Some((on, branch)) = seen.first() {
            ours.push(format!(
                "{} told not yet AFTER what they declared they wait on was seen in the record \
                 — {branch}, waiting on {on}; the probe cannot see its own event, so it is ours \
                 to read",
                seen.len()
            ));
        }
        if let Some(branch) = unobserved.first() {
            ours.push(format!(
                "{} declared a wait with no seen check — {branch}; nothing can ever say its \
                 event arrived, so writing one is ours (boss car waits-on --seen)",
                unobserved.len()
            ));
        }
        if let Some(branch) = undeclared.first() {
            ours.push(format!(
                "{} told not yet with no declared wait — {branch}; saying whose move it is \
                 is ours (boss car waits-on)",
                undeclared.len()
            ));
        }
        if let Some(branch) = prose_only.first() {
            ours.push(format!(
                "{} wait on an event only prose names, with no probe — {branch}; nothing \
                 observes it, so it is ours",
                prose_only.len()
            ));
        }
        if let Some(branch) = unowned.first() {
            ours.push(format!(
                "{} declared no owner for their wait — {branch}; naming one (world, or the \
                 actor whose act it is) is ours",
                unowned.len()
            ));
        }
        if let Some((o, hours, branch)) = overdue.first() {
            ours.push(format!(
                "{} past the max wait they declared — {branch}, waiting on {}: {}, {hours}h \
                 against {}h; ours to read",
                overdue.len(),
                o.owner,
                o.on,
                o.max_wait_hours.unwrap_or_default()
            ));
        }
        // Named one by one: a wait on the world and a wait on David's
        // act are different errands, and "6 waiting" sends no one to
        // either.
        let waits = theirs
            .iter()
            .map(|(o, branch)| format!("waiting on {}: {} ({branch})", o.owner, o.on))
            .collect::<Vec<_>>()
            .join("; ");
        let (state, whose) = if ours.is_empty() {
            (RegionState::Busy, format!("none ours — {waits}"))
        } else if theirs.is_empty() {
            (RegionState::Troubled, ours.join("; "))
        } else {
            (
                RegionState::Troubled,
                format!(
                    "{}; the rest wait on their declared owner — {waits}",
                    ours.join("; ")
                ),
            )
        };
        (
            state,
            format!(
                "{} of {n} open past {PROOF_STALE_HOURS}h — oldest {oldest}h, {branch} — {whose}",
                stale.len()
            ),
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
/// Is a repair already aimed at this branch?
///
/// WHY (backlog aae1515e; David, 2026-09-21: "the Garage has been
/// flashing troubled all day, and it is either too long to have not
/// addressed or we need to have some indication that the fix is in
/// transit"). BOTH halves were true that day, and the second caused the
/// first. The garage went troubled at 08:50Z on a gate-run recorded
/// `lost`; at 18:0xZ the branch was still on the forge and nothing had
/// moved it in nine hours. The surface was ACCURATE the whole time —
/// and it rendered identically at minute five and at hour nine, which
/// is why nine hours passed.
///
/// A troubled region can be cleared only by the fix landing, so a long
/// repair is indistinguishable from total neglect. §Diagnosis already
/// says a troubled packet must look troubled; the corollary is that a
/// troubled thing BEING REPAIRED must look different from one nobody
/// has touched, or the colour stops carrying information and the reader
/// learns to discount it — the same decay as a permanently-red check.
///
/// THE PREDICATE IS CHEAP AND EXACT for this region: a lost or
/// never-judged gate-run whose BRANCH has a newer gate-run still open
/// is under repair. Nothing is inferred — the newer run is the repair,
/// and it is in the same list the region already reads.
fn under_repair(gate_runs: &[Job], branch: &str, packet_id: &str) -> bool {
    gate_runs.iter().any(|r| {
        r.status == boss_core::job::JobStatus::Open
            && r.id.to_string() != packet_id
            && r.metadata.get("branch").and_then(Value::as_str) == Some(branch)
    })
}

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
    // How many of the troubled members already have a newer gate-run
    // open on their branch — a repair in flight (aae1515e).
    let repairing = s
        .stranded
        .iter()
        .filter(|g| under_repair(inputs.gate_runs, &g.branch, &g.packet_id))
        .count()
        + s.limbo
            .iter()
            .filter(|l| under_repair(inputs.gate_runs, &l.branch, &l.packet_id))
            .count();
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
        // STILL TROUBLED, ANNOTATED — deliberately not a fourth state.
        // A new value in the state vocabulary forces every consumer to
        // handle it (the yard, the world map, orient, any alarm keyed on
        // `troubled`), and one that does not becomes WRONG rather than
        // merely incomplete. The reader's question is "is anyone on it",
        // and a sentence answers that without moving the colour.
        if repairing > 0 {
            parts.push(format!(
                "{repairing} of {} under repair — a newer gate-run is open on that branch",
                stranded + limbo
            ));
        } else {
            parts.push("nothing aimed at any of them".to_string());
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

/// THE SHOP FLOOR: what is being BUILT — the runs in flight, with the
/// sessions that dispatched them standing in the region as crews
/// (design 511fa7d4 car 2b, backlog 94c6ffd0). It is the region
/// UPSTREAM OF THE DOCK, and the map had no such place until now: the
/// world began where a car was already finished, and the interval an
/// actor spends building one was rows on a board somewhere else.
///
/// The count is the runs IN FLIGHT against [`run_capacity`], because
/// that pair is the fact an operator needs before queueing more work —
/// a floor at its cap refuses the next dispatch. Troubled is the
/// failure the cap makes expensive: a run whose `building` step is
/// done while its `reported` step is still open has FINISHED and is
/// still holding a slot, which is invisible from every count of "runs
/// in flight" that does not read the steps. The trend is the build
/// duration — a run's own open-to-close, for the runs that closed in
/// each window.
fn shop_floor(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let Some(runs) = inputs.agent_runs else {
        return region(
            "shop-floor",
            None,
            inputs.run_capacity,
            RegionState::Troubled,
            "the agent-run packets could not be read".to_string(),
            duration_trend("build duration", "minutes", Vec::new(), Vec::new()),
        );
    };
    let durations = runs.iter().filter_map(|(j, _)| {
        let opened = opened_at(j)?;
        let closed = closed_at(j)?;
        let d = (closed - opened).num_seconds();
        (d > 0).then_some((closed, d))
    });
    let (cur, prev) = split(w, durations);
    let trend = duration_trend("build duration", "minutes", cur, prev);

    let in_flight: Vec<&(Job, Vec<Step>)> = runs
        .iter()
        .filter(|(j, _)| j.status == JobStatus::Open)
        .collect();
    // Finished, and still holding its slot: `building` completed, the
    // handback never recorded. The run's own step is the only place
    // this shows — the packet is open and looks like work in progress.
    let unreported = in_flight
        .iter()
        .filter(|(_, steps)| {
            step_done_at(find_step(steps, "building", "Building")).is_some()
                && step_done_at(find_step(steps, "reported", "Report recorded")).is_none()
        })
        .count();
    // A crew count is only stated where the sessions were READ: an
    // unread session list is not a floor with nobody on it, and the
    // clause is left off rather than printed as a zero.
    let crews = inputs
        .sessions
        .map(|s| plural(s.len(), "crew on the floor", "crews on the floor"));
    let count = in_flight.len();
    let at_cap = inputs.run_capacity.is_some_and(|cap| count >= cap);
    let (state, why) = if unreported > 0 {
        (
            RegionState::Troubled,
            format!(
                "{} finished and not reported — each holds a slot until the handback lands",
                plural(unreported, "run", "runs")
            ),
        )
    } else if at_cap {
        (
            RegionState::Busy,
            format!(
                "{} — at the cap, the next dispatch is refused",
                plural(count, "run in flight", "runs in flight")
            ),
        )
    } else {
        (
            RegionState::Clear,
            [
                Some(plural(count, "run in flight", "runs in flight")),
                crews,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<String>>()
            .join(", "),
        )
    };
    region(
        "shop-floor",
        Some(count),
        inputs.run_capacity,
        state,
        why,
        trend,
    )
}

/// THE PUBLISH REGION (design cb38d806, backlog eee42416) — the
/// crossing OUT of the world, where what landed on main is proposed to
/// the public mirror as a pull request and a person merges it.
///
/// The count is the pull requests the record shows STILL OPEN, because
/// that is the number the design's targets are written against: one
/// publish a day, sized like a day of trains (§1), and at most one
/// human act per publish (§3). A PR left standing is what turns the
/// next one into a week-scale snapshot no reader and no scan can judge.
///
/// The two troubled conditions are §4's, verbatim, and each carries
/// what it read rather than that something is wrong:
///
///   * A RED READING NOBODY JUDGED ([`PublishPr::unjudged_red`]) — the
///     defect §2 names. This leads, because it is the one a merge would
///     make permanent: #238 was merged over 64 unread alerts.
///   * A PULL REQUEST OPEN PAST [`STALLED_PUBLISH_HOURS`] — the publish
///     has stalled on the human gate, and every day it stands makes the
///     next diff larger.
///   * A PACKET HELD PAST [`STALLED_PUBLISH_HOURS`] WITH NO PULL REQUEST
///     ([`held_publish`], backlog f49ae66d) — the same stall one gate
///     earlier, at the sign-off, where no PR exists for the two above
///     to see. It reads last because it names a packet, not a PR.
///
/// The trend is publishes per day: the PRs opened in each window, which
/// is §1's own number.
fn publish(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let Some(packets) = inputs.publish_packets else {
        return region(
            "publish",
            None,
            None,
            RegionState::Troubled,
            "the publish-to-github packets could not be read".to_string(),
            rate_trend("publishes", w, 0, 0),
        );
    };
    let prs = publish_prs(packets);
    let (cur, prev) = count_split(w, prs.iter().map(|p| p.opened));
    let trend = rate_trend("publishes", w, cur, prev);

    let open: Vec<&PublishPr> = prs.iter().filter(|p| p.awaiting()).collect();
    let unjudged = open.iter().find(|p| p.unjudged_red());
    // Oldest first: the PR that has stood longest is the one the
    // sentence should name.
    let stalled = open
        .iter()
        .filter(|p| (inputs.now - p.opened).num_hours() >= STALLED_PUBLISH_HOURS)
        .min_by_key(|p| p.opened);
    let (state, why) = if let Some(p) = unjudged {
        (
            RegionState::Troubled,
            format!("{} — {}", p.url, p.reading()),
        )
    } else if let Some(p) = stalled {
        let age = plural(
            usize::try_from((inputs.now - p.opened).num_hours()).unwrap_or(0),
            "hour",
            "hours",
        );
        // Openness is said only where GitHub was READ saying it
        // (backlog a5d4322c); an unread PR is named as unread.
        let why = if p.state_read_at.is_empty() {
            format!(
                "{} was opened {age} ago and its state was never read from GitHub — past the {STALLED_PUBLISH_HOURS}h a publish may stand, so it is stalled or unobserved",
                p.url
            )
        } else {
            format!(
                "{} has been open {age} (GitHub read it open at {}) — past the {STALLED_PUBLISH_HOURS}h a publish may stand, and every day it does the next diff is larger",
                p.url, p.state_read_at
            )
        };
        (RegionState::Troubled, why)
    } else if let Some(why) = held_publish(packets, inputs.now) {
        (RegionState::Troubled, why)
    } else if let Some(p) = open.first() {
        (
            RegionState::Busy,
            format!(
                "{} — {} awaiting a merge",
                p.url,
                plural(open.len(), "pull request", "pull requests")
            ),
        )
    } else {
        (
            RegionState::Clear,
            "no pull request awaiting a merge".to_string(),
        )
    };
    region("publish", Some(open.len()), None, state, why, trend)
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
            opened_at: None,
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
            conductor: None,
            ops_requests: Some(&[]),
            runner_hosts: Some(&[]),
            agent_runs: Some(&[]),
            sessions: Some(&[]),
            publish_packets: Some(&[]),
            run_capacity: None,
            predecessors: &[],
            now: t(NOW),
            window_hours: 24,
        }
    }

    fn machines_in<'a>(r: &'a Regions, region: &str) -> &'a [Machine] {
        &by_name(r, region).machines
    }

    fn machine_state(r: &Regions, region: &str, id: &str) -> MachineState {
        machines_in(r, region)
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{region} has no machine {id}"))
            .state
    }

    fn by_name<'a>(r: &'a Regions, name: &str) -> &'a Region {
        r.regions.iter().find(|x| x.name == name).unwrap()
    }

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

    /// A STALE PROOF IS EITHER ON US OR ON THE WORLD, and the shed said
    /// neither.
    ///
    /// Measured 2026-09-22, an hour after the staleness signal above
    /// shipped: the shed read "8 of 12 open past 24h — oldest 108h" and
    /// three rechecks of the oldest cars each came back "not yet: no
    /// real prune since convergence", "not yet: no sponsorship packet
    /// opened since the change converged", "not yet: no answered
    /// sweep-archive-branches request". Those cars are HEALTHY and
    /// blocked on a qualifying event the world has not produced — the
    /// probe asked and was answered. A car whose probe has never been
    /// run is the opposite, and the same sentence covered both.
    ///
    /// Reporting them identically is the decay CLAUDE.md names under
    /// "a check nobody reads": a colour that fires on a state nobody
    /// can act on teaches the reader to discount it. The distinction
    /// already exists in the type — `ShedPlace::ProbeNotYet` versus a
    /// `ProbePending` with no attempt recorded — and only the stale
    /// branch threw it away.
    #[test]
    fn a_stale_proof_says_whether_it_waits_on_us_or_on_the_world() {
        // NOW is 2026-09-19T12:00:00Z; both cars are two days old, so
        // age cannot be what separates them.
        let aged = |branch: &str, attempt: Value| {
            let mut md = json!({
                "branch": branch,
                "merged": true,
                "opened_at": "2026-09-17T10:00:00Z",
                "proof_probe": "true",
            });
            if !attempt.is_null() {
                md["proof_attempt"] = attempt;
            }
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();

        // ON THE WORLD: the probe ran and was told not yet.
        let asked = vec![aged("feat/asked", json!({ "not_yet": true, "exit": 75 }))];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &asked,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Troubled,
            "still troubled — a car stuck two days is worth a look either way: {}",
            shed.why
        );
        assert!(
            shed.why.contains("told not yet"),
            "but it says the probe asked and the world answered: {}",
            shed.why
        );
        assert!(
            !shed.why.contains("never"),
            "and does not accuse anyone of neglecting it: {}",
            shed.why
        );

        // ON US, same age, same everything else: no attempt recorded,
        // so nothing has ever run this car's probe. THE CONTROL — it is
        // what stops the clause reading as always-waiting-on-the-world.
        let never = vec![aged("feat/never", Value::Null)];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &never,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("never probed"),
            "the same age with no attempt on record reads as on us: {}",
            shed.why
        );

        // MIXED: the count has to survive both being present, or the
        // commonest real shed (a few of each) gets one of the two
        // sentences and the other half goes unmentioned.
        let both = vec![
            aged("feat/asked", json!({ "not_yet": true, "exit": 75 })),
            aged("feat/never", Value::Null),
        ];
        let out = regions(&inputs(&status, &[], &[], &both, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert!(
            shed.why.contains("1 never probed"),
            "names how many are on us, alongside the rest: {}",
            shed.why
        );
    }

    /// A PROBE THAT HAS ANSWERED NOT-YET FOR DAYS IS NOT WAITING ON THE
    /// WORLD (backlog adef5ddf). The split above read every told-not-yet
    /// car as the world's move, and two of the six measured at 86+
    /// consecutive not-yets could never pass: b8c4267f greps a literal a
    /// later car removed, 52e0287e reads the wrong occurrence of a call.
    /// Exit 75 cannot tell them apart from a patient probe; the length of
    /// the streak can, so a streak past [`NOT_YET_STARVED_HOURS`] is
    /// named as ours to read — and a short one, same car age, is not.
    #[test]
    fn a_not_yet_that_has_lasted_days_is_ours_to_read_not_the_worlds() {
        // NOW is 2026-09-19T12:00:00Z; both cars opened four days ago,
        // so the car's age cannot be what separates them — only the
        // streak the attempt carries.
        let aged = |branch: &str, since: &str, runs: u64| {
            let md = json!({
                "branch": branch,
                "merged": true,
                "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z",
                    "not_yet": true,
                    "exit": 75,
                    "probe": "true",
                    (crate::car::NOT_YET_SINCE): since,
                    (crate::car::NOT_YET_RUNS): runs,
                },
            });
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };

        // STARVED: 96 runs across 96 hours, all not-yet.
        let shed = read(&[aged("fix/starved", "2026-09-15T11:00:00Z", 96)]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            !shed.why.contains("waiting on the world"),
            "a streak of days is not the world's move: {}",
            shed.why
        );
        assert!(
            shed.why.contains("ours to read") && shed.why.contains("fix/starved"),
            "it names the car, as ours to read: {}",
            shed.why
        );
        assert!(
            shed.why.contains("96h") && shed.why.contains("96 runs"),
            "carrying the streak, which is the finding: {}",
            shed.why
        );

        // THE CONTROL: same age, a six-hour streak — not starved. It
        // declared no wait, so it is still not the world's (3881f5c9):
        // nothing on the car says whose move it is.
        let shed = read(&[aged("fix/patient", "2026-09-19T05:00:00Z", 7)]);
        assert!(
            shed.why.contains("no declared wait") && !shed.why.contains("waiting on the world"),
            "a short undeclared streak is not starved, and not the world's either: {}",
            shed.why
        );
        assert!(!shed.why.contains("ours to read"), "{}", shed.why);

        // MIXED: both counts survive together.
        let shed = read(&[
            aged("fix/starved", "2026-09-15T11:00:00Z", 96),
            aged("fix/patient", "2026-09-19T05:00:00Z", 7),
        ]);
        assert!(
            shed.why.contains("1 told not yet without a break")
                && shed.why.contains("1 told not yet with no declared wait"),
            "names the starved one and the undeclared one apart: {}",
            shed.why
        );
    }

    /// A CAR THAT SAID WHAT IT WAITS ON IS NOT STARVED BY DURATION
    /// (backlog b461341d). All six cars adef5ddf's streak measured were
    /// honest waits on the world; with `waits_on` declared, a 96h streak
    /// is the world's — until the declared event is SEEN in the record
    /// while the probe still says not-yet, which is ours at once.
    #[test]
    fn a_declared_wait_is_the_worlds_until_its_event_is_seen() {
        let car = |branch: &str, seen_at: Option<&str>| {
            let mut attempt = json!({
                "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                (crate::car::NOT_YET_RUNS): 96,
            });
            if let Some(s) = seen_at {
                attempt[crate::car::WAITS_ON_SEEN_AT] = json!(s);
            }
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true", "proof_attempt": attempt,
                (crate::car::WAITS_ON): {
                    "on": "a real Stripe sponsorship charge", "seen": "true",
                    (crate::car::WAITS_ON_OWNER): "world",
                },
            });
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };

        let shed = read(&[car("fix/declared", None)]);
        assert!(
            shed.why.contains("waiting on the world") && !shed.why.contains("ours to read"),
            "a declared wait not yet seen is the world's, at 96h: {}",
            shed.why
        );
        assert_eq!(shed.state, RegionState::Busy, "{}", shed.why);

        let shed = read(&[car("fix/contradicted", Some("2026-09-19T10:00:00Z"))]);
        assert!(
            shed.why.contains("ours to read")
                && shed.why.contains("fix/contradicted")
                && shed.why.contains("a real Stripe sponsorship charge"),
            "seen in the record, still not yet — ours, naming what it waited on: {}",
            shed.why
        );
    }

    /// A DECLARED WAIT WITH NO OBSERVER IS COUNTED, NOT SILENT (backlog
    /// e9b164a1). b461341d exempts a declared wait from the streak bound
    /// and hands its judgement to the `seen` check — so a declaration
    /// with `seen` null has handed it to nothing, and without this count
    /// it read exactly like an observed wait: "waiting on the world",
    /// forever. All six cars declared on 2026-09-23 were in that shape.
    #[test]
    fn a_declared_wait_with_no_seen_check_is_counted_as_ours() {
        let car = |branch: &str, seen: Option<&str>| {
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": "a real Stripe sponsorship charge", "seen": seen,
                    (crate::car::WAITS_ON_OWNER): "world",
                },
            });
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };

        let shed = read(&[car("fix/unobserved", None)]);
        assert!(
            shed.why.contains("1 declared a wait with no seen check")
                && shed.why.contains("fix/unobserved")
                && !shed.why.contains("waiting on the world"),
            "a declaration nothing observes is ours, and counted: {}",
            shed.why
        );

        let shed = read(&[
            car("fix/unobserved", None),
            car("fix/observed", Some("true")),
        ]);
        assert!(
            shed.why.contains("1 declared a wait with no seen check")
                && shed.why.contains("the rest wait on their declared owner")
                && shed.why.contains("waiting on the world"),
            "an observed wait stays the world's beside it: {}",
            shed.why
        );

        let shed = read(&[car("fix/observed", Some("true"))]);
        assert!(
            !shed.why.contains("no seen check"),
            "an observed wait is not counted: {}",
            shed.why
        );
    }

    /// THE SHED'S COLOUR FOLLOWS WHOSE MOVE THE WAIT IS (backlog
    /// 3881f5c9). Measured 2026-09-23 ~16:05Z: the shed read troubled
    /// with the words "every one asked and was told not yet — waiting on
    /// the world, not on us" — the region said the waits were not ours
    /// and painted them red anyway, so red stopped meaning ours to fix.
    /// Troubled is now only ours; a declared, observed, owned wait —
    /// on the world or on a named actor's act — is busy and says whose,
    /// until it runs past a max wait the car itself declared.
    #[test]
    fn the_shed_is_troubled_only_by_waits_that_are_ours() {
        let car = |branch: &str, on: &str, owner: Value, max: Value| {
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": on, "seen": "true",
                    (crate::car::WAITS_ON_OWNER): owner,
                    (crate::car::WAITS_ON_MAX_WAIT_HOURS): max,
                },
            });
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };
        let world = || {
            car(
                "feat/stripe",
                "a Stripe charge",
                json!("world"),
                Value::Null,
            )
        };
        let david = || {
            car(
                "feat/release",
                "a cut-a-release packet",
                json!("emp-david"),
                Value::Null,
            )
        };

        // THEIRS: the world's event and a named actor's act, 98h open.
        let shed = read(&[world(), david()]);
        assert_eq!(
            shed.state,
            RegionState::Busy,
            "declared, observed, owned waits are not ours: {}",
            shed.why
        );
        assert!(
            shed.why
                .contains("waiting on the world: a Stripe charge (feat/stripe)")
                && shed
                    .why
                    .contains("waiting on emp-david: a cut-a-release packet"),
            "and it says whose move each one is: {}",
            shed.why
        );

        // PAST ITS OWN DECLARED PATIENCE: 98h open against a 72h max.
        let late = car("feat/late", "a Stripe charge", json!("world"), json!(72));
        let shed = read(&[late, david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("past the max wait they declared")
                && shed.why.contains("feat/late")
                && shed.why.contains("the rest wait on their declared owner"),
            "{}",
            shed.why
        );

        // NO OWNER: the prose may name David, the field does not.
        let unowned = car(
            "feat/unowned",
            "a release (David opens it)",
            Value::Null,
            Value::Null,
        );
        let shed = read(&[unowned]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("declared no owner") && shed.why.contains("feat/unowned"),
            "an owner is declared, never read out of prose: {}",
            shed.why
        );

        // AN EVENT ONLY PROSE NAMES: no probe runs, so no `seen` check
        // does either — nothing observes it (the dev-door login car,
        // 2026-09-23, is this shape).
        let (mut j, s) = world();
        j.metadata = json!({
            "branch": "feat/prose", "merged": true, "opened_at": "2026-09-15T10:00:00Z",
            "proof_event": "David logs in through the dev door",
        });
        let shed = read(&[(j, s), david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("only prose names") && shed.why.contains("feat/prose"),
            "{}",
            shed.why
        );
    }

    /// A car awaiting proof PAST A DAY troubles the shed, even when its
    /// probe is answering `not yet` — early, not wrong, and the
    /// commonest place a car stops (backlog 488d42e6).
    ///
    /// Measured 2026-09-22: nine cars at `proven`, every one merged,
    /// aged 9.0h to 105.7h, and the shed said `9 landed cars awaiting
    /// proof` — the words it prints ten minutes after a landing. The
    /// age had roughly doubled since the packet was filed while the
    /// COUNT fell, so the rate was the finding and nothing showed it.
    #[test]
    fn a_car_awaiting_proof_past_a_day_troubles_the_shed() {
        // NOW is 2026-09-19T12:00:00Z.
        let aged = |branch: &str, opened: &str| {
            let j = job(
                "ship-a-change",
                branch,
                JobStatus::Open,
                json!({
                    "branch": branch,
                    "merged": true,
                    "opened_at": opened,
                    "proof_probe": "true",
                    // `not yet` — the place that was never troubled.
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();

        // FRESH: landed this morning, still working. Must stay busy, or
        // the signal fires on every landing and stops meaning anything.
        let fresh = vec![aged("fix/fresh", "2026-09-19T06:00:00Z")];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &fresh,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Busy,
            "a car six hours old is working, not troubled: {}",
            shed.why
        );

        // STALE: two days at `not yet`.
        let stale = vec![
            aged("fix/fresh", "2026-09-19T06:00:00Z"),
            aged("feat/two-days", "2026-09-17T10:00:00Z"),
        ];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &stale,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Troubled,
            "a car past {PROOF_STALE_HOURS}h must trouble the shed: {}",
            shed.why
        );
        assert!(
            shed.why.contains("feat/two-days"),
            "and NAME the oldest — a count sends a reader to a list, a branch sends them \
             to a car: {}",
            shed.why
        );
        assert!(
            shed.why.contains("50h"),
            "carrying its age, which is the finding: {}",
            shed.why
        );
        assert!(
            !shed.why.contains("fix/fresh"),
            "and not the fresh one, which is not the problem: {}",
            shed.why
        );

        // A CAR WITH NO `opened_at` IS SKIPPED, not treated as
        // infinitely old. An absent timestamp is not evidence of age,
        // and reading it as one would trouble the shed for a missing
        // field — the zero-means-unknown defect, in a region card.
        let no_clock = {
            let j = job(
                "ship-a-change",
                "fix/no-clock",
                JobStatus::Open,
                json!({
                    "branch": "fix/no-clock",
                    "merged": true,
                    "proof_probe": "true",
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let st = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            vec![(j, st)]
        };
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &no_clock,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Busy,
            "a car with no opened_at has no age, so it is busy — never troubled for a \
             field it does not carry: {}",
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

    /// A parked car on the dock — open, review ready — with this metadata.
    fn parked(branch: &str, extra: Value) -> (Job, Vec<Step>) {
        let mut md = json!({ "branch": branch });
        if let (Some(dst), Some(src)) = (md.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let j = job("ship-a-change", branch, JobStatus::Open, md);
        let s = vec![step(&j, crate::car::REVIEW_SLUG, StepStatus::Ready, None)];
        (j, s)
    }

    /// A status whose dock holds these cars, at the boarding depth.
    fn dock_at_depth(cars: &[(Job, Vec<Step>)]) -> YardStatus {
        let mut status = empty_status();
        status.dock = cars.iter().map(|(j, _)| crate::yard::dock_car(j)).collect();
        status.boarding.threshold_met = Some(true);
        status
    }

    /// THE INCIDENT'S SURFACE (backlog 4142d821, design cf820810 Q7).
    /// Three cars sat unable to board for nine and a half hours while
    /// this region said "the boarding depth is met, a train is due" —
    /// the sentence it says two minutes after a healthy departure —
    /// because only the conductor ever asked whether a car could board.
    /// A car held on its declared ordering edge is counted, named by
    /// what it waits behind, and the region stays busy WITHOUT promising
    /// a train.
    #[test]
    fn a_car_waiting_behind_its_edge_keeps_the_dock_busy_and_promises_no_train() {
        let first = parked("fix/first-half", json!({}));
        let pred_id = first.0.id.to_string();
        let second = parked("fix/second-half", json!({ "boards_after": pred_id }));
        let cars = vec![first.clone(), second];
        let status = dock_at_depth(&cars);
        let preds = vec![(
            pred_id.clone(),
            crate::car::Predecessor::Found(packet_value(&first.0, &first.1)),
        )];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let dock = by_name(&out, "dock");
        assert_eq!(dock.state, RegionState::Busy, "{}", dock.why);
        assert_eq!(dock.count, Some(2), "parked is still parked");
        let behind = format!("fix/first-half (car {})", &pred_id[..8]);
        assert_eq!(
            dock.why,
            format!("2 cars parked, 1 cannot board (waiting behind {behind})")
        );
        assert!(
            !dock.why.contains("a train is due"),
            "a dock with a car the conductor will refuse must not promise a train: {}",
            dock.why
        );
        assert_eq!(
            declared_edges(&status, &cars),
            vec![pred_id],
            "the handler reads exactly the predecessors this region judges"
        );
    }

    /// An edge that can NEVER clear is not a wait: the conductor refuses
    /// the car identically every window until a person acts, so the
    /// region says so and looks troubled (CLAUDE.md §Diagnosis: a
    /// troubled packet must look troubled).
    #[test]
    fn a_car_behind_an_edge_that_can_never_clear_troubles_the_dock() {
        let gone = "cccccccc-1111-2222-3333-444444444444".to_string();
        let held = parked("fix/orphan", json!({ "boards_after": gone }));
        let cars = vec![held];
        let status = dock_at_depth(&cars);
        let preds = vec![(gone, crate::car::Predecessor::Absent)];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let dock = by_name(&out, "dock");
        assert_eq!(dock.state, RegionState::Troubled, "{}", dock.why);
        assert!(
            dock.why.starts_with("1 car parked, 1 cannot board"),
            "{}",
            dock.why
        );
        assert!(
            dock.why.contains("car cccccccc") && dock.why.contains("a human must clear"),
            "name what it is stuck behind, and whose move it is: {}",
            dock.why
        );
    }

    /// FAIL-OPEN, AS THE CONDUCTOR IS. An edge whose predecessor could
    /// not be read boards the car, so it is not counted as held — but
    /// the region says it could not judge it rather than staying silent.
    /// And a dock with no edges at all reads exactly as it did.
    #[test]
    fn an_unread_edge_boards_as_the_conductor_boards_it_and_says_so() {
        let pred_id = "dddddddd-1111-2222-3333-444444444444".to_string();
        let car = parked("fix/after-a-blip", json!({ "boards_after": pred_id }));
        let cars = vec![car];
        let status = dock_at_depth(&cars);
        let preds = vec![(
            pred_id,
            crate::car::Predecessor::Unreadable("HTTP 503".into()),
        )];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let dock = by_name(&out, "dock");
        assert_eq!(dock.state, RegionState::Busy, "{}", dock.why);
        assert!(
            dock.why
                .starts_with("1 car parked — the boarding depth is met, a train is due"),
            "{}",
            dock.why
        );
        assert!(
            dock.why.contains("1 ordering edge could not be read"),
            "{}",
            dock.why
        );

        let plain = vec![parked("fix/no-edge", json!({}))];
        let status = dock_at_depth(&plain);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &plain,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        assert_eq!(
            by_name(&out, "dock").why,
            "1 car parked — the boarding depth is met, a train is due",
            "a car with no edge — every car but one today — reads as it always did"
        );
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
        assert!(
            garage.why.contains("nothing aimed at any of them"),
            "and with no newer gate-run on that branch, says nobody is on it — which is \
             the fact nine hours of flashing never carried (aae1515e): {}",
            garage.why
        );
    }

    /// A REPAIR IN FLIGHT LOOKS DIFFERENT FROM NEGLECT (backlog
    /// aae1515e; David, 2026-09-21: "it is either too long to have not
    /// addressed or we need to have some indication that the fix is in
    /// transit").
    ///
    /// The garage went troubled at 08:50Z on a lost gate-run and read
    /// identically at minute five and at hour nine, so nine hours
    /// passed. Still TROUBLED here — deliberately not a fourth state,
    /// which would force every consumer to learn a new value — but the
    /// sentence now answers the reader's actual question.
    #[test]
    fn a_stranded_green_with_a_newer_gate_run_says_a_repair_is_in_flight() {
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
        assert_eq!(status.stranded.len(), 1);

        // The repair: a SECOND, still-open gate-run on the same branch.
        let repair = job(
            "gate-run",
            "fix/lost",
            JobStatus::Open,
            json!({ "branch": "fix/lost", "opened_at": "2026-09-19T11:00:00Z" }),
        );
        let rows = vec![run.clone(), repair];
        let out = regions(&inputs(&status, &[], &[], &[], &rows, Some(&[]), Some(&[])));
        let garage = by_name(&out, "garage");
        assert_eq!(
            garage.state,
            RegionState::Troubled,
            "a repair in flight does not make it well — the green is still stranded"
        );
        assert!(
            garage.why.contains("under repair"),
            "but it says someone is on it: {}",
            garage.why
        );
        assert!(
            !garage.why.contains("nothing aimed"),
            "and drops the sentence that would send a reader to look: {}",
            garage.why
        );

        // THE CONTROL, on the same fixture: without the newer run it
        // reads as neglect again. Without this the annotation could be
        // unconditional and nobody would notice.
        let alone = vec![run];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &alone,
            Some(&[]),
            Some(&[]),
        ));
        let garage = by_name(&out, "garage");
        assert!(
            garage.why.contains("nothing aimed at any of them"),
            "the same stranded green with no repair reads as nobody on it: {}",
            garage.why
        );
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

    // -----------------------------------------------------------------
    // The machinery (design d2154293, car 5).
    // -----------------------------------------------------------------

    /// An ops-request as the runner leaves it: the packet plus the
    /// `execute` step the runner completes in one write.
    fn ops_request(
        verb: &str,
        status: JobStatus,
        disposition: Option<&str>,
        exit: Option<&str>,
    ) -> (Job, Vec<Step>) {
        let job = job(
            "ops-request",
            verb,
            status,
            json!({ "verb": verb, "opened_at": "2026-09-19T11:00:00Z", "closed_at": "2026-09-19T11:01:00Z" }),
        );
        let mut execute = step(&job, "execute", StepStatus::Completed, None);
        execute.metadata = json!({
            "disposition": disposition.unwrap_or(""),
            "exit_code": exit.unwrap_or(""),
        });
        (job, vec![execute])
    }

    /// A machine nobody can read is UNKNOWN, and unknown is its own
    /// reading — never the idle one. This is the whole point of the
    /// fourth state: a default of `idle` would have the world map draw
    /// a calm, confident machinery hall for a system nothing is
    /// measuring.
    #[test]
    fn an_unreadable_machine_is_unknown_and_never_idle() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), None);
        i.conductor = None;
        i.ops_requests = None;
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "track", "conductor"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "arrivals", "runner:converge"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "marshalling", "stations"),
            MachineState::Unknown
        );
        // And nothing unknown is ever drawn as idle anywhere on the map.
        for r in &out.regions {
            for m in &r.machines {
                assert_ne!(
                    (m.state, m.why.contains("could not be read")),
                    (MachineState::Idle, true),
                    "{}/{}: {}",
                    r.name,
                    m.id,
                    m.why
                );
            }
        }
    }

    /// A conductor that declares no heartbeat cannot be judged silent —
    /// `yard::conductor_health` leaves `silent` false — so the machine
    /// says unknown rather than inheriting that permissive answer.
    #[test]
    fn the_conductor_is_running_idle_never_and_unknown_without_a_declared_heartbeat() {
        let status = empty_status();
        let heard = crate::yard::conductor_health(
            Some(t("2026-09-19T11:55:00Z")),
            Some("reconcile"),
            Some(0),
            Some(10),
            Some(t(NOW)),
        );
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.conductor = Some(&heard);
        assert_eq!(
            machine_state(&regions(&i), "track", "conductor"),
            MachineState::Running
        );

        let undeclared = crate::yard::conductor_health(
            Some(t("2026-09-19T01:00:00Z")),
            Some("reconcile"),
            Some(0),
            None,
            Some(t(NOW)),
        );
        i.conductor = Some(&undeclared);
        assert_eq!(
            machine_state(&regions(&i), "track", "conductor"),
            MachineState::Unknown
        );
    }

    /// A failed machine troubles its territory at WORLD scale: the
    /// region turns troubled and the machine's own sentence leads the
    /// `why`, so the map names the failure without a zoom.
    #[test]
    fn a_silent_conductor_fails_its_machine_and_troubles_the_track() {
        let status = empty_status();
        let silent = crate::yard::conductor_health(
            Some(t("2026-09-19T10:00:00Z")),
            Some("reconcile"),
            Some(0),
            Some(10),
            Some(t(NOW)),
        );
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.conductor = Some(&silent);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "track", "conductor"),
            MachineState::Failed
        );
        let track = by_name(&out, "track");
        assert_eq!(track.state, RegionState::Troubled);
        assert!(track.why.starts_with("conductor: SILENT"), "{}", track.why);
        // The region's own reading is kept behind the machine's, not
        // overwritten — a verdict adds to the record, it does not
        // replace it.
        assert!(track.why.contains("in transit"), "{}", track.why);
    }

    /// The runner machine reports the RUNNER, not the verb's verdict:
    /// `run-car-probe` answers 75 ("not yet") on most passes and the
    /// runner is fine. A refusal is its failure.
    #[test]
    fn a_runner_that_answered_is_idle_whatever_the_verb_exited_and_a_refusal_fails_it() {
        let status = empty_status();
        let answered = [ops_request(
            "run-car-probe",
            JobStatus::Closed,
            Some("answered"),
            Some("75"),
        )];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.ops_requests = Some(&answered);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Idle
        );
        assert_eq!(by_name(&out, "shed").state, RegionState::Clear);

        let refused = [ops_request(
            "converge",
            JobStatus::Closed,
            Some("refused"),
            None,
        )];
        i.ops_requests = Some(&refused);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "arrivals", "runner:converge"),
            MachineState::Failed
        );
        assert_eq!(by_name(&out, "arrivals").state, RegionState::Troubled);
        // The probe runner has no request in these rows at all — which
        // is unknown, not idle: it declares no heartbeat.
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Unknown
        );

        let in_flight = [ops_request("converge", JobStatus::Open, None, None)];
        i.ops_requests = Some(&in_flight);
        assert_eq!(
            machine_state(&regions(&i), "arrivals", "runner:converge"),
            MachineState::Running
        );
    }

    /// A gate bay is the one machine that can honestly be idle: the
    /// policy declares how many bays there are and this process counts
    /// what stands in them, so an empty bay is OBSERVED empty.
    #[test]
    fn every_gate_bay_the_policy_declares_is_a_machine_and_a_corpse_fails_one() {
        let run = job(
            "gate-run",
            "gate feat/x",
            JobStatus::Open,
            json!({ "branch": "feat/x", "opened_at": "2026-09-17T00:00:00Z" }),
        );
        let runs = [run];
        let status = build_status_for(
            YardInputs {
                gate_runs: &runs
                    .iter()
                    .map(|j| (j.clone(), Vec::new()))
                    .collect::<Vec<_>>(),
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let bays = machines_in(&out, "gates");
        assert_eq!(
            bays.len(),
            usize::try_from(status.gates.capacity).unwrap(),
            "one machine per declared bay"
        );
        // The run opened two days ago, so the yard calls it stale — a
        // corpse holding the bay, which fails that bay and troubles the
        // gates.
        assert_eq!(
            machine_state(&out, "gates", "gate-bay-1"),
            MachineState::Failed
        );
        assert_eq!(
            machine_state(&out, "gates", "gate-bay-2"),
            MachineState::Idle
        );
        assert_eq!(by_name(&out, "gates").state, RegionState::Troubled);
    }

    /// A station whose flow the cube cannot count is UNKNOWN even while
    /// it holds work — the reading an idle glyph would lie about.
    #[test]
    fn a_station_with_uncountable_flow_is_unknown_and_an_over_limit_one_fails() {
        let status = empty_status();
        let rows = [
            StationReading {
                name: "design-review".into(),
                over_limit: false,
                members: vec!["a".into(), "b".into()],
                served: None,
                previous_served: None,
            },
            StationReading {
                name: "backlog".into(),
                over_limit: true,
                members: vec!["c".into()],
                served: Some(3),
                previous_served: Some(2),
            },
            StationReading {
                name: "quiet".into(),
                over_limit: false,
                members: vec![],
                served: Some(0),
                previous_served: Some(0),
            },
        ];
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&rows)));
        assert_eq!(
            machine_state(&out, "marshalling", "station:design-review"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "marshalling", "station:backlog"),
            MachineState::Failed
        );
        assert_eq!(
            machine_state(&out, "marshalling", "station:quiet"),
            MachineState::Idle
        );
    }

    /// The verbs the handler fetches are DERIVED from the runner table
    /// the map draws from, so the read and the drawing cannot drift
    /// (CLAUDE.md §9a).
    #[test]
    fn the_runner_verbs_the_handler_reads_are_the_runners_the_map_draws() {
        assert_eq!(runner_verbs(), vec!["converge", "run-car-probe"]);
    }

    // -----------------------------------------------------------------
    // The hosts that SHOULD have a runner (backlog 49ed87b4).
    // -----------------------------------------------------------------

    /// The same ops-request, filed against a host — what a runner reads
    /// to decide a packet is its own (`ops-runner.sh`: metadata.host
    /// equals HOST_ID).
    fn on_host(mut r: (Job, Vec<Step>), host: &str) -> (Job, Vec<Step>) {
        r.0.metadata
            .as_object_mut()
            .expect("job metadata is an object")
            .insert("host".to_string(), json!(host));
        r
    }

    fn host(id: &str) -> RunnerHost {
        RunnerHost {
            id: id.to_string(),
            label: id.to_string(),
        }
    }

    /// THE DEAD HOST IS DRAWN. A runner named only by what it answered
    /// is invisible the moment it stops answering, and an absent glyph
    /// is indistinguishable from a runner that does not exist. The
    /// registry says which hosts SHOULD have one, so the host with
    /// nothing in the window is a machine on the map — unknown, because
    /// a runner still declares no poll interval, never idle.
    #[test]
    fn a_declared_runner_host_with_nothing_in_the_window_is_drawn_unknown_not_absent() {
        let status = empty_status();
        let hosts = [host("forge"), host("boss-gcp")];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = Some(&hosts);
        let out = regions(&i);
        let drawn: Vec<&str> = machines_in(&out, "receiving")
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(drawn, vec!["runner:host:forge", "runner:host:boss-gcp"]);
        let m = machines_in(&out, "receiving")
            .iter()
            .find(|m| m.id == "runner:host:boss-gcp")
            .expect("the declared host is drawn");
        assert_eq!(m.state, MachineState::Unknown);
        assert!(
            m.why.contains("declares an ops-runner"),
            "the why names the registry that expects it: {}",
            m.why
        );
    }

    /// A host's runner is judged from ANY verb it answered — the
    /// evidence is the runner polling, not what the verb decided. A
    /// refusal and an answerless close are the runner's own failures.
    #[test]
    fn a_runner_host_is_judged_from_any_verb_it_answered() {
        let status = empty_status();
        let hosts = [host("boss-gcp")];
        let rows = [on_host(
            ops_request("uptime", JobStatus::Closed, Some("answered"), Some("0")),
            "boss-gcp",
        )];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = Some(&hosts);
        i.ops_requests = Some(&rows);
        assert_eq!(
            machine_state(&regions(&i), "receiving", "runner:host:boss-gcp"),
            MachineState::Idle
        );

        let refused = [on_host(
            ops_request("converge", JobStatus::Closed, Some("refused"), None),
            "boss-gcp",
        )];
        i.ops_requests = Some(&refused);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "receiving", "runner:host:boss-gcp"),
            MachineState::Failed
        );
        assert_eq!(by_name(&out, "receiving").state, RegionState::Troubled);

        let in_flight = [on_host(
            ops_request("df", JobStatus::Open, None, None),
            "boss-gcp",
        )];
        i.ops_requests = Some(&in_flight);
        assert_eq!(
            machine_state(&regions(&i), "receiving", "runner:host:boss-gcp"),
            MachineState::Running
        );

        // Another host's request is not this host's evidence.
        let elsewhere = [on_host(
            ops_request("uptime", JobStatus::Closed, Some("answered"), Some("0")),
            "forge",
        )];
        i.ops_requests = Some(&elsewhere);
        assert_eq!(
            machine_state(&regions(&i), "receiving", "runner:host:boss-gcp"),
            MachineState::Unknown
        );
    }

    /// An unread estate registry is ONE unknown machine, never an
    /// estate with no runners in it — the false-empty class the whole
    /// machinery reading exists to refuse.
    #[test]
    fn an_unread_estate_registry_is_unknown_and_never_an_empty_estate() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = None;
        let out = regions(&i);
        let m = machines_in(&out, "receiving")
            .iter()
            .find(|m| m.id == "runner:hosts")
            .expect("an unread registry is still a machine");
        assert_eq!(m.state, MachineState::Unknown);
        assert!(
            m.why.contains("could not be read"),
            "the why names the read that failed: {}",
            m.why
        );
    }

    /// The hosts the handler reads are DERIVED from the same role the
    /// map draws on, and a retired machine is not expected to answer
    /// (CLAUDE.md §9a: one definition, not two lists).
    #[test]
    fn the_runner_hosts_are_the_nodes_declaring_the_role_and_never_a_retired_one() {
        let node = |id: &str, roles: &[&str], retired: bool| crate::port::EstateNode {
            id: id.to_string(),
            label: format!("{id} label"),
            address: "10.0.0.1".to_string(),
            role: "forge".to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            notes: None,
            retired,
        };
        let nodes = [
            node("forge", &[OPS_RUNNER_ROLE, "cluster-operator"], false),
            node("w-1", &[], false),
            node("old", &[OPS_RUNNER_ROLE], true),
        ];
        assert_eq!(
            runner_hosts_of(&nodes),
            vec![RunnerHost {
                id: "forge".to_string(),
                label: "forge label".to_string()
            }]
        );
    }

    // -----------------------------------------------------------------
    // THE SHOP FLOOR (backlog 94c6ffd0) — the region upstream of the
    // dock, where a car is still being built.
    // -----------------------------------------------------------------

    fn run(open: bool, opened: &str, closed: Option<&str>) -> Job {
        let mut md = json!({ "opened_at": opened, "agent": "agent-claude" });
        if let Some(c) = closed {
            md["closed_at"] = json!(c);
        }
        job(
            crate::agent_budget::RUN_KIND,
            "a run",
            if open {
                JobStatus::Open
            } else {
                JobStatus::Closed
            },
            md,
        )
    }

    fn session(actor: &str, last_active: Option<&str>) -> Job {
        let mut md = json!({ "actor": actor, "started_at": "2026-09-19T06:00:00Z" });
        if let Some(t) = last_active {
            md["last_active_at"] = json!(t);
        }
        job(SESSION_KIND, "a session", JobStatus::Open, md)
    }

    fn floor<'a>(
        base: RegionInputs<'a>,
        runs: &'a [(Job, Vec<Step>)],
        sessions: &'a [Job],
        capacity: Option<usize>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            agent_runs: Some(runs),
            sessions: Some(sessions),
            run_capacity: capacity,
            ..base
        }
    }

    #[test]
    fn the_shop_floor_counts_the_runs_in_flight_against_the_registrys_capacity() {
        let status = empty_status();
        let runs = vec![
            (run(true, "2026-09-19T11:00:00Z", None), Vec::new()),
            (run(true, "2026-09-19T11:30:00Z", None), Vec::new()),
            // Closed inside the window: not in flight — the trend's
            // sample instead.
            (
                run(false, "2026-09-19T08:00:00Z", Some("2026-09-19T09:00:00Z")),
                Vec::new(),
            ),
        ];
        let sessions = Vec::new();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let r = regions(&floor(base, &runs, &sessions, Some(6)));
        let f = by_name(&r, "shop-floor");
        assert_eq!(f.count, Some(2), "two runs are in flight");
        assert!(
            f.why.contains("0 crews on the floor"),
            "a READ session list of none is a real zero: {}",
            f.why
        );
        assert_eq!(f.bound, Some(6));
        assert_eq!(f.state, RegionState::Clear, "{}", f.why);
        assert_eq!(f.trend.metric, "build duration");
        assert_eq!(f.trend.unit, "minutes");
        assert_eq!(f.trend.current, Some(60.0), "the one run that closed");
    }

    #[test]
    fn a_floor_at_the_registrys_capacity_is_busy_because_the_next_dispatch_is_refused() {
        let status = empty_status();
        let runs: Vec<(Job, Vec<Step>)> = (0..2)
            .map(|_| (run(true, "2026-09-19T11:00:00Z", None), Vec::new()))
            .collect();
        let sessions = Vec::new();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let r = regions(&floor(base, &runs, &sessions, Some(2)));
        let f = by_name(&r, "shop-floor");
        assert_eq!(f.state, RegionState::Busy, "{}", f.why);
        assert!(f.why.contains("at the cap"), "{}", f.why);
    }

    /// A run that finished and never reported still holds its slot —
    /// the failure the cap makes expensive, and the one an operator
    /// needs to see from world scale.
    #[test]
    fn a_finished_but_unreported_run_troubles_the_shop_floor() {
        let status = empty_status();
        let j = run(true, "2026-09-19T09:00:00Z", None);
        let steps = vec![
            step(
                &j,
                "building",
                StepStatus::Completed,
                Some("2026-09-19T10:00:00Z"),
            ),
            step(&j, "reported", StepStatus::Ready, None),
        ];
        let runs = vec![(j, steps)];
        let sessions = Vec::new();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let r = regions(&floor(base, &runs, &sessions, Some(6)));
        let f = by_name(&r, "shop-floor");
        assert_eq!(f.state, RegionState::Troubled, "{}", f.why);
        assert!(
            f.why.contains("holds a slot") && f.why.contains("not reported"),
            "the why names the failure and its cost: {}",
            f.why
        );
    }

    /// An unread floor is troubled, never an empty one: a map that drew
    /// nobody building would read as a quiet shop rather than an unread
    /// one (the rule every region here keeps).
    #[test]
    fn an_unread_run_list_is_a_troubled_floor_and_never_a_quiet_one() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let r = regions(&RegionInputs {
            agent_runs: None,
            sessions: None,
            run_capacity: None,
            ..base
        });
        let f = by_name(&r, "shop-floor");
        assert_eq!(f.count, None);
        assert!(
            !f.why.contains("crew"),
            "an unread session list states no crew count at all: {}",
            f.why
        );
        assert_eq!(f.state, RegionState::Troubled);
        assert!(f.why.contains("could not be read"), "{}", f.why);
        // And the crews: one unknown machine naming the failed read,
        // never a floor with nobody standing on it.
        let m = machines_in(&r, "shop-floor");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].state, MachineState::Unknown);
    }

    /// The crews ARE the sessions (design 511fa7d4 car 2b). Idle is a
    /// reading, taken only where the packet declares a heartbeat; a
    /// session that has never prompted is unknown, not idle.
    #[test]
    fn the_crews_are_the_sessions_and_silence_is_read_only_from_a_heartbeat() {
        let status = empty_status();
        let sessions = vec![
            session("emp-david", Some("2026-09-19T11:55:00Z")),
            session("claude@algedonic.dev", Some("2026-09-19T06:30:00Z")),
            session("emp-quiet", None),
        ];
        let at_work = sessions[0].id.to_string();
        let silent = sessions[1].id.to_string();
        let never = sessions[2].id.to_string();
        let runs = Vec::new();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let r = regions(&floor(base, &runs, &sessions, None));
        assert_eq!(
            machine_state(&r, "shop-floor", &format!("session:{at_work}")),
            MachineState::Running
        );
        assert_eq!(
            machine_state(&r, "shop-floor", &format!("session:{silent}")),
            MachineState::Idle
        );
        assert_eq!(
            machine_state(&r, "shop-floor", &format!("session:{never}")),
            MachineState::Unknown,
            "a session that never prompted is not idle — nothing measured it"
        );
    }

    // -----------------------------------------------------------------
    // The publish region (design cb38d806, backlog eee42416).
    // -----------------------------------------------------------------

    /// One publish packet as the live ones are shaped (measured against
    /// b423d16b, 7d5c9051 and 254177e2 on 2026-09-22): the `open-pr`
    /// step carries the PR, its snapshot and the mirror head it was
    /// built on; `read-checks` carries the reading; `judge-checks` is
    /// skipped when the scan concluded `success`.
    fn publish_packet(
        pr: &str,
        snapshot: &str,
        mirror_head: &str,
        opened_at: &str,
        reading: Option<(&str, &str, &str)>,
        judged: bool,
    ) -> (Job, Vec<Step>) {
        let j = job(
            PUBLISH_KIND,
            "Publish to the public mirror",
            JobStatus::Closed,
            json!({}),
        );
        let mut open_pr = step(&j, "open-pr", StepStatus::Completed, Some(opened_at));
        open_pr.metadata = json!({
            "pr_url": pr,
            "snapshot_commit": snapshot,
            "mirror_head": mirror_head,
        });
        let mut steps = vec![open_pr];
        if let Some((conclusion, alerts, rules)) = reading {
            let mut checks = step(&j, "read-checks", StepStatus::Completed, Some(opened_at));
            checks.metadata = json!({ "conclusion": conclusion, "alerts": alerts, "rules": rules });
            steps.push(checks);
            steps.push(step(
                &j,
                "judge-checks",
                if judged {
                    StepStatus::Completed
                } else if conclusion == "success" {
                    StepStatus::Skipped
                } else {
                    StepStatus::Ready
                },
                judged.then_some(opened_at),
            ));
        }
        (j, steps)
    }

    fn with_publish<'a>(
        base: RegionInputs<'a>,
        packets: Option<&'a [(Job, Vec<Step>)]>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            publish_packets: packets,
            ..base
        }
    }

    /// §4's first troubled condition: a pull request standing past a
    /// day. The record has the instant (`open-pr` completed) and what
    /// GitHub answered when `--measure` asked (`pr_state`), so the
    /// region can say STALLED from the record alone.
    #[test]
    fn a_mirror_pull_request_open_past_a_day_troubles_the_publish_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        // Opened 30 hours before NOW, and GitHub, asked, said open.
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/240",
                "snap-240",
                "mirror-a",
                "2026-09-18T06:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/240",
            "open",
            false,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(1));
        assert!(
            p.why
                .contains("GitHub read it open at 2026-09-19T00:01:08Z"),
            "the why says when openness was observed: {}",
            p.why
        );
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(
            p.why.contains("pull/240"),
            "the why names the PR: {}",
            p.why
        );
        assert!(
            p.why.contains("30 hours"),
            "the why says how long it has stood: {}",
            p.why
        );
    }

    /// §4's second: a red reading with no disposition. The sentence
    /// carries THE READING — the conclusion and both counts the step
    /// recorded — not "something is wrong with the publish" (a verdict
    /// must name what failed).
    #[test]
    fn a_red_reading_nobody_judged_troubles_the_region_and_carries_the_reading() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        // Opened an hour ago: not stalled, so the trouble can only be
        // the unjudged reading.
        let packets = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-19T11:00:00Z",
            Some(("failure", "109", "14")),
            false,
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(p.why.contains("pull/239"), "{}", p.why);
        assert!(
            p.why.contains("failure") && p.why.contains("109") && p.why.contains("14"),
            "the why is the reading, not a symptom: {}",
            p.why
        );

        // Judged, the same reading is no longer trouble: the merge now
        // follows a disposition per rule, which is all §2 asks.
        let judged = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-19T11:00:00Z",
            Some(("failure", "109", "14")),
            true,
        )];
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let out = regions(&with_publish(base, Some(&judged)));
        assert_eq!(by_name(&out, "publish").state, RegionState::Busy);
    }

    /// What `publish-github-pr.sh --measure` writes onto a publish
    /// packet when it asks GitHub about the pull request the packet
    /// opened (backlog a5d4322c): the PR it asked about, the state and
    /// merge GitHub answered, and when it asked.
    fn observed(
        (mut j, steps): (Job, Vec<Step>),
        pr: &str,
        state: &str,
        merged: bool,
        read_at: &str,
    ) -> (Job, Vec<Step>) {
        j.metadata = json!({ "pr_state": {
            "pr_url": pr, "state": state, "merged": merged, "read_at": read_at,
        }});
        (j, steps)
    }

    /// The merge is READ, never assumed. Measured 2026-09-22 (backlog
    /// a5d4322c): the region called #239 open for 86 hours and itself
    /// TROUBLED over it, while GitHub said #239 had merged three days
    /// earlier. It had inferred the merge from a later mirror head
    /// equalling the PR's snapshot, which a GitHub merge never makes
    /// true — #239 merged as merge commit b27382e5 and #241 was
    /// squashed to a7061022, neither of them the snapshot. So a later
    /// packet built on an old one's snapshot proves nothing; what
    /// GitHub answered does.
    #[test]
    fn a_pull_request_github_read_as_merged_leaves_the_count_and_clears_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/239",
                "snap-239",
                "mirror-a",
                "2026-09-15T08:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/239",
            "closed",
            true,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(0), "{}", p.why);
        assert_eq!(p.state, RegionState::Clear, "{}", p.why);

        // A PR closed WITHOUT a merge awaits nothing either.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/238",
                "snap-238",
                "mirror-a",
                "2026-09-15T08:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/238",
            "closed",
            false,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        assert_eq!(by_name(&out, "publish").count, Some(0));

        // The retired inference: a later snapshot built on this one's is
        // NOT a merge the region may claim.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![
            publish_packet(
                "https://mirror/pull/240",
                "snap-240",
                "mirror-a",
                "2026-09-19T11:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            publish_packet(
                "https://mirror/pull/241",
                "snap-241",
                "snap-240",
                "2026-09-19T11:30:00Z",
                Some(("success", "0", "0")),
                false,
            ),
        ];
        let out = regions(&with_publish(base, Some(&packets)));
        assert_eq!(
            by_name(&out, "publish").count,
            Some(2),
            "a mirror head is not GitHub's answer"
        );
    }

    /// The sentence says only what was observed. A PR whose state was
    /// never read from GitHub is NOT "open for N hours" — that is the
    /// claim the region could not earn (backlog a5d4322c) — it is a PR
    /// nobody asked about, and it still troubles the region past a day,
    /// because an unasked question is not a pass.
    #[test]
    fn a_pull_request_never_read_from_github_is_not_called_open() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-18T06:00:00Z",
            Some(("success", "0", "0")),
            false,
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(!p.why.contains("has been open"), "{}", p.why);
        assert!(p.why.contains("never read from GitHub"), "{}", p.why);

        // A reading of ANOTHER pull request is no reading of this one.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/239",
                "snap-239",
                "mirror-a",
                "2026-09-18T06:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/238",
            "closed",
            true,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(1), "{}", p.why);
        assert!(p.why.contains("never read from GitHub"), "{}", p.why);
    }

    /// A publish packet HELD before its pull request, as the live one
    /// was shaped on 2026-09-23 (d2967a9c): open, `open-pr` not reached,
    /// the step it waits at `ready`, and the daily `--measure` reading
    /// on its metadata as `drift_refresh`.
    fn held_packet(opened_at: &str, waiting_at: &str, drift: Option<Value>) -> (Job, Vec<Step>) {
        let mut j = job(
            PUBLISH_KIND,
            "Publish to the public mirror",
            JobStatus::Open,
            drift
                .map(
                    |d| json!({ "drift_refresh": d, "drift_refreshed_at": "2026-09-19T00:01:08Z" }),
                )
                .unwrap_or_else(|| json!({})),
        );
        j.opened_at = Some(t(opened_at));
        let steps = vec![
            step(&j, "opened", StepStatus::Completed, Some(opened_at)),
            step(&j, waiting_at, StepStatus::Ready, None),
            step(&j, "open-pr", StepStatus::Pending, None),
        ];
        (j, steps)
    }

    /// THE HELD PACKET (backlog f49ae66d, the surface half of e1b6ddf7;
    /// design cb38d806). A packet left open before its sign-off opens no
    /// pull request, so the two PR conditions cannot see it — and it is
    /// the hold that cost the week of 2026-09-17 -> 09-18, because the
    /// daily cadence spawns nothing while a packet is open. Held past a
    /// day it is troubled, and the sentence carries the step it waits
    /// at and the drift `--measure` wrote onto it, not a symptom.
    #[test]
    fn a_publish_packet_held_past_a_day_without_a_pull_request_troubles_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![held_packet(
            "2026-09-17T12:00:00Z",
            "approve",
            Some(json!({
                "commits_ahead": "495",
                "files_changed": "114",
                "has_drift": "true",
                "measured_at": "2026-09-19T00:01:08Z",
            })),
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert_eq!(
            p.count,
            Some(0),
            "the count is still pull requests: {}",
            p.why
        );
        assert!(
            p.why.contains("48 hours"),
            "how long it has been held: {}",
            p.why
        );
        assert!(p.why.contains("approve"), "the step it waits at: {}", p.why);
        assert!(
            p.why.contains("495")
                && p.why.contains("114")
                && p.why.contains("2026-09-19T00:01:08Z"),
            "the dated drift reading: {}",
            p.why
        );

        // No reading on it yet is said, never drawn as a current mirror.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let bare = vec![held_packet("2026-09-17T12:00:00Z", "measure", None)];
        let out = regions(&with_publish(base, Some(&bare)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(p.why.contains("no drift measurement"), "{}", p.why);
    }

    /// Today's packet, a few hours into its own measure step, is the
    /// cadence working — not a hold. And a packet that CLOSED without a
    /// pull request (`nothing-to-publish`, `declined`) holds nothing.
    #[test]
    fn a_publish_packet_inside_its_day_or_closed_does_not_trouble_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let fresh = vec![held_packet("2026-09-19T08:00:00Z", "measure", None)];
        let out = regions(&with_publish(base, Some(&fresh)));
        assert_eq!(by_name(&out, "publish").state, RegionState::Clear);

        let (mut closed, steps) = held_packet("2026-09-15T00:00:00Z", "declined", None);
        closed.status = JobStatus::Closed;
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let done = vec![(closed, steps)];
        let out = regions(&with_publish(base, Some(&done)));
        assert_eq!(by_name(&out, "publish").state, RegionState::Clear);
    }

    /// An unread publish list is troubled, never a mirror that reads as
    /// current — the rule every region here keeps.
    #[test]
    fn an_unread_publish_list_is_troubled_and_never_a_current_mirror() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let out = regions(&with_publish(base, None));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, None);
        assert_eq!(p.state, RegionState::Troubled);
        assert!(p.why.contains("could not be read"), "{}", p.why);
    }

    /// The bound is the registry's, and an agent with no declared cap is
    /// unbounded — so the total is, too (the claim door's own rule).
    #[test]
    fn the_run_capacity_is_the_registrys_sum_and_an_undeclared_cap_is_unbounded() {
        let row = |cap: Option<i32>| crate::agents::AgentRow {
            id: "agent-claude".to_string(),
            display_name: "Claude".to_string(),
            default_model: "opus-5".to_string(),
            role: None,
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: cap,
            aliases: vec![],
        };
        assert_eq!(run_capacity(&[row(Some(6)), row(Some(2))]), Some(8));
        assert_eq!(run_capacity(&[row(Some(6)), row(None)]), None);
        assert_eq!(run_capacity(&[]), None);
    }
}
