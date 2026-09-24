//! The IT world map's BORDERS — `GET /api/yard/borders` (design
//! d2154293, decided 2026-09-19; this is car 2).
//!
//! WHY THIS EXISTS. Car 1 drew the map's regions as territories in one
//! coordinate space with a rail between each pair, and the rails were
//! decoration: a line with nothing on it. The feedback that opened this
//! design asked to "see the interactivity between regions at the
//! border" (c3105b2a). A border between two regions is a real thing in
//! this system — packets cross it at a rate, packets stand at it
//! waiting to cross, and some machine moves them — so this module
//! answers those three questions for every border the layout declares:
//!
//! 1. **The crossing rate** — packets per day that moved from A to B in
//!    this window, against the same window before it. Each border says
//!    IN WORDS what one crossing is ([`BorderSpec::crossing`]), and the
//!    count is that transition's own stamp in the rows the regions read
//!    (a step completing, a run opening, a train closing).
//! 2. **What is waiting to cross right now**, with the hold reason on
//!    each — greens gated and unparked, runs queued for a bay, cars
//!    parked waiting for a train, cars released by a red train.
//! 3. **The machinery that moves it**, by name, with its last-fired
//!    time.
//!
//! NO NUMBER THIS SURFACE MAKES UP (the same bar as `crate::regions`).
//! A border whose flow cannot be computed — the registry behind one of
//! its rows unread — answers a rate with `null` halves and
//! `waiting: null`, and is TROUBLED. It never answers zero: a zero on a
//! map read at a glance is the false-empty class in its most visible
//! form. Nor is a machine's silence invented: only a machine that
//! DECLARES a heartbeat (a cadence rule) can be judged silent, and
//! `silent: null` means "cannot tell", never "fine".
//!
//! WHAT RECORDS A MACHINE'S FIRING, AND WHAT DOES NOT. A cadence rule
//! is measured: `cadence_firings` holds every firing with its instant
//! and exit code, which is what `yard::conductor_health` already reads.
//! The gate runner is event-driven and keeps no heartbeat, so its last
//! run (a gate-run opening or closing) is the stamp and silence is not
//! a reading. A dispatcher rule is measured too, since backlog
//! b14afc48: `dispatcher_firings` holds one row per firing, written by
//! the rules runner that fired it. What it does NOT hold is an expected
//! interval — an event-driven rule declares no heartbeat — so a
//! dispatcher machine answers `last_fired` and leaves `silent` null.
//! That distinction is the whole reason the table has no `every_minutes`
//! column: a rate invented for an event rule manufactures false silence
//! the first quiet hour.
//!
//! AN EVENT RULE IS STILL JUDGED — without an interval (c53f8f38).
//! `silent` stays null for it, and the trouble reading it DOES support
//! needs no heartbeat: work is waiting, and the machine has not fired
//! since the oldest of it arrived. [`judge`] asks that of the
//! gates → dock rail, whose stranded greens carry their own
//! arrival instant, and a rail with nothing waiting is never troubled
//! however long it has been quiet.
//!
//! THE BORDER SET IS DATA, HELD EQUAL TO THE CLIENT'S. [`BORDERS`]
//! below is the server's copy of the layout's borders; the client's is
//! `apps/web/src/it/yard/world.ts::BORDERS`, and `borders.test.ts`
//! parses this table out of this file and asserts the two are the same
//! set, in the same order (CLAUDE.md §9a — a fact that lives twice gets
//! an equality test).

use std::collections::BTreeSet;

use boss_core::job::Job;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::region_states::{BORDER_STILL, STILL_AFTER_GAPS, duration_text};
use crate::regions::{
    Instant, RegionInputs, RegionState, Trend, Windows, awaiting_proof, closed_at, count_split,
    find_step, marshalling_view, meta_instant, opened_at, parse_instant, plural, rate_trend,
    receiving_standing, released_awaiting_repair, shed_place, step_done_at, stuck_ids, taken_in_at,
};
use crate::stranded::is_train_gate;
use crate::yard::{SILENT_AFTER_INTERVALS, TrainBlock};

/// How many hold reasons a border carries. The `waiting` COUNT is
/// always the true one; this bounds only the list a hover shows, so a
/// hundred-deep queue does not become a hundred-line payload.
pub const MAX_HOLDS: usize = 8;

/// What moves packets across a border — and, decisively, what records
/// that it fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineKind {
    /// A cadence rule: `cadence_firings` records every firing, and the
    /// rule declares its own interval. The ONLY kind whose silence is a
    /// reading.
    Cadence,
    /// A dispatcher rule, fired by an event. `dispatcher_firings`
    /// records each firing; the rule declares no interval, so its
    /// silence is never a reading (b14afc48).
    DispatcherRule,
    /// The gate runner, which runs when a gate-run is filed. Its last
    /// run is the stamp; it declares no interval, so it cannot be
    /// silent.
    GateRunner,
    /// No machine: a human or an agent moves this hop. `last_crossed`
    /// is the only stamp there is.
    Actors,
}

impl MachineKind {
    fn as_str(self) -> &'static str {
        match self {
            MachineKind::Cadence => "cadence",
            MachineKind::DispatcherRule => "dispatcher-rule",
            MachineKind::GateRunner => "gate-runner",
            MachineKind::Actors => "actors",
        }
    }
}

/// One declared border: which two territories it joins, what ONE
/// crossing of it is, and the machinery that moves things across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderSpec {
    pub from: &'static str,
    pub to: &'static str,
    /// What one crossing IS, in words — so a reader of the number never
    /// has to go and derive what it counted.
    pub crossing: &'static str,
    /// The machine's name as an operator would say it: a cadence or
    /// dispatcher rule by its registry name, otherwise the thing.
    pub machine: &'static str,
    pub machine_kind: MachineKind,
}

/// The ten borders of the world layout: the line in flow order, then
/// the crossing out to the mirror and the garage's two feeders — the
/// same set and order as `apps/web/src/it/yard/world.ts::BORDERS`,
/// pinned equal by `borders.test.ts`.
///
/// THE LINE IS THE ORDER A CAR WALKS IT (design 62de32ae decision 3,
/// decided 2026-09-24): receiving -> marshalling -> shop-floor -> gates
/// -> dock -> track -> arrivals -> shed. A branch built on the shop
/// floor takes a bay (`shop-floor -> gates`), a green parks as a car
/// (`gates -> dock`), and the car boards a train (`dock -> track`).
/// Until then the line put the dock BEFORE the gates, and the three
/// rails' own crossings contradicted it: "a car parked on a green gate"
/// arrived at the dock from the shop floor, "a branch taking a bay"
/// left the DOCK, and the cars "held on the dock" stood at the
/// gates -> track border — so a car followed across the map walked
/// backwards through two regions (review of 2026-09-24, finding 3).
/// `the_line_chains_in_the_order_a_car_walks_it` pins the chain.
///
/// THE SHOP FLOOR SPLIT ONE HOP IN TWO (backlog 94c6ffd0). What used
/// to be `marshalling -> dock` counted the car parking and said
/// nothing about the interval before it: a packet was taken off a
/// station and a car appeared on the dock some hours later, with the
/// build — the part an actor actually spends the time on — drawn
/// nowhere. The two hops are the two events that were always
/// distinct: a run OPENED on the packet, and its car PARKED.
pub const BORDERS: [BorderSpec; 10] = [
    BorderSpec {
        from: "receiving",
        to: "marshalling",
        crossing: "an inbound packet taken in — its intake step completed",
        machine: "the receiving desk",
        machine_kind: MachineKind::Actors,
    },
    BorderSpec {
        from: "marshalling",
        to: "shop-floor",
        crossing: "a packet taken off a station — a run opened on it",
        machine: "boss dispatch",
        machine_kind: MachineKind::Actors,
    },
    BorderSpec {
        from: "shop-floor",
        to: "gates",
        crossing: "a gate-run opened — a branch taking a bay",
        machine: "the gate runner",
        machine_kind: MachineKind::GateRunner,
    },
    BorderSpec {
        from: "gates",
        to: "dock",
        crossing: "a green gate parked as a car on the dock",
        machine: "auto-park-on-gate-green",
        machine_kind: MachineKind::DispatcherRule,
    },
    BorderSpec {
        from: "dock",
        to: "track",
        crossing: "a car boarded a train",
        machine: "train-board-on-dock-depth",
        machine_kind: MachineKind::Cadence,
    },
    BorderSpec {
        from: "track",
        to: "arrivals",
        crossing: "a train arrived",
        machine: "train-reconcile",
        machine_kind: MachineKind::Cadence,
    },
    BorderSpec {
        from: "arrivals",
        to: "shed",
        crossing: "a landed car proven",
        machine: "boss prove",
        machine_kind: MachineKind::Actors,
    },
    // THE CROSSING OUT OF THE WORLD (design cb38d806, backlog
    // eee42416). What arrived on main is what a publish proposes to the
    // public mirror, so the publish region hangs off arrivals. One
    // crossing is one pull request opened — the machine that opens it
    // is the dispatcher rule, not the daily cadence that files the
    // packet: the cadence measures the drift every day and most days
    // close on `nothing-to-publish`, which is not a crossing.
    BorderSpec {
        from: "arrivals",
        to: "publish",
        crossing: "a snapshot of main proposed to the public mirror",
        machine: "publish-github-pr-on-open-pr-ready",
        machine_kind: MachineKind::DispatcherRule,
    },
    BorderSpec {
        from: "gates",
        to: "garage",
        crossing: "a gate-run judged red",
        machine: "the gate runner",
        machine_kind: MachineKind::GateRunner,
    },
    BorderSpec {
        from: "track",
        to: "garage",
        crossing: "a train cancelled with cars aboard",
        machine: "train-reconcile",
        machine_kind: MachineKind::Cadence,
    },
];

/// One packet standing at a border, and why it has not crossed. The
/// reason is the record's own — a hold marker's text, a queue position,
/// a train's block — never a sentence this module invents about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    /// What is waiting, as an operator names it: a branch, a train, a
    /// packet title.
    pub what: String,
    pub why: String,
}

/// ON WHOM what stands at a border is waiting — every packet `waiting`
/// counts, in exactly one class, so the four always sum to it (design
/// 31bade8f decision 8, car M1 on backlog d220022f). The map shades its
/// pile by these: solid for a machine, hollow for a person or the world,
/// grey for "cannot tell", red sediment for stuck.
///
/// WHY A FIELD AND NOT THE HOLDS' WORDS. The prototype read each hold's
/// sentence with a regex and extrapolated the unlisted rest from the
/// [`MAX_HOLDS`] it could see — a shade the record never said, for most
/// of a 235-deep pile. The server classifies every packet it counted,
/// from the same facts the hold sentence was written from.
///
/// `stuck` IS THE STUCK BLOCK'S OWN POPULATION (`regions::stuck_ids`),
/// not a second definition: across all borders the stuck class sums to
/// the regions payload's `stuck` block, so a rail's sediment and the
/// HUD's stuck number cannot disagree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldsByClass {
    /// In line for a machine: a bay, a train, a probe runner, a rule.
    pub machine: usize,
    /// Waiting on a person or the world: a triage nobody has taken, a
    /// merge, a repair, an event that has to happen.
    pub person: usize,
    /// The server cannot tell: a station whose flow the cube is blind
    /// to, a pull request GitHub was never asked about, a gate that
    /// never judged, an ordering edge nobody could read.
    pub unknown: usize,
    /// Stuck and ours ([`crate::regions::stuck`]).
    pub stuck: usize,
}

/// One packet's class, before it is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldClass {
    Machine,
    Person,
    Unknown,
    Stuck,
}

impl HoldsByClass {
    fn of(classes: impl IntoIterator<Item = HoldClass>) -> Self {
        classes
            .into_iter()
            .fold(HoldsByClass::default(), |c, class| match class {
                HoldClass::Machine => HoldsByClass {
                    machine: c.machine + 1,
                    ..c
                },
                HoldClass::Person => HoldsByClass {
                    person: c.person + 1,
                    ..c
                },
                HoldClass::Unknown => HoldsByClass {
                    unknown: c.unknown + 1,
                    ..c
                },
                HoldClass::Stuck => HoldsByClass {
                    stuck: c.stuck + 1,
                    ..c
                },
            })
    }

