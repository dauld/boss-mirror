//! THE HUD'S ROWS — one per third of the operator surface, and the one
//! whole-system machine cell (design 00774ca8, "The HUD frame:
//! whole-system figures, fixed, one row per third", approved by David
//! 2026-09-24; car F of design 62de32ae).
//!
//! WHY THIS EXISTS. The HUD answers about the WHOLE system and the map
//! answers about the parts (design 17567423). Until this module the only
//! whole-system figure on /it was `borders.ts::summaryLine`, which the
//! CLIENT computed by adding up border rows — and that sum double-counted
//! about 260 backlog items, because two borders counted largely the same
//! packets (62de32ae decision 4). So every figure here is computed ONCE,
//! on the server, counting each packet once by id, and the HUD and
//! `boss orient` both print it. No client adds anything up.
//!
//! EACH ROW CARRIES TWO FIGURES that no single territory can answer
//! (decision 2):
//!
//! * **Balance** — is the third taking work in faster than it lets work
//!   out? In/day against out/day at the third's outer edges, in ONE unit
//!   per third, each packet counted once. "Out" includes packets that
//!   CLOSE inside the third: a finished run leaves actors-building by
//!   closing, not by crossing a border. The net is written here too, so
//!   the one subtraction a reader would otherwise do happens once.
//! * **Stuck · waiting** — [`crate::regions::ThirdStuck`] exactly as
//!   [`crate::regions::stuck`] answers it, side by side and never summed.
//!
//! ONE UNIT PER THIRD, and the edge pair says which ([`BALANCE`]). A
//! third's in and out must count the same thing — "trains against cars"
//! is the example the design names — so each edge declares its unit and
//! `a_thirds_two_edges_count_one_unit` refuses a pair that differs:
//!
//! | third            | unit             | in                          | out                                    |
//! |------------------|------------------|-----------------------------|----------------------------------------|
//! | queue-management | inbound packets  | an inbound packet opened    | its first run opened, or its close     |
//! | actors-building  | runs             | a run opened                | a run closed                           |
//! | delivery         | cars             | a car parked on a green     | a car closed                           |
//!
//! NO NUMBER THIS SURFACE MAKES UP (the bar of `crate::regions`). An
//! edge whose rows could not be read answers `null`, never zero, and the
//! balance says which read failed in `why`. The net is `null` whenever
//! either half is.

use boss_core::job::{Job, JobStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::regions::{
    Instant, MachineState, PUBLISH_KIND, Region, RegionInputs, SESSION_KIND, THIRDS, ThirdStuck,
    Windows, closed_at, find_step, opened_at, step_done_at,
};

/// One edge of a third: what ONE packet crossing it is, and the unit it
/// counts. The unit is what [`BALANCE`]'s pin compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// An inbound packet opened — work arriving from the world into
    /// receiving.
    InboundOpened,
    /// An inbound packet leaving queue management: the first run opened
    /// on it (an actor took it off a station), or its close if that came
    /// first (triaged away, answered by hand).
    InboundLeft,
    /// A run opened on the shop floor.
    RunOpened,
    /// A run closed — handed back, landed, or died.
    RunClosed,
    /// A green gate parked as a car on the dock (the car's `gate` step).
    CarParked,
    /// A car closed — landed and proven, or withdrawn.
    CarClosed,
}

impl Edge {
    /// The unit this edge counts, as the HUD prints it.
    pub fn unit(self) -> &'static str {
        match self {
            Edge::InboundOpened | Edge::InboundLeft => "inbound packets",
            Edge::RunOpened | Edge::RunClosed => "runs",
            Edge::CarParked | Edge::CarClosed => "cars",
        }
    }

    /// What one crossing of this edge IS, in words — a reader of the
    /// number never has to go and derive what it counted.
    pub fn means(self) -> &'static str {
        match self {
            Edge::InboundOpened => "an inbound packet opened",
            Edge::InboundLeft => {
                "an inbound packet taken off the queue — its first run opened, or its close"
            }
            Edge::RunOpened => "a run opened",
            Edge::RunClosed => "a run closed",
            Edge::CarParked => "a green gate parked as a car",
            Edge::CarClosed => "a car closed",
        }
    }
}

/// Each third's balance edges, in [`THIRDS`] order: (third, in, out).
/// `the_balance_table_follows_the_thirds` holds the names to [`THIRDS`];
/// `a_thirds_two_edges_count_one_unit` holds each pair to one unit.
pub const BALANCE: [(&str, Edge, Edge); 3] = [
    ("queue-management", Edge::InboundOpened, Edge::InboundLeft),
    ("actors-building", Edge::RunOpened, Edge::RunClosed),
    ("delivery", Edge::CarParked, Edge::CarClosed),
];