    fn total(self) -> usize {
        self.machine + self.person + self.unknown + self.stuck
    }
}

/// The machinery of one border, as its own record answers for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub kind: String,
    /// RFC3339, or `null` when nothing records this machine's firings —
    /// or when the record could not be read. `why` says which.
    pub last_fired: Option<String>,
    pub silent_for_minutes: Option<i64>,
    /// The machine's OWN declared heartbeat, from the cadence registry.
    /// `null` for a machine that declares none.
    pub expected_every_minutes: Option<i64>,
    /// `Some(true)` past [`SILENT_AFTER_INTERVALS`] of that heartbeat,
    /// `Some(false)` inside it, `null` when it cannot be told. Never
    /// `false` for "unknown": that is how a dead machine renders healthy.
    pub silent: Option<bool>,
    /// Where the last-fired time was read, or why there is none.
    pub why: String,
}

/// One border's rail, as the map draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Border {
    pub from: String,
    pub to: String,
    /// What one crossing is, in words ([`BorderSpec::crossing`]).
    pub crossing: String,
    /// Crossings per day, this window against the previous. Both halves
    /// `null` when the rows behind it could not be read.
    pub rate: Trend,
    /// The newest crossing's instant (RFC3339), `null` when nothing has
    /// crossed inside the two windows read.
    pub last_crossed: Option<String>,
    /// How many packets stand at the border now. `null` when the read
    /// that would answer it failed — never 0.
    pub waiting: Option<usize>,
    /// Up to [`MAX_HOLDS`] of them, each with its reason.
    pub holds: Vec<Hold>,
    /// All of them, by whom they wait on — summing to `waiting`, and
    /// `null` exactly when `waiting` is (design 31bade8f decision 8).
    pub holds_by_class: Option<HoldsByClass>,
    /// Is work crossing this rail? `false` once it has been quiet past
    /// [`STILL_AFTER_GAPS`] of its own mean gap, or nothing crossed in
    /// the window; `null` when its crossings could not be read — never
    /// `true` or `false` for "cannot tell" (design 31bade8f decision 8).
    /// It does not change `state`: stillness is drawn beside the state.
    pub flowing: Option<bool>,
    /// RFC3339: the record's own last crossing, when `flowing` is
    /// `false` — the instant the map's "held for" clock counts from, so
    /// no client ever invents one. `null` while flowing, and when nothing
    /// crossed in the whole read (the onset is not in the record read).
    pub held_since: Option<String>,
    /// The rule `flowing` was judged by, in words, with the numbers it
    /// was judged on.
    pub flowing_why: String,
    pub machine: Machine,
    pub state: RegionState,
    /// One sentence naming why the state is what it is.
    pub why: String,
}

/// The whole border set, in [`BORDERS`] order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Borders {
    pub window_hours: i64,
    pub borders: Vec<Border>,
}

/// One cadence rule's firing record, as the handler read it. An entry
/// PRESENT means the record was read: `fired_at: None` is "read, never
/// fired". A declared machine with no entry at all is a name the
/// cadence registry does not hold — which the border says, because a
/// machine name that has drifted is a defect, not a blank.
#[derive(Debug, Clone, PartialEq)]
pub struct CadenceFiring {
    pub rule: String,
    pub fired_at: Option<Instant>,
    /// The rule's declared interval, from its registry row.
    pub every_minutes: Option<i64>,
}

/// One dispatcher rule's firing record (`dispatcher_firings`, backlog
/// b14afc48). An entry PRESENT means the record was read for that rule:
/// `fired_at: None` is "read, never fired". A declared machine with no
/// entry at all is a rule the read did not cover, which the border says
/// rather than drawing as a quiet rail.
///
/// THERE IS NO `every_minutes` HERE, and that is the point. A dispatcher
/// rule fires on an event and declares no heartbeat, so it can be said
/// to have fired at T and never to be silent — an interval invented for
/// it would manufacture trouble the first quiet hour.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatcherFiring {
    pub rule: String,
    pub fired_at: Option<Instant>,
}

/// Everything [`borders`] reads: the same rows the regions map is built
/// from, plus the cadence firings. `firings: None` is the cadence record
/// unread — every cadence machine then says so rather than reading as
/// never-fired.
#[derive(Debug, Clone)]
pub struct BorderInputs<'a> {
    pub regions: &'a RegionInputs<'a>,
    pub firings: Option<&'a [CadenceFiring]>,
    /// The dispatcher rules' firings (`dispatcher_firings`).
    /// `dispatcher_firings: None` is that record unread — every
    /// dispatcher machine then says so rather than reading as
    /// never-fired.
    pub dispatcher_firings: Option<&'a [DispatcherFiring]>,
}

/// What one border's traffic came to: the rate, the newest crossing,
/// the queue and its reasons — or the read that could not be taken.
struct Flow {
    rate: Trend,
    last: Option<Instant>,
    /// Every packet standing at the border, by class. The border's
    /// `waiting` is their TOTAL, so the count and the pile's shades are
    /// one number and cannot drift apart. `None` when the queue could
    /// not be read.
    classes: Option<HoldsByClass>,
    holds: Vec<Hold>,
    /// Set when a row this border needs could not be read. The border
    /// is then troubled and this is its `why`.
    unread: Option<String>,
    /// The oldest packet standing at this border that the MACHINE is
    /// supposed to move, and when it arrived — what [`judge`] compares
    /// a last firing against (c53f8f38). `None` on every border that
    /// cannot answer it: nothing waiting for the machine, an arrival
    /// nobody stamped, or a hop whose waiting is not the machine's to
    /// clear. Never a guess: an invented arrival alarms on the first
    /// unstamped row, which is the failure this whole module refuses.
    oldest_waiting: Option<Oldest>,
}

/// The oldest thing waiting on a border's machine, with how many stand
/// with it — the two facts the trouble sentence needs.
#[derive(Debug, Clone, Copy)]
struct Oldest {
    n: usize,
    arrived: Instant,
}

fn unknown_rate() -> Trend {
    Trend {
        metric: "crossings".to_string(),
        unit: "per day".to_string(),
        current: None,
        previous: None,
        samples: 0,
        previous_samples: 0,
    }
}

impl Flow {
    /// A read that could not be taken: no rate, no queue, and the
    /// sentence that says which read failed.
    fn unread(why: &str) -> Self {
        Flow {
            rate: unknown_rate(),
            last: None,
            classes: None,
            holds: Vec::new(),
            unread: Some(why.to_string()),
            oldest_waiting: None,
        }
    }

    /// The rate and newest crossing were measured; the QUEUE was not.
    /// An unread queue is not an empty one, so it has no count and no
    /// classes, and the border is troubled with `why`.
    fn queue_unread(w: &Windows, stamps: Vec<Instant>, why: &str) -> Self {
        Flow {
            classes: None,
            unread: Some(why.to_string()),
            ..Flow::of(w, stamps, HoldsByClass::default(), Vec::new())
        }
    }

    fn waiting(&self) -> Option<usize> {
        self.classes.map(HoldsByClass::total)
    }

    /// The arrival this border's machine is judged against. Only a
    /// border that can source every candidate's instant calls it.
    fn waiting_since(mut self, oldest: Option<Oldest>) -> Self {
        self.oldest_waiting = oldest;
        self
    }

    fn of(w: &Windows, stamps: Vec<Instant>, classes: HoldsByClass, holds: Vec<Hold>) -> Self {
        let last = stamps.iter().copied().max();
        let (cur, prev) = count_split(w, stamps);
        Flow {
            rate: rate_trend("crossings", w, cur, prev),
            last,
            classes: Some(classes),
            holds: holds.into_iter().take(MAX_HOLDS).collect(),
            unread: None,
            oldest_waiting: None,
        }
    }
}

fn hold(what: &str, why: String) -> Hold {
    Hold {
        what: what.to_string(),
        why,
    }
}

/// A train's block in words — the same facts `TrainBlock` carries, said
/// once here so the border's hover and the yard's card agree.
fn block_words(b: &TrainBlock) -> String {
    match b {
        TrainBlock::DeployBlocked { reason, .. } => format!("deploy blocked — {reason}"),
        TrainBlock::CiRed { checks } => match checks {
            Some(c) => format!("CI red — {c}"),
            None => "CI red".to_string(),
        },
        TrainBlock::ConvergeOverdue => "the cluster has not converged past the policy".to_string(),
        TrainBlock::Stalled { since } => format!("stalled since {since}"),
    }
}