/// IS THE THIRD TAKING WORK IN FASTER THAN IT LETS WORK OUT? (decision 2)
/// Rates are per day over the current window; the counts they rest on
/// ride beside them, because a rate without its sample cannot be checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Balance {
    /// What both edges count: `inbound packets`, `runs`, `cars`.
    pub unit: String,
    /// What one packet in is, and one packet out, in words.
    pub in_means: String,
    pub out_means: String,
    /// Packets in per day. `null` when the rows behind it could not be
    /// read — never zero.
    #[serde(rename = "in")]
    pub into: Option<f64>,
    /// Packets out per day, closes inside the third included.
    pub out: Option<f64>,
    /// `in − out` per day: positive is the third filling up. `null` when
    /// either half is.
    pub net: Option<f64>,
    /// Distinct packets in, and out, inside the window.
    pub in_count: Option<usize>,
    pub out_count: Option<usize>,
    /// The read that failed, when a half is `null`; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One HUD row: a third, its regions in flow order (the row's membership,
/// straight from [`THIRDS`]), its balance and its stuck reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Third {
    pub third: String,
    pub regions: Vec<String>,
    pub balance: Balance,
    pub stuck: ThirdStuck,
}

/// A machine that is failed or cannot be judged, with the region it
/// stands in — the HUD's click-through to its glyph (decision 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineAt {
    pub region: String,
    pub id: String,
    pub name: String,
    pub state: MachineState,
    pub why: String,
}

/// THE WHOLE SYSTEM'S MACHINES, counted once by state (decision 3). A
/// count split by third would change meaning when the host runners move
/// into a plant strip serving every region (62de32ae decision 11), so
/// there is one cell, beside the rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineSummary {
    pub running: usize,
    pub idle: usize,
    pub failed: usize,
    /// Machines whose state the record cannot tell — "unjudged" on the
    /// HUD. Never folded into idle.
    pub unknown: usize,
    pub total: usize,
    /// Every failed and every unknown machine, failed first, then in
    /// region order — each one a link to its own glyph on the map.
    pub failed_or_unknown: Vec<MachineAt>,
}

/// The kinds a region of their OWN reads by kind — the shop floor's runs,
/// its crews, the publish packets. They are platform kinds, so the
/// registry's inbound rule admits them (`regions::inbound_kinds`), but
/// none of them is work arriving in the queue: counting a run opened as
/// an inbound packet opened would count one dispatch as intake too.
fn own_region_kind(kind: &str) -> bool {
    kind == crate::agent_budget::RUN_KIND || kind == SESSION_KIND || kind == PUBLISH_KIND
}

const NO_INBOUND: &str = "the workflow registry that names the inbound kinds could not be read";
const NO_RUNS: &str = "the agent-run packets could not be read";
const NO_DEPARTURES: &str = "the agent-run packets could not be read, so when a packet was taken off the queue cannot be told";

/// The inbound packets that are queue management's to count: every
/// inbound row but the kinds a region of their own reads.
fn queued<'a>(inputs: &RegionInputs<'a>) -> Result<Vec<&'a Job>, &'static str> {
    Ok(inputs
        .inbound
        .ok_or(NO_INBOUND)?
        .iter()
        .map(|(j, _)| j)
        .filter(|j| !own_region_kind(&j.kind))
        .collect())
}