/// The metadata `outcome` a closed train carries.
fn outcome(j: &Job) -> &str {
    j.metadata
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The traffic over one border, from the rows the regions map already
/// holds. Every arm names the transition it counts; an undeclared
/// border answers UNREAD rather than an empty rail (a border nobody
/// taught this module about must not read as a quiet one).
///
/// EVERY ARM CLASSES WHAT IT COUNTS ([`HoldsByClass`]): a packet in the
/// stuck block's population (`stuck`) is stuck wherever it stands, and
/// every other packet is classed by what its own hold says it waits on.
#[allow(clippy::too_many_lines)]
fn flow_of(spec: &BorderSpec, r: &RegionInputs<'_>, w: &Windows, stuck: &BTreeSet<String>) -> Flow {
    let status = r.status;
    let class_of = |id: &str, otherwise: HoldClass| {
        if stuck.contains(id) {
            HoldClass::Stuck
        } else {
            otherwise
        }
    };
    match (spec.from, spec.to) {
        // inbound -> taken in. One crossing per packet whose intake
        // step completed, stamped when it completed (`taken_in_at`, the
        // same rule that draws the line between the two regions — design
        // 62de32ae decision 4). It counted inbound CLOSURES until
        // 2026-09-24, which is a different event on most kinds: a
        // user-feedback is taken in at triage and closes days later
        // after its build, and a packet withdrawn untriaged closes
        // without ever crossing. The handler's inbound read carries the
        // steps of the open rows and of those closed within two
        // windows, which covers every intake either window can hold.
        ("receiving", "marshalling") => {
            let Some(inbound) = r.inbound else {
                return Flow::unread(
                    "the workflow registry that names the inbound kinds could not be read",
                );
            };
            let stamps: Vec<Instant> = inbound.iter().filter_map(|(_, s)| taken_in_at(s)).collect();
            let today = r.now.date_naive();
            // What waits here is RECEIVING's own count — the packets not
            // yet taken in — and never a packet the next border also
            // counts (design 62de32ae decision 4): the two used to be
            // 325 and 321 of largely the same packets.
            let mut open: Vec<&Job> = receiving_standing(r).unwrap_or_default();
            open.sort_by_key(|j| j.opened_on);
            let holds = open
                .iter()
                .map(|j| {
                    let days = (today - j.opened_on).num_days();
                    hold(
                        &j.title,
                        format!(
                            "standing {} — nobody has taken it in",
                            plural(usize::try_from(days).unwrap_or(0), "day", "days",)
                        ),
                    )
                })
                .collect();
            // Nobody has taken it in: a person's triage — or, past the
            // triage band, stuck.
            let classes = HoldsByClass::of(
                open.iter()
                    .map(|j| class_of(&j.id.to_string(), HoldClass::Person)),
            );
            Flow::of(w, stamps, classes, holds)
        }
        // routed -> being built: an actor took a packet off a station
        // and a run opened on it (design c87fb59b car 2). One crossing
        // per `agent-run` filed. What waits is the marshalling yard
        // itself — the open packets standing at its stations, which is
        // what a dispatch takes one of — counted once across stations,
        // because a packet can match two predicates.
        ("marshalling", "shop-floor") => {
            let Some(runs) = r.agent_runs else {
                return Flow::unread("the agent-run packets could not be read");
            };
            let stamps: Vec<Instant> = runs.iter().filter_map(|(j, _)| opened_at(j)).collect();
            // MARSHALLING's own packets (design 62de32ae decision 4): a
            // station also holds untriaged intake and packets that stand
            // in a region of their own, and neither is waiting here.
            let stations = match marshalling_view(r) {
                Ok(view) => view,
                Err(why) => {
                    // The rate is still a measurement; the QUEUE is not,
                    // and an unread station registry is not an empty yard.
                    return Flow::queue_unread(w, stamps, why);
                }
            };
            let standing: std::collections::BTreeSet<&str> = stations
                .iter()
                .flat_map(|s| s.members.iter().map(String::as_str))
                .collect();
            let mut deep: Vec<&crate::regions::StationReading> =
                stations.iter().filter(|s| !s.members.is_empty()).collect();
            deep.sort_by_key(|s| std::cmp::Reverse(s.members.len()));
            let holds = deep
                .iter()
                .map(|s| {
                    hold(
                        &s.name,
                        format!(
                            "{} standing — nobody has taken one",
                            plural(s.members.len(), "packet", "packets")
                        ),
                    )
                })
                .collect();
            // Each distinct packet once, the worst of its stations
            // deciding: stuck (a station over its limit or not draining —
            // the stuck block's own predicate), then unknown (a station
            // within its limit whose flow the cube cannot see, the stuck
            // block's `unknown` arm), then a person to take it.
            let classes = HoldsByClass::of(standing.iter().map(|id| {
                let blind = stations.iter().any(|s| {
                    !s.over_limit && s.served.is_none() && s.members.iter().any(|m| m == *id)
                });
                class_of(
                    id,
                    if blind {
                        HoldClass::Unknown
                    } else {
                        HoldClass::Person
                    },
                )
            }));
            Flow::of(w, stamps, classes, holds)
        }
        // being built -> gated: a branch built on the shop floor taking
        // a bay. What waits is the line for a slot, in its own order.
        // A TRAIN's gate (design 128b5496, `is_train_gate`) runs in the
        // same bays, but its branch came off the track, not the shop
        // floor, so it is neither a crossing of this rail nor a packet
        // waiting at it — the track shows it as the train at its gate.
        ("shop-floor", "gates") => {
            let stamps: Vec<Instant> = r
                .gate_runs
                .iter()
                .filter(|g| !is_train_gate(&g.metadata))
                .filter_map(|g| meta_instant(&g.metadata, "opened_at").or_else(|| opened_at(g)))
                .collect();
            let capacity = status.gates.capacity;
            let queued: Vec<&crate::yard::QueuedGate> = status
                .gates
                .queued
                .iter()
                .filter(|q| q.train.is_none())
                .collect();
            let holds = queued
                .iter()
                .map(|q| {
                    hold(
                        &q.branch,
                        format!("{} in line for one of {capacity} bays", ordinal(q.position)),
                    )
                })
                .collect();
            // In line for a bay.
            let classes = HoldsByClass::of(
                queued
                    .iter()
                    .map(|q| class_of(&q.packet_id, HoldClass::Machine)),
            );
            Flow::of(w, stamps, classes, holds)
        }
        // gated green -> parked: the green became a car standing on the
        // dock, which is the car's `gate` (park) step completing. What
        // waits is a green that never became a car — held on purpose,
        // with its reason, or stranded, which is a fix about to be
        // rebuilt blind.
        ("gates", "dock") => {
            let stamps: Vec<Instant> = r
                .cars
                .iter()
                .filter_map(|(_, s)| {
                    step_done_at(find_step(s, crate::car::GATE_SLUG, crate::car::GATE))
                })
                .collect();
            let mut holds: Vec<Hold> = status
                .held
                .iter()
                .map(|h| hold(&h.branch, format!("held off the dock — {}", h.reason)))
                .collect();
            holds.extend(status.stranded.iter().map(|s| {
                hold(
                    &s.branch,
                    "green, and no car claims it — rescue it, never rebuild it blind".to_string(),
                )
            }));
            // A held green is a brake someone here set (the stuck block
            // counts it); a stranded one is what this hop's rule owes a
            // car, which is how the judge below reads it (c53f8f38).
            let classes = HoldsByClass::of(
                status
                    .held
                    .iter()
                    .map(|h| class_of(&h.packet_id, HoldClass::Person))
                    .chain(
                        status
                            .stranded
                            .iter()
                            .map(|s| class_of(&s.packet_id, HoldClass::Machine)),
                    ),
            );
            // WHAT THIS HOP'S MACHINE IS JUDGED AGAINST (c53f8f38). Only
            // the STRANDED greens: a held green is a brake an operator
            // put on, and `auto-park-on-gate-green` was never going to
            // file a car for it, so its silence about one says nothing
            // (`crate::stranded`). The arrival is the gate-run's own
            // `since` — `boss gate`'s `opened_at` stamp, as
            // `yard::opened_since` read it — and it is EARLIER than the
            // green it gated, which makes this test strictly
            // conservative. A `since` that is a bare date (the row
            // carried no stamp: 6c2eba00) does not parse, and one
            // unparseable candidate refuses the whole reading, because
            // the oldest might be the one that could not be read.
            let arrivals: Option<Vec<Instant>> = status
                .stranded
                .iter()
                .map(|s| parse_instant(&s.since))
                .collect();
            let oldest = arrivals.and_then(|a| {
                a.iter().copied().min().map(|arrived| Oldest {
                    n: a.len(),
                    arrived,
                })
            });
            Flow::of(w, stamps, classes, holds).waiting_since(oldest)
        }
        // parked -> boarded: one crossing per CAR the train collected,
        // stamped at the train's `collect`. What waits is the dock — the
        // cars ready to board, and the ones an operator has braked.
        ("dock", "track") => {
            let mut stamps: Vec<Instant> = Vec::new();
            for (j, steps) in r.open_trains.iter().chain(r.closed_trains.iter()) {
                let Some(collect) = step_done_at(find_step(
                    steps,
                    "collect",
                    "Collect what is ready to board",
                )) else {
                    continue;
                };
                let aboard = j
                    .metadata
                    .get("boarded_jobs")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                stamps.extend(std::iter::repeat_n(collect, aboard));
            }
            if matches!(r.dock_reading, crate::yard::Reading::Unread) {
                // The rate is still a measurement; the QUEUE is not, and
                // an unread dock is not an empty one (52fed017).
                return Flow::queue_unread(
                    w,
                    stamps,
                    "the loading-dock station row could not be read",
                );
            }
            let mut holds: Vec<Hold> = status
                .held_cars
                .iter()
                .map(|h| {
                    hold(
                        h.car.branch.as_deref().unwrap_or(h.car.title.as_str()),
                        format!("held on the dock — {}", h.reason),
                    )
                })
                .collect();
            let waiting_for = match status.boarding.threshold_met {
                Some(true) => "parked — the boarding depth is met, a train is due".to_string(),
                _ => "parked, waiting for the boarding depth or the clock rule".to_string(),
            };
            // A car the conductor will refuse on its declared ordering
            // edge is listed with the conductor's own refusal, not as a
            // train due (backlog 4142d821) — judged by the one function
            // both read, through the dock region's own reading.
            let edges = crate::regions::dock_edges(r);
            let edge_holds: Vec<(&str, String)> = edges
                .iter()
                .filter_map(|(d, o)| match o {
                    crate::car::EdgeOutcome::Hold(h) => Some((d.id.as_str(), h.reason.clone())),
                    _ => None,
                })
                .collect();
            // A parked car is in line for a train — behind a predecessor
            // still in flight too, which boards by itself when that one
            // lands. An edge that can never clear, and a car braked on
            // the dock by hand, are the stuck block's; an edge nobody
            // could read is unknown (the conductor boards it anyway).
            let unjudged = |id: &str| {
                edges.iter().any(|(d, o)| {
                    d.id == id && matches!(o, crate::car::EdgeOutcome::BoardUnjudged(_))
                })
            };
            let classes = HoldsByClass::of(
                status
                    .held_cars
                    .iter()
                    .map(|h| class_of(&h.car.id, HoldClass::Person))
                    .chain(status.dock.iter().map(|c| {
                        class_of(
                            &c.id,
                            if unjudged(&c.id) {
                                HoldClass::Unknown
                            } else {
                                HoldClass::Machine
                            },
                        )
                    })),
            );
            holds.extend(status.dock.iter().map(|c| {
                let why = edge_holds.iter().find(|(id, _)| *id == c.id).map_or_else(
                    || waiting_for.clone(),
                    |(_, reason)| format!("cannot board — {reason}"),
                );
                hold(c.branch.as_deref().unwrap_or(c.title.as_str()), why)
            }));
            Flow::of(w, stamps, classes, holds)
        }
        // boarded -> arrived. What waits is every train in transit, each
        // with the yard's own block when it has one.
        ("track", "arrivals") => {
            let stamps: Vec<Instant> = r
                .closed_trains
                .iter()
                .filter(|(j, _)| outcome(j) == "arrived")
                .filter_map(|(j, _)| closed_at(j))
                .collect();
            let holds = status
                .trains
                .iter()
                .map(|t| {
                    let why = match t.block.as_ref() {
                        Some(b) => block_words(b),
                        None => match t.at_step.as_deref() {
                            Some(step) => format!("in transit, at {step}"),
                            None => "in transit".to_string(),
                        },
                    };
                    hold(&t.title, why)
                })
                .collect();
            // A train in transit is the conductor's to move. A BLOCKED
            // one is not in a queue any more: the machine refused it or
            // ran past its own policy, and what moves it next is a
            // person's repair or the world (a red CI, a deploy refused, a
            // converge overdue) — the block rides its hold in words.
            let classes = HoldsByClass::of(status.trains.iter().map(|t| {
                class_of(
                    &t.id,
                    if t.block.is_some() {
                        HoldClass::Person
                    } else {
                        HoldClass::Machine
                    },
                )
            }));
            Flow::of(w, stamps, classes, holds)
        }
        // arrived -> proven. What waits is the inspection shed, each car
        // with WHERE it stands (`regions::shed_place`) — a probe
        // pending, a probe that said not yet, an event to wait on, or
        // nothing mechanical at all.
        ("arrivals", "shed") => {
            let stamps: Vec<Instant> = r
                .cars
                .iter()
                .filter_map(|(_, s)| step_done_at(find_step(s, "proven", "Proven in production")))
                .collect();
            let awaiting = awaiting_proof(r.cars);
            let holds = awaiting
                .iter()
                .map(|(j, _)| {
                    let branch = j
                        .metadata
                        .get("branch")
                        .and_then(Value::as_str)
                        .unwrap_or(j.title.as_str());
                    hold(branch, shed_words(&j.metadata))
                })
                .collect();
            // A probe recorded and not yet run waits on the probe runner;
            // every other place waits on a person or the world — a
            // failing probe on its fix, a not-yet on the event it names,
            // an unproven car on someone to say how it is proven. Past
            // the proof band and ours, the stuck block's.
            let classes = HoldsByClass::of(awaiting.iter().map(|(j, _)| {
                class_of(
                    &j.id.to_string(),
                    match shed_place(&j.metadata) {
                        crate::regions::ShedPlace::ProbePending { last: None } => {
                            HoldClass::Machine
                        }
                        _ => HoldClass::Person,
                    },
                )
            }));
            Flow::of(w, stamps, classes, holds)
        }
        // A red gate sends a car to the garage. What stands AT this
        // border is a run the gate never judged: "we do not know" is not
        // a verdict, and nothing moves it on its own.
        ("gates", "garage") => {
            // A red TRAIN gate strikes the train and the cars aboard it,
            // which the track -> garage rail counts; it sends no car of
            // its own here (the garage lane makes the same exclusion).
            let stamps: Vec<Instant> = r
                .gate_runs
                .iter()
                .filter(|g| !is_train_gate(&g.metadata))
                .filter(|g| g.metadata.get("outcome").and_then(Value::as_str) == Some("failed"))
                .filter_map(closed_at)
                .collect();
            let holds = status
                .limbo
                .iter()
                .map(|l| {
                    hold(
                        &l.branch,
                        format!(
                            "the gate never judged it ({}) — nothing will move it",
                            l.verdict
                        ),
                    )
                })
                .collect();
            // A run the gate never judged: whether it passed cannot be
            // told.
            let classes = HoldsByClass::of(status.limbo.iter().map(|_| HoldClass::Unknown));
            Flow::of(w, stamps, classes, holds)
        }
        // A red train releases its cars. What stands at the border is
        // the cars it released that are still awaiting repair — the same
        // predicate the arrivals region is troubled by.
        ("track", "garage") => {
            let stamps: Vec<Instant> = r
                .closed_trains
                .iter()
                .filter(|(j, _)| outcome(j) != "arrived")
                .filter(|(j, _)| {
                    j.metadata
                        .get("boarded_jobs")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty())
                })
                .filter_map(|(j, _)| closed_at(j))
                .collect();
            let released = released_awaiting_repair(r.closed_trains, r.cars, w);
            let mut holds = Vec::new();
            // Each released car waits on its repair — a person's (or an
            // agent's) re-gate.
            let mut repairs = Vec::new();
            for (train, cars) in released {
                repairs.extend(cars.iter().map(|_| HoldClass::Person));
                holds.extend(cars.into_iter().map(|c| {
                    hold(
                        c,
                        format!("released by {train}, not re-gated and not re-boarded"),
                    )
                }));
            }
            Flow::of(w, stamps, HoldsByClass::of(repairs), holds)
        }
        // A snapshot of main proposed to the public mirror (design
        // cb38d806). One crossing per pull request OPENED, which is the
        // `open-pr` step completing. What stands at the border is every
        // PR the record still shows open, each with its own reason for
        // standing there: a reading nobody judged, or simply a merge
        // nobody has made — the second is a human gate by decision
        // (David, 2026-09-19: the merge stays his hand act), so the
        // hold says whose it is rather than implying a fault.
        ("arrivals", "publish") => {
            let Some(packets) = r.publish_packets else {
                return Flow::unread("the publish-to-github packets could not be read");
            };
            let prs = crate::regions::publish_prs(packets);
            let stamps: Vec<Instant> = prs.iter().map(|p| p.opened).collect();
            let open: Vec<&crate::regions::PublishPr> =
                prs.iter().filter(|p| p.awaiting()).collect();
            let holds = open
                .iter()
                .map(|p| {
                    hold(
                        &p.url,
                        if p.unjudged_red() {
                            p.reading()
                        } else if p.state_read_at.is_empty() {
                            // Unasked is not open (backlog a5d4322c).
                            "its state was never read from GitHub".to_string()
                        } else {
                            "open on the mirror — the merge is a person's".to_string()
                        },
                    )
                })
                .collect();
            // No `waiting_since`: what stands here waits on a PERSON to
            // merge, not on this rule, and judging the rule against it
            // would blame the machine for a gate the design made
            // David's on purpose.
            //
            // A red reading nobody judged waits on a person to judge it;
            // a PR GitHub was never asked about is unknown — an unasked
            // question is not "open" (a5d4322c); one read open waits on
            // the person whose merge it is. The same order the hold's
            // sentence is chosen in above.
            let classes = HoldsByClass::of(open.iter().map(|p| {
                if !p.unjudged_red() && p.state_read_at.is_empty() {
                    HoldClass::Unknown
                } else {
                    HoldClass::Person
                }
            }));
            Flow::of(w, stamps, classes, holds)
        }
        _ => Flow::unread("no crossing is declared for this border"),
    }
}

/// `1st in line`, `2nd`, … — the queue position as an operator reads it.
fn ordinal(n: i32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Where a car in the shed stands, in the words its place already has
/// ([`crate::regions::shed_place`]).
fn shed_words(md: &Value) -> String {
    match shed_place(md) {
        crate::regions::ShedPlace::ProbePending { last: Some(why) } => {
            format!("its probe is FAILING — {why}")
        }
        crate::regions::ShedPlace::ProbePending { last: None } => {
            "a probe is recorded and has not run yet".to_string()
        }
        crate::regions::ShedPlace::ProbeNotYet { said } => {
            format!("the probe said not yet — {said}")
        }
        crate::regions::ShedPlace::WaitingOn(event) => format!("waiting on {event}"),
        crate::regions::ShedPlace::Unproven => {
            "UNPROVEN — no probe, no event: nothing mechanical can settle it".to_string()
        }
    }
}

/// The machine's own record, read where one exists and refused where it
/// does not. Every branch says where the answer came from, because a
/// last-fired time nobody can source is the thing this surface exists
/// to stop.
fn machine_of(spec: &BorderSpec, inputs: &BorderInputs<'_>, now: Instant) -> Machine {
    let r = inputs.regions;
    let (last, every, why) = match spec.machine_kind {
        MachineKind::Cadence => match inputs.firings {
            None => (
                None,
                None,
                "the cadence firing record could not be read".to_string(),
            ),
            Some(firings) => match firings.iter().find(|f| f.rule == spec.machine) {
                None => (
                    None,
                    None,
                    format!("the cadence registry holds no rule named {}", spec.machine),
                ),
                Some(f) => (
                    f.fired_at,
                    f.every_minutes,
                    match f.fired_at {
                        Some(_) => "its own firing in cadence_firings".to_string(),
                        None => format!("{} has never fired", spec.machine),
                    },
                ),
            },
        },
        // The runner is event-driven: it runs when a run is filed, so
        // its last run is the stamp and there is no interval to be
        // silent against.
        MachineKind::GateRunner => {
            let last = r
                .gate_runs
                .iter()
                .filter_map(|g| closed_at(g).or_else(|| meta_instant(&g.metadata, "opened_at")))
                .max();
            (
                last,
                None,
                "its last gate-run; the runner is event-driven and declares no cadence".to_string(),
            )
        }
        // `dispatcher_firings` holds one row per firing (b14afc48).
        // The interval half stays None on every branch: an event-driven
        // rule declares no heartbeat, so `silent` below can only be
        // null for it — never false, which is how a dead machine
        // renders healthy.
        MachineKind::DispatcherRule => match inputs.dispatcher_firings {
            None => (
                None,
                None,
                "the dispatcher firing record could not be read".to_string(),
            ),
            Some(firings) => match firings.iter().find(|f| f.rule == spec.machine) {
                None => (
                    None,
                    None,
                    format!("no firing record was read for {}", spec.machine),
                ),
                Some(f) => (
                    f.fired_at,
                    None,
                    match f.fired_at {
                        Some(_) => "its own firing in dispatcher_firings; an event rule \
                             declares no interval, so its silence is not a reading"
                            .to_string(),
                        None => format!(
                            "{} has never fired — dispatcher_firings holds no row for it",
                            spec.machine
                        ),
                    },
                ),
            },
        },
        MachineKind::Actors => (
            None,
            None,
            "no machine moves this hop — an actor does; the last crossing is the stamp".to_string(),
        ),
    };
    let silent_for = last.map(|t| (now - t).num_minutes().max(0));
    let silent = match (silent_for, every) {
        (Some(mins), Some(every)) if every > 0 => Some(mins > every * SILENT_AFTER_INTERVALS),
        _ => None,
    };
    Machine {
        name: spec.machine.to_string(),
        kind: spec.machine_kind.as_str().to_string(),
        last_fired: last.map(|t| t.to_rfc3339()),
        silent_for_minutes: silent_for,
        expected_every_minutes: every,
        silent,
        why,
    }
}

/// The map's rails. Pure: rows in, one border per declared rail out,
/// in [`BORDERS`] order.
pub fn borders(inputs: &BorderInputs<'_>) -> Borders {
    let r = inputs.regions;
    let w = Windows::of(r.now, r.window_hours);
    let stuck = stuck_ids(r);
    Borders {
        window_hours: r.window_hours,
        borders: BORDERS
            .iter()
            .map(|spec| {
                let flow = flow_of(spec, r, &w, &stuck);
                let machine = machine_of(spec, inputs, r.now);
                let (state, why) = judge(spec, &flow, &machine, &w);
                let motion = motion(&flow, &w, r.now);
                Border {
                    from: spec.from.to_string(),
                    to: spec.to.to_string(),
                    crossing: spec.crossing.to_string(),
                    last_crossed: flow.last.map(|t| t.to_rfc3339()),
                    waiting: flow.waiting(),
                    holds_by_class: flow.classes,
                    flowing: motion.flowing,
                    held_since: motion.held_since.map(|t| t.to_rfc3339()),
                    flowing_why: motion.why,
                    rate: flow.rate,
                    holds: flow.holds,
                    machine,
                    state,
                    why,
                }
            })
            .collect(),
    }
}

/// Whether a rail is flowing, and since when it has been held.
struct Motion {
    flowing: Option<bool>,
    held_since: Option<Instant>,
    why: String,
}

/// IS WORK CROSSING THIS RAIL? (design 31bade8f decision 8, car M1).
/// Judged ON THE SERVER, from the rail's own record, so no client ever
/// decides that a rail has stalled — `borders.ts` already says "nothing
/// here derives a judgement", and the map's stillness is drawn off this.
///
/// THE EXPECTED INTERVAL IS THE RAIL'S OWN MEAN GAP — the window over
/// its crossings in it — and the rail is still past [`STILL_AFTER_GAPS`]
/// of those ([`BORDER_STILL`]). The design offered a machine's declared
/// cadence as the interval where one exists, and it is not one: the
/// cadence is how often the MACHINE runs, not how often the rail is
/// crossed. `train-reconcile` fires every 10 minutes and moves the
/// track -> garage rail about 4 times a day (live, 2026-09-24), so four
/// of its intervals would call that rail still for most of every day.
/// The machine's heartbeat is already judged, as `machine.silent`.
///
/// NULL IS "CANNOT TELL": a rail whose crossings could not be read has
/// no rate to judge against. A rail nothing crossed in the window is
/// still — a plain fact about motion — held since the last crossing the
/// read holds, or with no onset when it holds none.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn motion(flow: &Flow, w: &Windows, now: Instant) -> Motion {
    if flow.rate.current.is_none() {
        return Motion {
            flowing: None,
            held_since: None,
            why: "its crossings could not be read, so whether it flows cannot be told".to_string(),
        };
    }
    let n = flow.rate.samples;
    let Some(last) = flow.last.filter(|_| n > 0) else {
        return Motion {
            flowing: Some(false),
            held_since: flow.last,
            why: match flow.last {
                Some(last) => format!(
                    "nothing crossed in {}h; the last crossing read was {}",
                    w.hours,
                    last.to_rfc3339()
                ),
                None => format!("nothing crossed in the {}h read", w.hours * 2),
            },
        };
    };
    let gap = (w.hours * 60) as f64 / n as f64;
    let quiet = (now - last).num_minutes().max(0);
    let gap_text = duration_text(gap.ceil() as i64);
    let measured = format!(
        "a mean gap of {gap_text}: {} in {}h",
        plural(n, "crossing", "crossings"),
        w.hours
    );
    if quiet as f64 > STILL_AFTER_GAPS as f64 * gap {
        Motion {
            flowing: Some(false),
            held_since: Some(last),
            why: format!(
                "quiet {} > {} ({measured})",
                duration_text(quiet),
                BORDER_STILL.band
            ),
        }
    } else {
        Motion {
            flowing: Some(true),
            held_since: None,
            why: format!(
                "last crossed {} ago, inside {STILL_AFTER_GAPS}× its mean gap ({measured})",
                duration_text(quiet)
            ),
        }
    }
}