/// ONE INSTANT PER DISTINCT PACKET that crossed `edge`, or the read that
/// failed. [`balance`] keeps the ones inside the window.
///
/// THE REACH, SAID, for [`Edge::InboundLeft`]: runs are read two windows
/// back, as every region reads them. A packet whose only runs opened
/// before that reach, and which closes inside this window, is counted as
/// leaving at its close.
fn stamps(edge: Edge, inputs: &RegionInputs<'_>) -> Result<Vec<Instant>, &'static str> {
    let runs = || inputs.agent_runs.ok_or(NO_RUNS);
    match edge {
        Edge::InboundOpened => Ok(queued(inputs)?.into_iter().filter_map(opened_at).collect()),
        Edge::InboundLeft => {
            let queued = queued(inputs)?;
            let runs = inputs.agent_runs.ok_or(NO_DEPARTURES)?;
            let mut first_run: std::collections::HashMap<&str, Instant> = Default::default();
            for (run, _) in runs {
                let (Some(packet), Some(at)) = (
                    run.metadata.get("packet").and_then(Value::as_str),
                    opened_at(run),
                ) else {
                    continue;
                };
                first_run
                    .entry(packet)
                    .and_modify(|t| *t = (*t).min(at))
                    .or_insert(at);
            }
            Ok(queued
                .into_iter()
                .filter_map(|j| {
                    let run = first_run.get(j.id.to_string().as_str()).copied();
                    let close = (j.status == JobStatus::Closed)
                        .then(|| closed_at(j))
                        .flatten();
                    match (run, close) {
                        (Some(r), Some(c)) => Some(r.min(c)),
                        (r, c) => r.or(c),
                    }
                })
                .collect())
        }
        Edge::RunOpened => Ok(runs()?.iter().filter_map(|(j, _)| opened_at(j)).collect()),
        Edge::RunClosed => Ok(runs()?.iter().filter_map(|(j, _)| closed_at(j)).collect()),
        Edge::CarParked => Ok(inputs
            .cars
            .iter()
            .filter_map(|(_, s)| {
                step_done_at(find_step(s, crate::car::GATE_SLUG, crate::car::GATE))
            })
            .collect()),
        Edge::CarClosed => Ok(inputs
            .cars
            .iter()
            .filter(|(j, _)| j.status == JobStatus::Closed)
            .filter_map(|(j, _)| closed_at(j))
            .collect()),
    }
}

/// A third's balance from its two edges: each packet counted once per
/// edge inside the window, as a per-day rate beside its count. A half
/// whose read failed is `None`, with the failed read in `why`.
fn balance(w: &Windows, into: Edge, out: Edge, inputs: &RegionInputs<'_>) -> Balance {
    let ins = stamps(into, inputs);
    let outs = stamps(out, inputs);
    let count = |s: &Result<Vec<Instant>, &str>| {
        s.as_ref()
            .ok()
            .map(|s| s.iter().filter(|t| w.current(**t)).count())
    };
    let (in_count, out_count) = (count(&ins), count(&outs));
    let mut why: Vec<&str> = [ins.err(), outs.err()].into_iter().flatten().collect();
    why.dedup();
    let into_rate = in_count.map(|n| w.per_day(n));
    let out_rate = out_count.map(|n| w.per_day(n));
    Balance {
        unit: into.unit().to_string(),
        in_means: into.means().to_string(),
        out_means: out.means().to_string(),
        into: into_rate,
        out: out_rate,
        net: into_rate.zip(out_rate).map(|(i, o)| i - o),
        in_count,
        out_count,
        why: (!why.is_empty()).then(|| why.join("; ")),
    }
}

/// THE HUD'S ROWS, in [`THIRDS`] order — each with its regions, the
/// balance of its [`BALANCE`] edges, and the stuck reading
/// [`crate::regions::stuck`] already made (passed in rather than
/// recomputed, so the top-level `stuck` block and the rows cannot
/// disagree). A third with no balance row, or no stuck reading, says so
/// rather than reading as a quiet one.
pub fn thirds(inputs: &RegionInputs<'_>, stuck: &[ThirdStuck]) -> Vec<Third> {
    let w = Windows::of(inputs.now, inputs.window_hours);
    THIRDS
        .iter()
        .filter_map(|(name, regions)| {
            let (_, into, out) = BALANCE.iter().find(|(t, _, _)| t == name)?;
            Some(Third {
                third: (*name).to_string(),
                regions: regions.iter().map(|r| (*r).to_string()).collect(),
                balance: balance(&w, *into, *out, inputs),
                stuck: stuck
                    .iter()
                    .find(|s| s.third == *name)
                    .cloned()
                    .unwrap_or_else(|| ThirdStuck {
                        third: (*name).to_string(),
                        stuck: 0,
                        waiting: 0,
                        unknown: vec!["no stuck reading was made for this third".into()],
                        oldest_hours: None,
                        regions: Vec::new(),
                    }),
            })
        })
        .collect()
}

/// The machine cell: every machine the regions carry, counted once by
/// state, and the failed and unjudged ones named with their region.
pub fn machine_summary(regions: &[Region]) -> MachineSummary {
    let all: Vec<(&str, &crate::regions::Machine)> = regions
        .iter()
        .flat_map(|r| r.machines.iter().map(move |m| (r.name.as_str(), m)))
        .collect();
    let of = |s: MachineState| all.iter().filter(|(_, m)| m.state == s).count();
    let named = |s: MachineState| {
        all.iter()
            .filter(move |(_, m)| m.state == s)
            .map(|(region, m)| MachineAt {
                region: (*region).to_string(),
                id: m.id.clone(),
                name: m.name.clone(),
                state: m.state,
                why: m.why.clone(),
            })
    };
    MachineSummary {
        running: of(MachineState::Running),
        idle: of(MachineState::Idle),
        failed: of(MachineState::Failed),
        unknown: of(MachineState::Unknown),
        total: all.len(),
        failed_or_unknown: named(MachineState::Failed)
            .chain(named(MachineState::Unknown))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::{Machine, REGIONS, Regions, parse_instant, regions};
    use crate::yard::{BoardingReadings, Reading, YardInputs, YardStatus, build_status_for};
    use boss_core::job::{JobId, Priority, Step, StepId, StepStatus, Subject};
    use serde_json::json;

    const NOW: &str = "2026-09-19T12:00:00Z";

    fn t(s: &str) -> Instant {
        parse_instant(s).unwrap()
    }

    fn job(kind: &str, status: JobStatus, metadata: Value) -> Job {
        Job {
            id: JobId::new(),
            kind: kind.into(),
            workflow_version: 1,
            subject: Subject::new("custom", "s"),
            title: kind.into(),
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

    /// A packet opened at `opened`, closed at `closed` when given.
    fn packet(kind: &str, opened: &str, closed: Option<&str>) -> Job {
        let mut md = json!({ "opened_at": opened });
        let status = match closed {
            Some(c) => {
                md["closed_at"] = json!(c);
                JobStatus::Closed
            }
            None => JobStatus::Open,
        };
        job(kind, status, md)
    }

    /// A run on `on`, opened at `opened`, closed at `closed` when given.
    fn run(on: &Job, opened: &str, closed: Option<&str>) -> Job {
        let mut j = packet(crate::agent_budget::RUN_KIND, opened, closed);
        j.metadata["packet"] = json!(on.id.to_string());
        j
    }

    /// A car whose `gate` (park) step completed at `parked`.
    fn car(parked: &str, closed: Option<&str>) -> (Job, Vec<Step>) {
        let j = packet("ship-a-change", parked, closed);
        let mut s = Step::new(j.id, "task", crate::car::GATE_SLUG, 0);
        s.id = StepId::new();
        s.spec_slug = Some(crate::car::GATE_SLUG.into());
        s.status = StepStatus::Completed;
        s.completed_at = Some(t(parked));
        (j, vec![s])
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

    fn inputs<'a>(
        status: &'a YardStatus,
        cars: &'a [(Job, Vec<Step>)],
        inbound: Option<&'a [(Job, Vec<Step>)]>,
        runs: Option<&'a [(Job, Vec<Step>)]>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            status,
            dock_reading: Reading::Read,
            open_trains: &[],
            closed_trains: &[],
            cars,
            gate_runs: &[],
            inbound,
            stations: Some(&[]),
            conductor: None,
            ops_requests: Some(&[]),
            runner_hosts: Some(&[]),
            agent_runs: runs,
            sessions: Some(&[]),
            publish_packets: Some(&[]),
            run_capacity: None,
            predecessors: &[],
            now: t(NOW),
            window_hours: 24,
        }
    }

    fn rows(jobs: &[Job]) -> Vec<(Job, Vec<Step>)> {
        jobs.iter().map(|j| (j.clone(), Vec::new())).collect()
    }

    fn row<'a>(r: &'a Regions, name: &str) -> &'a Third {
        r.thirds
            .iter()
            .find(|t| t.third == name)
            .unwrap_or_else(|| panic!("no third {name}: {:?}", r.thirds))
    }

    /// Decision 1: the thirds are a PARTITION — every region the layout
    /// draws stands in exactly one third, so the HUD's rows can take
    /// their membership from the payload and hardcode none of it.
    #[test]
    fn every_region_stands_in_exactly_one_third() {
        for region in REGIONS {
            let owners: Vec<&str> = THIRDS
                .iter()
                .filter(|(_, rs)| rs.contains(&region))
                .map(|(t, _)| *t)
                .collect();
            assert_eq!(owners.len(), 1, "{region} stands in {owners:?}");
        }
        let listed: usize = THIRDS.iter().map(|(_, rs)| rs.len()).sum();
        assert_eq!(
            listed,
            REGIONS.len(),
            "a third names a region the map does not draw"
        );
    }

    #[test]
    fn the_balance_table_follows_the_thirds() {
        let names: Vec<&str> = BALANCE.iter().map(|(t, _, _)| *t).collect();
        let thirds: Vec<&str> = THIRDS.iter().map(|(t, _)| *t).collect();
        assert_eq!(names, thirds);
    }

    /// Decision 2: a third's in and out count ONE unit — trains against
    /// cars would be a balance of two different things.
    #[test]
    fn a_thirds_two_edges_count_one_unit() {
        for (third, into, out) in BALANCE {
            assert_eq!(
                into.unit(),
                out.unit(),
                "{third}: in counts {} but out counts {}",
                into.unit(),
                out.unit()
            );
        }
    }

    /// The rows ride the payload in THIRDS order, each with its regions
    /// and the SAME stuck reading as the top-level block; an empty yard
    /// balances at a true zero — measured, not unknown.
    #[test]
    fn the_rows_carry_their_regions_their_balance_and_the_stuck_block() {
        let status = empty_status();
        let out = regions(&inputs(&status, &[], Some(&[]), Some(&[])));
        let names: Vec<&str> = out.thirds.iter().map(|t| t.third.as_str()).collect();
        assert_eq!(names, ["queue-management", "actors-building", "delivery"]);
        assert_eq!(
            row(&out, "actors-building").regions,
            ["shop-floor", "gates", "garage"]
        );
        for (t, s) in out.thirds.iter().zip(&out.stuck) {
            assert_eq!(&t.stuck, s, "one stuck reading, not two");
            assert_eq!(
                (t.balance.into, t.balance.out, t.balance.net),
                (Some(0.0), Some(0.0), Some(0.0)),
                "{t:?}"
            );
            assert_eq!(t.balance.why, None);
        }
        let v = serde_json::to_value(&out).unwrap();
        let b = &v["thirds"][0]["balance"];
        for key in [
            "unit",
            "in",
            "out",
            "net",
            "in_count",
            "out_count",
            "in_means",
            "out_means",
        ] {
            assert!(b.get(key).is_some(), "the balance carries {key}: {b}");
        }
        // An older payload: no rows and no machine cell — not answered,
        // never zero.
        let older: Regions =
            serde_json::from_value(json!({ "window_hours": 24, "regions": [] })).unwrap();
        assert!(older.thirds.is_empty() && older.machines.is_none());
    }

    /// Queue management counts each inbound packet once: in when it
    /// opened, out at its FIRST run or its close, whichever came first. A
    /// run opened on an agent-run is not intake.
    #[test]
    fn queue_management_counts_a_packet_out_once_at_its_first_run_or_its_close() {
        // NOW is 2026-09-19T12:00:00Z; the window is the 24h before it.
        let fresh = packet("backlog-item", "2026-09-19T08:00:00Z", None);
        let old_twice_run = packet("backlog-item", "2026-09-17T08:00:00Z", None);
        let old_run_before = packet("backlog-item", "2026-09-17T08:00:00Z", None);
        let triaged_away = packet(
            "backlog-item",
            "2026-09-18T01:00:00Z",
            Some("2026-09-19T01:00:00Z"),
        );
        let waiting = packet("user-feedback", "2026-09-10T08:00:00Z", None);
        let runs = rows(&[
            run(&old_twice_run, "2026-09-19T02:00:00Z", None),
            run(&old_twice_run, "2026-09-19T09:00:00Z", None),
            // Its first run was in the PREVIOUS window: it left then.
            run(
                &old_run_before,
                "2026-09-18T02:00:00Z",
                Some("2026-09-18T05:00:00Z"),
            ),
            run(&old_run_before, "2026-09-19T10:00:00Z", None),
        ]);
        let mut inbound = rows(&[fresh, old_twice_run, old_run_before, triaged_away, waiting]);
        // A run is an inbound kind by the registry's rule; it is the shop
        // floor's, not intake.
        inbound.extend(runs.iter().cloned());
        let status = empty_status();
        let out = regions(&inputs(&status, &[], Some(&inbound), Some(&runs)));
        let b = &row(&out, "queue-management").balance;
        assert_eq!(b.unit, "inbound packets");
        assert_eq!(
            b.in_count,
            Some(1),
            "only the fresh packet opened in the window: {b:?}"
        );
        assert_eq!(
            b.out_count,
            Some(2),
            "the twice-run packet leaves once, the triaged-away one at its close: {b:?}"
        );
        assert_eq!(b.net, Some(-1.0));
    }

    /// Actors building: runs opened against runs closed, whatever they
    /// are runs on.
    #[test]
    fn actors_building_balances_runs_opened_against_runs_closed() {
        let item = packet("backlog-item", "2026-09-10T08:00:00Z", None);
        let runs = rows(&[
            run(&item, "2026-09-19T02:00:00Z", Some("2026-09-19T03:00:00Z")),
            run(&item, "2026-09-19T04:00:00Z", None),
            run(&item, "2026-09-19T05:00:00Z", None),
            run(&item, "2026-09-18T01:00:00Z", Some("2026-09-19T01:00:00Z")),
        ]);
        let status = empty_status();
        let out = regions(&inputs(&status, &[], Some(&[]), Some(&runs)));
        let b = &row(&out, "actors-building").balance;
        assert_eq!((b.in_count, b.out_count), (Some(3), Some(2)), "{b:?}");
        assert_eq!((b.into, b.out, b.net), (Some(3.0), Some(2.0), Some(1.0)));
        assert_eq!(b.unit, "runs");
    }

    /// Delivery: cars parked against cars closed — the in-edge is the
    /// gates → dock rail's own stamp.
    #[test]
    fn delivery_balances_cars_parked_against_cars_closed() {
        let cars = vec![
            car("2026-09-19T02:00:00Z", None),
            car("2026-09-19T03:00:00Z", Some("2026-09-19T09:00:00Z")),
            car("2026-09-18T03:00:00Z", Some("2026-09-19T01:00:00Z")),
            car("2026-09-17T03:00:00Z", Some("2026-09-18T01:00:00Z")),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &cars, Some(&[]), Some(&[])));
        let b = &row(&out, "delivery").balance;
        assert_eq!((b.in_count, b.out_count), (Some(2), Some(2)), "{b:?}");
        assert_eq!(b.net, Some(0.0));
        assert_eq!(b.unit, "cars");
    }

    /// A read that failed is `null` with its reason — never a zero, and
    /// never a net computed from half a reading.
    #[test]
    fn an_unread_edge_is_null_with_its_reason_never_zero() {
        let status = empty_status();
        let fresh = rows(&[packet("backlog-item", "2026-09-19T08:00:00Z", None)]);
        let out = regions(&inputs(&status, &[], Some(&fresh), None));
        let qm = &row(&out, "queue-management").balance;
        assert_eq!(
            (qm.in_count, qm.out_count, qm.net),
            (Some(1), None, None),
            "{qm:?}"
        );
        assert!(
            qm.why.as_deref().is_some_and(|w| w.contains("agent-run")),
            "{qm:?}"
        );
        let ab = &row(&out, "actors-building").balance;
        assert_eq!((ab.into, ab.out, ab.net), (None, None, None), "{ab:?}");
        assert!(ab.why.is_some());

        let out = regions(&inputs(&status, &[], None, Some(&[])));
        let qm = &row(&out, "queue-management").balance;
        assert_eq!((qm.into, qm.out), (None, None), "{qm:?}");
        assert!(qm.why.as_deref().is_some_and(|w| w.contains("inbound")));
    }

    /// Decision 3: one machine cell, counted once by state, with every
    /// failed and unjudged machine named by its region — failed first.
    #[test]
    fn the_machine_cell_counts_each_machine_once_and_names_the_ones_not_judged_well() {
        let m = |id: &str, state| Machine {
            id: id.into(),
            name: id.into(),
            state,
            why: format!("{id} read"),
        };
        let status = empty_status();
        let mut out = regions(&inputs(&status, &[], Some(&[]), Some(&[])));
        for r in &mut out.regions {
            r.machines = Vec::new();
        }
        out.regions[0].machines =
            vec![m("a", MachineState::Running), m("b", MachineState::Unknown)];
        out.regions[1].machines = vec![
            m("c", MachineState::Idle),
            m("d", MachineState::Failed),
            m("e", MachineState::Unknown),
        ];
        let s = machine_summary(&out.regions);
        assert_eq!(
            (s.running, s.idle, s.failed, s.unknown, s.total),
            (1, 1, 1, 2, 5)
        );
        let named: Vec<(&str, &str)> = s
            .failed_or_unknown
            .iter()
            .map(|m| (m.region.as_str(), m.id.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                (out.regions[1].name.as_str(), "d"),
                (out.regions[0].name.as_str(), "b"),
                (out.regions[1].name.as_str(), "e"),
            ]
        );
        // The payload carries it, always, once the server answers.
        assert!(
            regions(&inputs(&status, &[], Some(&[]), Some(&[])))
                .machines
                .is_some()
        );
    }
}