/// How a border reads at a glance. Troubled: a read that could not be
/// taken (refused like a failure, never drawn as a quiet rail), or
/// traffic waiting while the machine that moves it has been silent past
/// its own declared cadence — the shape the whole design is for — or,
/// for an event rule that declares no cadence, traffic that arrived
/// before the machine's last firing (c53f8f38). Clear: everything else,
/// a queue moving on a live machine included — its size is the rail's
/// `waiting`, not its colour.
fn judge(spec: &BorderSpec, flow: &Flow, machine: &Machine, w: &Windows) -> (RegionState, String) {
    if let Some(unread) = flow.unread.as_deref() {
        return (RegionState::Troubled, unread.to_string());
    }
    let waiting = flow.waiting().unwrap_or(0);
    if waiting > 0 && machine.silent == Some(true) {
        let silent = machine.silent_for_minutes.unwrap_or_default();
        let every = machine.expected_every_minutes.unwrap_or_default();
        return (
            RegionState::Troubled,
            format!(
                "{} waiting and {} silent for {silent}m — it declares every {every}m",
                plural(waiting, "packet", "packets"),
                machine.name
            ),
        );
    }
    // THE EVENT-DRIVEN FORM OF THE SAME QUESTION (c53f8f38, named by
    // the builder of b14afc48). A cadence machine is judged above
    // against the heartbeat it DECLARES; an event rule declares none,
    // and an interval invented for it manufactures false silence the
    // first quiet hour. The question that needs no interval: has the
    // machine fired since the oldest thing waiting on it ARRIVED? A hop
    // with nothing waiting is never troubled however long it has been
    // quiet, which is why this reads the queue first.
    //
    // ASKED ONLY OF A DISPATCHER RULE, deliberately. The gate runner's
    // stamp is its last RUN rather than its last decision, so the same
    // comparison would read a full set of bays — a queue that is
    // capacity, not a stall — as a stopped machine. A machine that has
    // NEVER fired is out of scope too: `last_fired` is null both for
    // that and for a firing record nobody could read, and this surface
    // does not guess between them.
    if spec.machine_kind == MachineKind::DispatcherRule
        && let Some(oldest) = flow.oldest_waiting
        && let Some(fired) = machine.last_fired.as_deref().and_then(parse_instant)
        && fired < oldest.arrived
    {
        return (
            RegionState::Troubled,
            format!(
                "{} waiting since {} and {} has not fired since — its last firing was {}",
                plural(oldest.n, "packet", "packets"),
                oldest.arrived.to_rfc3339(),
                machine.name,
                fired.to_rfc3339(),
            ),
        );
    }
    // TRAFFIC WAITING ON A LIVE MACHINE IS A RAIL WORKING — clear, with
    // the queue named (design 62de32ae, decision 1: one vocabulary
    // everywhere, and clear includes busy-and-healthy). It was `busy`
    // until 2026-09-24, which painted every rail with anything on it the
    // same amber as a saturated one; the waiting count rides the rail
    // either way.
    if waiting > 0 {
        let first = flow
            .holds
            .first()
            .map(|h| format!("{} ({})", h.what, h.why))
            .unwrap_or_else(|| spec.crossing.to_string());
        return (
            RegionState::Clear,
            format!(
                "{} waiting to cross — {first}",
                plural(waiting, "packet", "packets")
            ),
        );
    }
    let crossed = flow.rate.samples;
    (
        RegionState::Clear,
        format!(
            "nothing waiting; {} in {}h",
            plural(crossed, "crossing", "crossings"),
            w.hours
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::{DEFAULT_WINDOW_HOURS, RegionInputs, StationReading};
    use crate::yard::{BoardingReadings, Reading, YardInputs, YardStatus, build_status_for};
    use boss_core::job::{JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
    use serde_json::json;

    const NOW: &str = "2026-09-19T12:00:00Z";

    fn t(s: &str) -> Instant {
        crate::regions::parse_instant(s).unwrap()
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

    /// One publish packet, shaped as the live ones are: the pull
    /// request, its snapshot and the mirror head it was built on ride
    /// `open-pr`; the scan reading rides `read-checks`.
    fn publish_packet(
        pr: &str,
        snapshot: &str,
        mirror_head: &str,
        opened_at: &str,
        reading: (&str, &str, &str),
    ) -> (Job, Vec<Step>) {
        let j = job(
            crate::regions::PUBLISH_KIND,
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
        let mut checks = step(&j, "read-checks", StepStatus::Completed, Some(opened_at));
        checks.metadata =
            json!({ "conclusion": reading.0, "alerts": reading.1, "rules": reading.2 });
        (j, vec![open_pr, checks])
    }

    /// The map's inputs with nothing in them — every read answered.
    fn region_inputs<'a>(
        status: &'a YardStatus,
        cars: &'a [(Job, Vec<Step>)],
        closed_trains: &'a [(Job, Vec<Step>)],
        gate_runs: &'a [Job],
        inbound: Option<&'a [(Job, Vec<Step>)]>,
        stations: Option<&'a [StationReading]>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            status,
            dock_reading: Reading::Read,
            open_trains: &[],
            closed_trains,
            cars,
            gate_runs,
            inbound,
            stations,
            // Borders are drawn from the flows between regions; the
            // machinery a region runs (car 5) says nothing about an
            // edge, so these fixtures read it as unread.
            conductor: None,
            ops_requests: None,
            runner_hosts: None,
            agent_runs: None,
            sessions: None,
            publish_packets: None,
            run_capacity: None,
            predecessors: &[],
            now: t(NOW),
            window_hours: DEFAULT_WINDOW_HOURS,
        }
    }

    fn only<'a>(borders: &'a Borders, from: &str, to: &str) -> &'a Border {
        borders
            .borders
            .iter()
            .find(|b| b.from == from && b.to == to)
            .expect("the border is declared")
    }

    #[test]
    fn every_declared_border_is_answered_once_in_layout_order() {
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let pairs: Vec<(String, String)> = out
            .borders
            .iter()
            .map(|b| (b.from.clone(), b.to.clone()))
            .collect();
        let declared: Vec<(String, String)> = BORDERS
            .iter()
            .map(|s| (s.from.to_string(), s.to.to_string()))
            .collect();
        assert_eq!(pairs, declared);
        assert_eq!(out.window_hours, DEFAULT_WINDOW_HOURS);
        // Every border says what one crossing of it IS.
        assert!(out.borders.iter().all(|b| !b.crossing.is_empty()));
    }

    #[test]
    fn an_unreadable_inbound_registry_is_unknown_and_troubled_never_zero() {
        let status = empty_status();
        // inbound: None — the workflow registry could not be read.
        let inputs = region_inputs(&status, &[], &[], &[], None, Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "receiving", "marshalling");
        assert_eq!(b.state, RegionState::Troubled);
        assert_eq!(b.waiting, None, "an unread queue is not an empty one");
        assert_eq!(b.rate.current, None, "a rate nobody measured is not zero");
        assert_eq!(b.rate.previous, None);
        assert!(b.why.contains("workflow registry"), "why: {}", b.why);
    }

    /// THE CROSSING OUT OF THE WORLD (design cb38d806). One pull
    /// request opened is one crossing; what stands at the border is
    /// every PR the record still shows open, and each hold says WHY it
    /// stands — the reading when nobody judged it, the human gate when
    /// that is all there is. An unread packet list is troubled, never a
    /// quiet rail.
    #[test]
    fn a_pull_request_on_the_mirror_is_one_crossing_of_the_publish_border() {
        let status = empty_status();
        // What GitHub answered when `--measure` asked (backlog
        // a5d4322c): #240 merged, #241 still open.
        let asked = |(mut j, steps): (Job, Vec<Step>), pr: &str, state: &str| {
            j.metadata = json!({ "pr_state": {
                "pr_url": pr, "state": state, "merged": state == "closed",
                "read_at": "2026-09-19T11:00:00Z",
            }});
            (j, steps)
        };
        let publish = vec![
            asked(
                publish_packet(
                    "https://mirror/pull/240",
                    "snap-240",
                    "mirror-a",
                    "2026-09-19T09:00:00Z",
                    ("failure", "109", "14"),
                ),
                "https://mirror/pull/240",
                "closed",
            ),
            asked(
                publish_packet(
                    "https://mirror/pull/241",
                    "snap-241",
                    "snap-240",
                    "2026-09-19T10:00:00Z",
                    ("success", "0", "0"),
                ),
                "https://mirror/pull/241",
                "open",
            ),
        ];
        let mut inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        inputs.publish_packets = Some(&publish);
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "arrivals", "publish");
        // Two PRs opened in the window; GitHub said #240 merged, so
        // only #241 stands.
        assert_eq!(b.rate.samples, 2);
        assert_eq!(b.waiting, Some(1));
        assert_eq!(b.holds.len(), 1);
        assert_eq!(b.holds[0].what, "https://mirror/pull/241");
        assert!(
            b.holds[0].why.contains("the merge is a person's"),
            "why: {}",
            b.holds[0].why
        );

        // Unread: no rate, no queue, and the sentence names the read.
        let mut blind = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        blind.publish_packets = None;
        let out = borders(&BorderInputs {
            regions: &blind,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "arrivals", "publish");
        assert_eq!(b.state, RegionState::Troubled);
        assert_eq!(b.waiting, None);
        assert!(b.why.contains("publish-to-github"), "why: {}", b.why);
    }

    /// A PR whose reading nobody judged carries THE READING at the
    /// border too — the hold is what the scan said, not that something
    /// is wrong (a verdict must name what failed).
    #[test]
    fn an_unjudged_red_reading_is_the_publish_borders_hold_reason() {
        let status = empty_status();
        let publish = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-19T09:00:00Z",
            ("failure", "109", "14"),
        )];
        let mut inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        inputs.publish_packets = Some(&publish);
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "arrivals", "publish");
        assert_eq!(b.holds.len(), 1);
        assert!(
            b.holds[0].why.contains("failure")
                && b.holds[0].why.contains("109")
                && b.holds[0].why.contains("14"),
            "why: {}",
            b.holds[0].why
        );
    }

    #[test]
    fn an_unread_dock_leaves_the_boarding_border_unknown() {
        let status = empty_status();
        let mut inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        inputs.dock_reading = Reading::Unread;
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "dock", "track");
        assert_eq!(b.state, RegionState::Troubled);
        assert_eq!(b.waiting, None);
        assert!(b.why.contains("loading-dock"), "why: {}", b.why);
    }

    /// The dock region's per-car half (backlog 4142d821): a car held on
    /// its declared ordering edge is listed with the conductor's own
    /// refusal, not as "a train is due" beside the cars that can board.
    #[test]
    fn a_car_held_on_its_edge_is_listed_with_the_conductors_own_words() {
        let parked = |branch: &str, md: Value| {
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![step(&j, crate::car::REVIEW_SLUG, StepStatus::Ready, None)];
            (j, s)
        };
        let first = parked("fix/first-half", json!({ "branch": "fix/first-half" }));
        let pred_id = first.0.id.to_string();
        let second = parked(
            "fix/second-half",
            json!({ "branch": "fix/second-half", "boards_after": pred_id }),
        );
        let cars = vec![first.clone(), second];
        let mut status = empty_status();
        status.dock = cars.iter().map(|(j, _)| crate::yard::dock_car(j)).collect();
        status.boarding.threshold_met = Some(true);
        let preds = vec![(
            pred_id,
            crate::car::Predecessor::Found(crate::regions::packet_value(&first.0, &first.1)),
        )];
        let mut inputs = region_inputs(&status, &cars, &[], &[], Some(&[]), Some(&[]));
        inputs.predecessors = &preds;
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "dock", "track");
        let why_of = |what: &str| {
            b.holds
                .iter()
                .find(|h| h.what == what)
                .map(|h| h.why.clone())
                .unwrap_or_else(|| panic!("no hold for {what}: {:?}", b.holds))
        };
        let held = why_of("fix/second-half");
        assert!(
            held.starts_with("cannot board — boards after fix/first-half")
                && held.contains("STILL IN FLIGHT"),
            "{held}"
        );
        assert!(!held.contains("a train is due"), "{held}");
        assert!(
            why_of("fix/first-half").contains("a train is due"),
            "the car that CAN board is still due"
        );
    }

    #[test]
    fn a_car_parking_is_one_crossing_of_the_gates_dock_border() {
        let status = empty_status();
        let car = job("ship-a-change", "a car", JobStatus::Open, json!({}));
        let steps = vec![step(
            &car,
            crate::car::GATE_SLUG,
            StepStatus::Completed,
            Some("2026-09-19T09:00:00Z"),
        )];
        let older = job(
            "ship-a-change",
            "an older car",
            JobStatus::Closed,
            json!({}),
        );
        let older_steps = vec![step(
            &older,
            crate::car::GATE_SLUG,
            StepStatus::Completed,
            // Inside the PREVIOUS window (24-48h back).
            Some("2026-09-18T09:00:00Z"),
        )];
        let cars = vec![(car, steps), (older, older_steps)];
        let inputs = region_inputs(&status, &cars, &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "gates", "dock");
        assert_eq!(b.rate.samples, 1);
        assert_eq!(b.rate.previous_samples, 1);
        assert_eq!(b.rate.current, Some(1.0));
        assert_eq!(
            b.last_crossed.as_deref(),
            Some(t("2026-09-19T09:00:00Z").to_rfc3339().as_str())
        );
        assert_eq!(b.state, RegionState::Clear);
        assert_eq!(b.waiting, Some(0));
    }

    /// THE HOP INTO THE SHOP FLOOR (backlog 94c6ffd0): one crossing per
    /// run opened, and what waits is the marshalling yard's own
    /// standing packets — counted ONCE across stations, because a
    /// packet can stand at two of them.
    #[test]
    fn a_run_opening_is_one_crossing_into_the_shop_floor_and_the_stations_are_what_waits() {
        let status = empty_status();
        let runs = vec![
            (
                job(
                    "agent-run",
                    "a run",
                    JobStatus::Open,
                    json!({ "opened_at": "2026-09-19T09:00:00Z" }),
                ),
                Vec::new(),
            ),
            (
                job(
                    "agent-run",
                    "an older run",
                    JobStatus::Closed,
                    json!({ "opened_at": "2026-09-18T09:00:00Z" }),
                ),
                Vec::new(),
            ),
        ];
        let stations = vec![
            crate::regions::StationReading {
                name: "backlog".to_string(),
                members: vec!["p1".to_string(), "p2".to_string()],
                ..Default::default()
            },
            crate::regions::StationReading {
                name: "design-review".to_string(),
                members: vec!["p2".to_string()],
                ..Default::default()
            },
        ];
        let base = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&stations));
        let inputs = RegionInputs {
            agent_runs: Some(&runs),
            ..base
        };
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "marshalling", "shop-floor");
        assert_eq!(b.rate.samples, 1, "one run opened in this window");
        assert_eq!(b.rate.previous_samples, 1);
        assert_eq!(
            b.waiting,
            Some(2),
            "p2 stands at two stations, counted once"
        );
        assert_eq!(b.state, RegionState::Clear, "{}", b.why);
        assert_eq!(
            b.holds.first().map(|h| h.what.as_str()),
            Some("backlog"),
            "the deepest station leads"
        );
    }

    /// An unread station registry leaves the RATE — a measurement that
    /// was taken — and refuses the queue, which was not.
    #[test]
    fn an_unread_station_registry_leaves_the_rate_and_refuses_the_queue() {
        let status = empty_status();
        let runs = vec![(
            job(
                "agent-run",
                "a run",
                JobStatus::Open,
                json!({ "opened_at": "2026-09-19T09:00:00Z" }),
            ),
            Vec::new(),
        )];
        let base = region_inputs(&status, &[], &[], &[], Some(&[]), None);
        let inputs = RegionInputs {
            agent_runs: Some(&runs),
            ..base
        };
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "marshalling", "shop-floor");
        assert_eq!(b.rate.samples, 1);
        assert_eq!(b.waiting, None);
        assert_eq!(b.state, RegionState::Troubled);
        assert!(b.why.contains("station registry"), "why: {}", b.why);
    }

    #[test]
    fn a_machine_with_no_firing_record_says_so_rather_than_reading_healthy() {
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let dispatcher = only(&out, "gates", "dock");
        assert_eq!(dispatcher.machine.kind, "dispatcher-rule");
        assert_eq!(dispatcher.machine.last_fired, None);
        assert_eq!(
            dispatcher.machine.silent, None,
            "unknown must not render as not-silent"
        );
        assert!(
            dispatcher.machine.why.contains("no firing record was read"),
            "why: {}",
            dispatcher.machine.why
        );
        // A cadence machine the registry does not hold is named, not blank.
        let cadence = only(&out, "dock", "track");
        assert_eq!(cadence.machine.kind, "cadence");
        assert!(
            cadence.machine.why.contains("no rule named"),
            "why: {}",
            cadence.machine.why
        );
        // The whole cadence record unread reads differently again.
        let unread = borders(&BorderInputs {
            regions: &inputs,
            firings: None,
            dispatcher_firings: Some(&[]),
        });
        assert!(
            only(&unread, "dock", "track")
                .machine
                .why
                .contains("could not be read")
        );
    }

    #[test]
    fn traffic_waiting_and_a_machine_silent_past_its_cadence_is_troubled() {
        // Two cars parked on the dock, and the boarding rule last fired
        // three hours ago against a declared 30-minute heartbeat.
        let mut inputs_status = YardInputs {
            now: Some(t(NOW)),
            ..Default::default()
        };
        let a = job("ship-a-change", "car a", JobStatus::Open, json!({}));
        let b = job("ship-a-change", "car b", JobStatus::Open, json!({}));
        let parked = vec![(a.clone(), Vec::new()), (b.clone(), Vec::new())];
        inputs_status.dock_cars = &parked;
        let status = build_status_for(inputs_status, Reading::Read, BoardingReadings::default());
        assert_eq!(status.dock.len(), 2, "the dock rows are the fixture");
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let firings = vec![CadenceFiring {
            rule: "train-board-on-dock-depth".to_string(),
            fired_at: Some(t("2026-09-19T09:00:00Z")),
            every_minutes: Some(30),
        }];
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&firings),
            dispatcher_firings: Some(&[]),
        });
        let border = only(&out, "dock", "track");
        assert_eq!(border.waiting, Some(2));
        assert_eq!(border.machine.silent, Some(true));
        assert_eq!(border.machine.silent_for_minutes, Some(180));
        assert_eq!(border.state, RegionState::Troubled);
        assert!(
            border.why.contains("silent for 180m"),
            "why: {}",
            border.why
        );
        // A fresh firing is the same queue, clear rather than troubled.
        let fresh = vec![CadenceFiring {
            rule: "train-board-on-dock-depth".to_string(),
            fired_at: Some(t("2026-09-19T11:50:00Z")),
            every_minutes: Some(30),
        }];
        let ok = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&fresh),
            dispatcher_firings: Some(&[]),
        });
        let border = only(&ok, "dock", "track");
        assert_eq!(border.machine.silent, Some(false));
        assert_eq!(border.state, RegionState::Clear);
        assert!(border.holds.iter().all(|h| !h.why.is_empty()));
    }

    #[test]
    fn a_hold_carries_the_records_own_reason() {
        let mut yard = YardInputs {
            now: Some(t(NOW)),
            ..Default::default()
        };
        // A green held off the dock on purpose, with the operator's reason.
        let green = job(
            "gate-run",
            "a held green",
            JobStatus::Closed,
            json!({
                "branch": "fix/a-held-green",
                "outcome": "passed",
                "hold": "waiting on David's call",
                "opened_at": "2026-09-19T08:00:00Z",
            }),
        );
        let green_steps = vec![step(
            &green,
            "verdict",
            StepStatus::Completed,
            Some("2026-09-19T08:30:00Z"),
        )];
        let runs = vec![(green, green_steps)];
        yard.gate_runs = &runs;
        let status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "gates", "dock");
        let held = status.held.len() + status.stranded.len();
        assert_eq!(b.waiting, Some(held));
        if held > 0 {
            assert_eq!(b.state, RegionState::Clear);
            assert!(
                b.holds.iter().any(|h| h.what == "fix/a-held-green"),
                "holds: {:?}",
                b.holds
            );
        }
    }

    #[test]
    fn the_holds_list_is_bounded_and_the_count_is_not() {
        let mut yard = YardInputs {
            now: Some(t(NOW)),
            ..Default::default()
        };
        let parked: Vec<(Job, Vec<Step>)> = (0..MAX_HOLDS + 5)
            .map(|i| {
                (
                    job(
                        "ship-a-change",
                        &format!("car {i}"),
                        JobStatus::Open,
                        json!({}),
                    ),
                    Vec::new(),
                )
            })
            .collect();
        yard.dock_cars = &parked;
        let status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "dock", "track");
        assert_eq!(b.waiting, Some(MAX_HOLDS + 5));
        assert_eq!(b.holds.len(), MAX_HOLDS);
    }

    #[test]
    fn a_dispatcher_rules_firing_is_read_from_its_own_record() {
        // b14afc48: the gates -> dock rail's machine is the
        // dispatcher rule auto-park-on-gate-green, and until
        // `dispatcher_firings` existed it could only answer 'nothing
        // records a dispatcher rule's firings' — the most automated hop
        // on the map was the one that could not prove it ran.
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let fired = vec![DispatcherFiring {
            rule: "auto-park-on-gate-green".to_string(),
            fired_at: Some(t("2026-09-19T11:00:00Z")),
        }];
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&fired),
        });
        let b = only(&out, "gates", "dock");
        assert_eq!(b.machine.kind, "dispatcher-rule");
        assert_eq!(
            b.machine.last_fired.as_deref(),
            Some(t("2026-09-19T11:00:00Z").to_rfc3339().as_str())
        );
        assert_eq!(b.machine.silent_for_minutes, Some(60));
        assert!(
            b.machine.why.contains("dispatcher_firings"),
            "why: {}",
            b.machine.why
        );
        // The half that must NOT follow from a firing record: an event
        // rule declares no heartbeat, so there is no interval to be
        // silent against and `silent` stays null.
        assert_eq!(b.machine.expected_every_minutes, None);
        assert_eq!(
            b.machine.silent, None,
            "an event-driven rule has no declared interval: silence is not a reading for it"
        );
    }

    #[test]
    fn an_unread_dispatcher_record_reads_differently_from_a_rule_that_never_fired() {
        // The three answers a firing record owes, kept apart: unread,
        // read-and-never-fired, and a name the read did not cover. None
        // of them is a zero and none of them is a quiet rail.
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let unread = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: None,
        });
        let m = &only(&unread, "gates", "dock").machine;
        assert_eq!(m.last_fired, None);
        assert_eq!(m.silent, None);
        assert!(m.why.contains("could not be read"), "why: {}", m.why);

        let never = vec![DispatcherFiring {
            rule: "auto-park-on-gate-green".to_string(),
            fired_at: None,
        }];
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&never),
        });
        let m = &only(&out, "gates", "dock").machine;
        assert_eq!(m.last_fired, None);
        assert!(m.why.contains("has never fired"), "why: {}", m.why);
    }

    /// A gate-run that went green and no car ever claimed — the shape
    /// `stranded::unparked_green` recognises: a green verdict on a step,
    /// a branch, no hold and no spent marker. `opened_at` is the stamp
    /// `boss gate` writes and `yard::opened_since` reads; `None` is the
    /// row that carries only a date.
    fn green_run(branch: &str, opened_at: Option<&str>) -> (Job, Vec<Step>) {
        let mut md = json!({ "branch": branch, "outcome": "passed" });
        if let Some(o) = opened_at {
            md["opened_at"] = json!(o);
        }
        let g = job("gate-run", branch, JobStatus::Closed, md);
        let mut s = step(&g, "record-verdict", StepStatus::Completed, None);
        s.metadata = json!({ "verdict": "green" });
        (g, vec![s])
    }

    fn fired(at: Option<&str>) -> Vec<DispatcherFiring> {
        vec![DispatcherFiring {
            rule: "auto-park-on-gate-green".to_string(),
            fired_at: at.map(t),
        }]
    }

    #[test]
    fn a_green_waiting_since_before_the_rules_last_firing_is_troubled() {
        // c53f8f38: the event-driven form of silence. The rule declares
        // no interval and must not be given one — the question that
        // needs none is whether it has fired since the oldest thing
        // waiting on it ARRIVED.
        let runs = vec![green_run(
            "fix/a-forgotten-green",
            Some("2026-09-19T08:00:00Z"),
        )];
        let yard = YardInputs {
            now: Some(t(NOW)),
            gate_runs: &runs,
            ..Default::default()
        };
        let status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        assert_eq!(
            status.stranded.len(),
            1,
            "the fixture is one stranded green"
        );
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));

        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&fired(Some("2026-09-19T07:00:00Z"))),
        });
        let b = only(&out, "gates", "dock");
        assert_eq!(b.state, RegionState::Troubled);
        assert!(b.why.contains("has not fired since"), "why: {}", b.why);
        // The half this must NOT do: no interval was invented for an
        // event rule, so the cadence reading stays absent (b14afc48).
        assert_eq!(b.machine.expected_every_minutes, None);
        assert_eq!(b.machine.silent, None);

        // The control: the same queue with a firing AFTER the oldest
        // arrived is clear, not troubled — the machine is running.
        let ok = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&fired(Some("2026-09-19T11:00:00Z"))),
        });
        assert_eq!(only(&ok, "gates", "dock").state, RegionState::Clear);
    }

    #[test]
    fn a_green_with_no_arrival_stamp_is_never_judged_stalled() {
        // The 6c2eba00 case: `opened_at` is a filer convention, so a row
        // without one knows only its DAY. Judging a stall off midnight
        // would alarm on every such row; unknown is unknown.
        let runs = vec![green_run("fix/an-unstamped-green", None)];
        let yard = YardInputs {
            now: Some(t(NOW)),
            gate_runs: &runs,
            ..Default::default()
        };
        let status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        assert_eq!(status.stranded.len(), 1);
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&fired(Some("2026-09-19T07:00:00Z"))),
        });
        let b = only(&out, "gates", "dock");
        assert_eq!(b.waiting, Some(1), "it still stands at the border");
        assert_eq!(
            b.state,
            RegionState::Clear,
            "an arrival nobody stamped is not an alarm: {}",
            b.why
        );
    }

    #[test]
    fn a_green_held_on_purpose_never_makes_the_border_troubled() {
        // A brake deliberately on is not an alarm (`crate::stranded`).
        // The rule was never going to park a held green, so its silence
        // about one says nothing about the hop.
        let md = json!({
            "branch": "fix/a-held-green",
            "outcome": "passed",
            "hold": "waiting on David's call",
            "opened_at": "2026-09-19T08:00:00Z",
        });
        let g = job("gate-run", "a held green", JobStatus::Closed, md);
        let mut s = step(&g, "record-verdict", StepStatus::Completed, None);
        s.metadata = json!({ "verdict": "green" });
        let runs = vec![(g, vec![s])];
        let yard = YardInputs {
            now: Some(t(NOW)),
            gate_runs: &runs,
            ..Default::default()
        };
        let status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        assert_eq!(status.held.len(), 1, "the fixture is one HELD green");
        assert_eq!(status.stranded.len(), 0);
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&fired(Some("2026-09-19T07:00:00Z"))),
        });
        let b = only(&out, "gates", "dock");
        assert_eq!(b.waiting, Some(1));
        assert_eq!(b.state, RegionState::Clear, "why: {}", b.why);
    }

    /// THE FLOW ORDER (design 62de32ae decision 3). The line's rails,
    /// read in declared order, chain end to end — each rail leaves the
    /// territory the one before it arrived at — through the eight
    /// regions a car walks: built on the shop floor, gated, parked on
    /// the dock, boarded, arrived, proven. The map drew the dock BEFORE
    /// the gates until 2026-09-24, so a car followed across it walked
    /// backwards through two regions; the rails' own definitions said
    /// otherwise the whole time.
    #[test]
    fn the_line_chains_in_the_order_a_car_walks_it() {
        let sidings = ["garage", "publish"];
        let line: Vec<&BorderSpec> = BORDERS
            .iter()
            .filter(|b| !sidings.contains(&b.to))
            .collect();
        let mut walked = vec![line[0].from];
        for pair in line.windows(2) {
            assert_eq!(
                pair[0].to, pair[1].from,
                "{} -> {} is not followed by a rail out of {}",
                pair[0].from, pair[0].to, pair[0].to
            );
        }
        walked.extend(line.iter().map(|b| b.to));
        assert_eq!(
            walked,
            [
                "receiving",
                "marshalling",
                "shop-floor",
                "gates",
                "dock",
                "track",
                "arrivals",
                "shed"
            ]
        );
        // And the sidings hang where the design put them: the garage
        // under gates (judged red) and track (a red train), publish off
        // arrivals.
        let sides: Vec<(&str, &str)> = BORDERS
            .iter()
            .filter(|b| sidings.contains(&b.to))
            .map(|b| (b.from, b.to))
            .collect();
        assert_eq!(
            sides,
            [
                ("arrivals", "publish"),
                ("gates", "garage"),
                ("track", "garage")
            ]
        );
    }

    /// A gate-run as `boss gate` files it: `opened_at` stamped, a
    /// branch, open; `train` set when the conductor filed it for a
    /// train's assembled tree (design 128b5496).
    fn gate_run(branch: &str, opened_at: &str, train: Option<&str>, outcome: Option<&str>) -> Job {
        let mut md = json!({ "branch": branch, "opened_at": opened_at });
        if let Some(t) = train {
            md["train_gate"] = json!(true);
            md["train"] = json!(t);
        }
        let status = match outcome {
            Some(o) => {
                md["outcome"] = json!(o);
                md["closed_at"] = json!(opened_at);
                JobStatus::Closed
            }
            None => JobStatus::Open,
        };
        job("gate-run", branch, status, md)
    }

    /// THE HOP OUT OF THE SHOP FLOOR (design 62de32ae decision 3): a
    /// branch built there taking a bay is one crossing. A TRAIN's own
    /// gate runs in the same bays but came from the track, not the shop
    /// floor, so it is neither a crossing of this rail nor something
    /// waiting at it.
    #[test]
    fn a_gate_run_opening_is_one_crossing_from_the_shop_floor_and_a_trains_gate_is_not() {
        let runs = vec![
            gate_run("fix/a-car", "2026-09-19T09:00:00Z", None, None),
            gate_run("train/42", "2026-09-19T10:00:00Z", Some("train-42"), None),
        ];
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &runs, Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "shop-floor", "gates");
        assert_eq!(
            b.rate.samples, 1,
            "the car's gate crossed; the train's did not"
        );
        assert_eq!(
            b.last_crossed.as_deref(),
            Some(t("2026-09-19T09:00:00Z").to_rfc3339().as_str())
        );
        assert_eq!(b.machine.kind, "gate-runner");
    }

    /// A red TRAIN gate strikes the train, not a car: what reaches the
    /// garage from the gates is a car's branch judged red (the garage
    /// lane makes the same exclusion, `yard::garage_of`).
    #[test]
    fn a_red_trains_gate_sends_no_car_to_the_garage() {
        let runs = vec![
            gate_run(
                "fix/a-red-car",
                "2026-09-19T09:00:00Z",
                None,
                Some("failed"),
            ),
            gate_run(
                "train/42",
                "2026-09-19T10:00:00Z",
                Some("train-42"),
                Some("failed"),
            ),
        ];
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &runs, Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        assert_eq!(only(&out, "gates", "garage").rate.samples, 1);
    }

    /// THE CROSSING INTO MARSHALLING IS AN INTAKE (design 62de32ae
    /// decision 4, car B's `taken_in` rule): one crossing per inbound
    /// packet whose intake step completed in the window, stamped when
    /// it completed. A CLOSE is not a crossing — a packet taken in last
    /// week and closed today crossed last week, and one closed without
    /// ever being taken in never crossed at all. Until this, the rail
    /// counted inbound closures.
    #[test]
    fn an_intake_completing_is_one_crossing_into_marshalling_and_a_close_is_not() {
        let item = |title: &str, status: JobStatus, closed: Option<&str>| {
            let md = match closed {
                Some(c) => json!({ "closed_at": c }),
                None => json!({}),
            };
            job("backlog-item", title, status, md)
        };
        let admitted = |j: &Job| {
            let mut s = step(
                j,
                "submitted",
                StepStatus::Completed,
                Some("2026-09-10T08:00:00Z"),
            );
            s.kind = crate::regions::TRIGGER_STEP_KIND.to_string();
            s
        };
        // Taken in this morning, still open (waiting on its build).
        let a = item("taken in today", JobStatus::Open, None);
        let a_steps = vec![
            admitted(&a),
            step(
                &a,
                "triage",
                StepStatus::Completed,
                Some("2026-09-19T08:00:00Z"),
            ),
            step(&a, "build", StepStatus::Ready, None),
        ];
        // Taken in this morning and closed on the same act.
        let b = item(
            "triaged and closed today",
            JobStatus::Closed,
            Some("2026-09-19T09:00:00Z"),
        );
        let b_steps = vec![
            admitted(&b),
            step(
                &b,
                "triage",
                StepStatus::Completed,
                Some("2026-09-19T09:00:00Z"),
            ),
        ];
        // Taken in a week ago; its close today is not a crossing.
        let c = item(
            "taken in last week",
            JobStatus::Closed,
            Some("2026-09-19T10:00:00Z"),
        );
        let c_steps = vec![
            admitted(&c),
            step(
                &c,
                "triage",
                StepStatus::Completed,
                Some("2026-09-12T10:00:00Z"),
            ),
            step(
                &c,
                "build",
                StepStatus::Completed,
                Some("2026-09-19T10:00:00Z"),
            ),
        ];
        // Closed without anything taking it in: it never crossed.
        let d = item("withdrawn", JobStatus::Closed, Some("2026-09-19T11:00:00Z"));
        let d_steps = vec![admitted(&d)];
        // Still standing in receiving.
        let e = item("untriaged", JobStatus::Open, None);
        let e_steps = vec![admitted(&e), step(&e, "triage", StepStatus::Ready, None)];
        let inbound = vec![
            (a, a_steps),
            (b, b_steps),
            (c, c_steps),
            (d, d_steps),
            (e, e_steps),
        ];
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], Some(&inbound), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let rail = only(&out, "receiving", "marshalling");
        assert_eq!(rail.rate.samples, 2, "two intakes completed today");
        assert_eq!(
            rail.rate.previous_samples, 0,
            "last week's is outside both windows"
        );
        assert_eq!(
            rail.last_crossed.as_deref(),
            Some(t("2026-09-19T09:00:00Z").to_rfc3339().as_str()),
            "the newest INTAKE, not the newest close"
        );
        assert_eq!(rail.waiting, Some(1), "only the untriaged one stands");
    }

    // -----------------------------------------------------------------
    // MOTION'S FACTS (design 31bade8f, "The IT map moves", decision 8;
    // car M1 on backlog d220022f): whether a rail is flowing, since when
    // it has been held, and on whom what stands at it is waiting.
    // -----------------------------------------------------------------

    /// Every border's classes sum to its `waiting` — the pile the map
    /// draws is the server's count, square for square — and the stuck
    /// class across every border is the stuck block's own population, so
    /// a rail's red sediment and the HUD's stuck number cannot disagree.
    fn assert_conserved(out: &Borders, inputs: &RegionInputs<'_>) {
        for b in &out.borders {
            match (b.waiting, b.holds_by_class) {
                (Some(n), Some(c)) => assert_eq!(
                    c.machine + c.person + c.unknown + c.stuck,
                    n,
                    "{} -> {}: {c:?} does not sum to {n} waiting",
                    b.from,
                    b.to
                ),
                (None, None) => {}
                (w, c) => panic!(
                    "{} -> {}: waiting {w:?} and classes {c:?} disagree about whether the queue was read",
                    b.from, b.to
                ),
            }
        }
        let on_rails: usize = out
            .borders
            .iter()
            .filter_map(|b| b.holds_by_class)
            .map(|c| c.stuck)
            .sum();
        let in_block: usize = crate::regions::stuck(inputs).iter().map(|t| t.stuck).sum();
        assert_eq!(
            on_rails, in_block,
            "the rails' stuck sediment is the stuck block's population"
        );
    }

    fn classes(b: &Border) -> (usize, usize, usize, usize) {
        let c = b.holds_by_class.expect("the queue was read");
        (c.machine, c.person, c.unknown, c.stuck)
    }

    /// 24 branches took a bay in 24 minutes: a mean gap of 60m over the
    /// 24h window, so the rail is still once it has been quiet past
    /// 240m. It has been quiet since 05:23 — 397m at noon.
    #[test]
    fn a_rail_quiet_past_four_of_its_mean_gaps_is_still_and_held_since_its_last_crossing() {
        let mut runs: Vec<Job> = (0..24)
            .map(|k| {
                gate_run(
                    &format!("fix/b{k}"),
                    &format!("2026-09-19T05:{k:02}:00Z"),
                    None,
                    None,
                )
            })
            .collect();
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &runs, Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let still = only(&out, "shop-floor", "gates").clone();
        assert_eq!(still.rate.samples, 24);
        assert_eq!(still.flowing, Some(false), "{}", still.flowing_why);
        assert_eq!(
            still.held_since.as_deref(),
            Some(t("2026-09-19T05:23:00Z").to_rfc3339().as_str()),
            "held since the record's own last crossing"
        );
        assert_eq!(still.held_since, still.last_crossed);
        assert!(
            still.flowing_why.contains(BORDER_STILL.band)
                && still.flowing_why.contains("quiet 6h")
                && still
                    .flowing_why
                    .contains("a mean gap of 1h: 24 crossings in 24h"),
            "the rule's words, with the quiet and the gap it was judged by: {}",
            still.flowing_why
        );

        // THE CONTROL: one more crossing at 11:00 is 60m of quiet
        // against a limit of ~230m — flowing, and nothing is held.
        runs.push(gate_run("fix/late", "2026-09-19T11:00:00Z", None, None));
        let inputs = region_inputs(&status, &[], &[], &runs, Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let moving = only(&out, "shop-floor", "gates");
        assert_eq!(moving.flowing, Some(true), "{}", moving.flowing_why);
        assert_eq!(moving.held_since, None);
        // Stillness is its own signal: it changes neither the state nor
        // its sentence.
        assert_eq!(moving.state, still.state);
    }

    /// A rail nothing crossed in the window is still, and the instant it
    /// has been held since is the last crossing the record holds — or
    /// none, when nothing crossed in the whole read: an onset the record
    /// does not hold is never invented.
    #[test]
    fn a_rail_nothing_crossed_in_the_window_is_still_and_names_the_last_crossing_it_read() {
        let yesterday = vec![gate_run(
            "fix/red-yesterday",
            "2026-09-18T09:00:00Z",
            None,
            Some("failed"),
        )];
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &yesterday, Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "gates", "garage");
        assert_eq!((b.rate.samples, b.rate.previous_samples), (0, 1));
        assert_eq!(b.flowing, Some(false), "{}", b.flowing_why);
        assert_eq!(
            b.held_since.as_deref(),
            Some(t("2026-09-18T09:00:00Z").to_rfc3339().as_str())
        );
        assert!(
            b.flowing_why.contains("nothing crossed in 24h"),
            "{}",
            b.flowing_why
        );

        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "gates", "garage");
        assert_eq!(b.flowing, Some(false));
        assert_eq!(b.held_since, None, "no crossing read, so no onset");
        assert!(
            b.flowing_why.contains("nothing crossed in the 48h read"),
            "{}",
            b.flowing_why
        );
    }

    /// UNKNOWN STAYS NULL, NEVER 0 OR FALSE. A rail whose crossings could
    /// not be read cannot be told to flow, and a queue that could not be
    /// read has no classes — each rides the payload as an explicit null,
    /// so a reader can tell "cannot tell" from an older server.
    #[test]
    fn a_rail_whose_crossings_could_not_be_read_cannot_be_told_to_flow() {
        let status = empty_status();
        let inputs = region_inputs(&status, &[], &[], &[], None, Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let b = only(&out, "receiving", "marshalling");
        assert_eq!(b.flowing, None);
        assert_eq!(b.held_since, None);
        assert_eq!(b.holds_by_class, None);
        assert!(
            b.flowing_why.contains("cannot be told"),
            "{}",
            b.flowing_why
        );
        let v = serde_json::to_value(b).unwrap();
        for key in ["flowing", "held_since", "holds_by_class"] {
            assert_eq!(v.get(key), Some(&Value::Null), "{key}: {v}");
        }
        // And a read one carries the four classes by name.
        let read = serde_json::to_value(only(&out, "shop-floor", "gates")).unwrap();
        assert_eq!(
            read["holds_by_class"],
            json!({ "machine": 0, "person": 0, "unknown": 0, "stuck": 0 })
        );
    }

    /// RECEIVING and MARSHALLING. Intake nobody has taken in waits on a
    /// person, and past the triage band it is stuck (the stuck block's
    /// own predicate). A station's packet waits on a person to take it,
    /// is stuck at a station that served nothing, and is unknown at a
    /// station whose flow the cube cannot see — each distinct packet
    /// once, the worst of its stations deciding.
    #[test]
    fn intake_and_stations_are_classed_by_the_stuck_blocks_own_predicates() {
        let intake = |title: &str, opened: (i32, u32, u32)| {
            let mut j = job("backlog-item", title, JobStatus::Open, json!({}));
            j.opened_on = chrono::NaiveDate::from_ymd_opt(opened.0, opened.1, opened.2).unwrap();
            let mut admitted = step(
                &j,
                "submitted",
                StepStatus::Completed,
                Some("2026-09-10T08:00:00Z"),
            );
            admitted.kind = crate::regions::TRIGGER_STEP_KIND.to_string();
            let steps = vec![admitted, step(&j, "triage", StepStatus::Ready, None)];
            (j, steps)
        };
        let inbound = vec![
            intake("fresh", (2026, 9, 19)),
            intake("a week old", (2026, 9, 12)),
        ];
        let station = |name: &str, members: &[&str], served: Option<i64>| StationReading {
            name: name.to_string(),
            members: members.iter().map(|m| (*m).to_string()).collect(),
            served,
            ..Default::default()
        };
        let stations = vec![
            station("draining", &["p1", "p2"], Some(3)),
            station("blind", &["p2", "p3"], None),
            station("not-draining", &["p4"], Some(0)),
        ];
        let status = empty_status();
        let mut inputs = region_inputs(&status, &[], &[], &[], Some(&inbound), Some(&stations));
        // The run packets read, so the rail into the shop floor is read.
        inputs.agent_runs = Some(&[]);
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        assert_eq!(
            classes(only(&out, "receiving", "marshalling")),
            (0, 1, 0, 1),
            "fresh waits on a person; a week old is past the 3-day band"
        );
        assert_eq!(
            classes(only(&out, "marshalling", "shop-floor")),
            (0, 1, 2, 1),
            "p1 on a person; p2 and p3 at a blind station; p4 stuck"
        );
        assert_conserved(&out, &inputs);
    }

    /// THE DELIVERY RAILS. A branch in line for a bay and a parked car
    /// waiting for a train are in line for a machine; a held green and a
    /// car held on the dock are brakes someone here set — stuck, as the
    /// stuck block counts them; a stranded green waits on the rule the
    /// rail's own judge reads it against.
    #[test]
    fn the_delivery_rails_class_each_packet_by_what_it_waits_on() {
        let held = {
            let md = json!({
                "branch": "fix/a-held-green",
                "outcome": "passed",
                "hold": "waiting on David's call",
                "opened_at": "2026-09-19T08:00:00Z",
            });
            let g = job("gate-run", "a held green", JobStatus::Closed, md);
            let mut s = step(&g, "record-verdict", StepStatus::Completed, None);
            s.metadata = json!({ "verdict": "green" });
            (g, vec![s])
        };
        let runs = vec![
            held,
            green_run("fix/a-stranded-green", Some("2026-09-19T08:00:00Z")),
        ];
        let parked = vec![
            (
                job("ship-a-change", "car a", JobStatus::Open, json!({})),
                Vec::new(),
            ),
            (
                job("ship-a-change", "car b", JobStatus::Open, json!({})),
                Vec::new(),
            ),
        ];
        let yard = YardInputs {
            now: Some(t(NOW)),
            gate_runs: &runs,
            dock_cars: &parked,
            ..Default::default()
        };
        let mut status = build_status_for(yard, Reading::Read, BoardingReadings::default());
        assert_eq!((status.held.len(), status.stranded.len()), (1, 1));
        assert_eq!(status.dock.len(), 2);
        // One of the two parked cars held on the dock by hand.
        let braked = status.dock.remove(1);
        status.held_cars.push(crate::yard::HeldCar {
            car: braked,
            reason: "held for the restart".to_string(),
        });
        status.gates.queued.push(crate::yard::QueuedGate {
            branch: "fix/in-line".to_string(),
            packet_id: "g-1".to_string(),
            queued_at: "2026-09-19T11:50:00Z".to_string(),
            position: 1,
            waiting_seconds: None,
            estimated_wait_seconds: None,
            train: None,
        });
        let inputs = region_inputs(&status, &[], &[], &[], Some(&[]), Some(&[]));
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        assert_eq!(classes(only(&out, "shop-floor", "gates")), (1, 0, 0, 0));
        assert_eq!(
            classes(only(&out, "gates", "dock")),
            (1, 0, 0, 1),
            "the stranded green on the rule; the held green stuck"
        );
        assert_eq!(
            classes(only(&out, "dock", "track")),
            (1, 0, 0, 1),
            "the parked car on a train; the braked car stuck"
        );
        assert_conserved(&out, &inputs);
    }

    /// THE SHED and THE MIRROR. A car whose probe is recorded and has not
    /// run waits on the probe runner; one waiting on an event waits on
    /// the world; one a day old with nothing that can prove it is stuck.
    /// A pull request GitHub was never asked about is unknown; one open
    /// on the mirror waits on the person whose merge it is.
    #[test]
    fn the_shed_and_the_mirror_class_what_stands_at_them() {
        let awaiting = |branch: &str, md: Value| {
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-18T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let cars = vec![
            awaiting(
                "fix/probe-pending",
                json!({ "proof_probe": "true", "opened_at": "2026-09-19T10:00:00Z" }),
            ),
            awaiting(
                "fix/on-an-event",
                json!({ "proof_event": "the next red train", "opened_at": "2026-09-19T10:00:00Z" }),
            ),
            awaiting(
                "fix/forgotten",
                json!({ "opened_at": "2026-09-18T06:00:00Z" }),
            ),
        ];
        let publish = vec![
            publish_packet(
                "https://mirror/pull/250",
                "snap-250",
                "mirror-a",
                "2026-09-19T09:00:00Z",
                ("success", "0", "0"),
            ),
            {
                let (mut j, s) = publish_packet(
                    "https://mirror/pull/251",
                    "snap-251",
                    "snap-250",
                    "2026-09-19T10:00:00Z",
                    ("success", "0", "0"),
                );
                j.metadata = json!({ "pr_state": {
                    "pr_url": "https://mirror/pull/251", "state": "open", "merged": false,
                    "read_at": "2026-09-19T11:00:00Z",
                }});
                (j, s)
            },
        ];
        let status = empty_status();
        let mut inputs = region_inputs(&status, &cars, &[], &[], Some(&[]), Some(&[]));
        inputs.publish_packets = Some(&publish);
        let out = borders(&BorderInputs {
            regions: &inputs,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        assert_eq!(classes(only(&out, "arrivals", "shed")), (1, 1, 0, 1));
        assert_eq!(
            classes(only(&out, "arrivals", "publish")),
            (0, 1, 1, 0),
            "#250 never read from GitHub; #251 read open"
        );
        assert_conserved(&out, &inputs);
    }

    #[test]
    fn ordinals_read_as_an_operator_says_them() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(21), "21st");
    }
}
