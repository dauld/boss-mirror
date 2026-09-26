//! THE PARTITION (design 62de32ae decision 4, decided 2026-09-24): every
//! packet a region counts, it counts ALONE. The eight regions whose own
//! predicates name what they hold claim first ([`claimed`]); receiving
//! and marshalling are the remainder ([`members`]).
//!
//! ONE PLACEMENT FUNCTION (design e765b3fc §2a, car R1). Which region
//! holds a packet is decided packet by packet by [`place`], over the
//! lookups one reading builds ([`Lookups`]); [`members`] is a fold of
//! `place` over every packet the reading names. So what a region counts
//! and where a packet stands are one answer — the counts, the rails and
//! the moves record (`crate::moves`) cannot disagree (CLAUDE.md §9a).
//!
//! A TRAIN IS PLACED BY ITS ACTIVE STEP, not all on the track: made up
//! at the dock, at the gates while its train gate runs, on the track once
//! it has merged ([`train_region`]). David, on feedback 84cba7e2: "like
//! how the dock routes a train over to the gates before it departs onto
//! the tracks" — and that is what the conductor does. What a train
//! carries rides it ([`Placed::Aboard`]): the cars it boarded, and its
//! own train gate while it stands at the gates, which is the train under
//! test and holds its bay as the train, counted once.

use super::*;

/// Where ONE packet stands on one reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// In a region of the map, which counts it.
    At(&'static str),
    /// Riding the open train with this id — a car it boarded, or its own
    /// train gate while the train stands at the gates. In no region of
    /// its own: the train is counted, what it carries is not.
    Aboard(String),
    /// On no region of the map this reading drew.
    Off,
}

/// WHERE A TRAIN STANDS, by the step it is at — read off the yard's own
/// phase ([`crate::yard::TrainPhase`], the one reading of a train's
/// steps), matched exhaustively so a new phase cannot go unplaced:
///
/// | phase | the train is | region |
/// |---|---|---|
/// | boarding (`collect`, `assemble`, `pr`) | being made up | dock |
/// | awaiting CI, awaiting the merge (`ci`) | under its train gate | gates |
/// | deploying, converging (`merged` done) | in transit | track |
///
/// An open train whose arrival step has completed is still in transit
/// until it closes; a closed one is in arrivals (the window's).
pub fn train_region(phase: crate::yard::TrainPhase) -> &'static str {
    use crate::yard::TrainPhase::*;
    match phase {
        Boarding => "dock",
        AwaitingCi | AwaitingMerge => "gates",
        Deploying | Converging | Arrived => "track",
    }
}

/// The step kind a protocol's admission step carries. A trigger
/// completes when the packet is admitted, so completing it takes
/// nothing in ([`taken_in`]).
pub const TRIGGER_STEP_KIND: &str = "trigger";

/// Whether a packet has been TAKEN IN: its intake step completed.
///
/// THE INTAKE STEP IS THE FIRST STEP AFTER THE TRIGGER, read off the
/// step kinds rather than named per kind (a `match` on kinds is the
/// registry the protocols already are): `triage` on a backlog-item or a
/// user-feedback, `decide` on an approval, whatever step a protocol put
/// first. The trigger completes at admission, so it takes nothing in;
/// any OTHER step completed means an actor has picked the packet up, and
/// from then on it is queued work or work in a region of its own — never
/// inbound again. A skipped step is not a completed one.
pub fn taken_in(steps: &[Step]) -> bool {
    steps
        .iter()
        .any(|s| s.status == StepStatus::Completed && s.kind != TRIGGER_STEP_KIND)
}

/// WHEN a packet was taken in: the earliest completion among the steps
/// [`taken_in`] counts — the instant it crossed from receiving into
/// marshalling, which is what the border between the two counts
/// (`borders`, design 62de32ae). `None` for a packet nothing has taken
/// in, and for one whose completion carries no stamp: an instant nobody
/// recorded is not invented.
pub(crate) fn taken_in_at(steps: &[Step]) -> Option<Instant> {
    steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed && s.kind != TRIGGER_STEP_KIND)
        .filter_map(|s| s.completed_at)
        .min()
}

/// Packet ids, ordered so a test that names an offender names it the
/// same way every run.
pub type Ids = std::collections::BTreeSet<String>;

/// Why marshalling cannot be counted when the stations were read but the
/// inbound packets were not: which of the stations' packets are still in
/// receiving is exactly what that read decides, and a count that
/// included them would be the double count this partition ends.
const MARSHALLING_NEEDS_INBOUND: &str = "the inbound packets could not be read, so which of the \
     stations' packets are still in receiving cannot be told";

/// The eight regions whose OWN predicates name what they hold — a car on
/// the dock or in the shed, a gate-run in a bay or the garage, a train at
/// the dock, the gates or on the track ([`train_region`]) or arrived, a
/// run on the shop floor, a pull request on the mirror — each as the ids
/// its predicate names, in claim order. `None` exactly where the region's
/// count is `None`: an unread region claims nothing, and says so on its
/// own card. A train's own gate, while the train stands at the gates, is
/// not a gate-run the gates claim: it rides the train ([`carried`]).
fn claimed(inputs: &RegionInputs<'_>) -> [(&'static str, Option<Ids>); 8] {
    fn ids_of<'j>(rows: impl Iterator<Item = &'j Job>) -> Ids {
        rows.map(|j| j.id.to_string()).collect()
    }
    let s = inputs.status;
    let w = Windows::of(inputs.now, inputs.window_hours);
    let trains_at = |region: &str| -> Ids {
        s.trains
            .iter()
            .filter(|t| train_region(t.phase) == region)
            .map(|t| t.id.clone())
            .collect()
    };
    let under_test = trains_at("gates");
    let garage: Ids = s
        .held_cars
        .iter()
        .map(|h| h.car.id.clone())
        .chain(s.held.iter().map(|g| g.packet_id.clone()))
        .chain(s.stranded.iter().map(|g| g.packet_id.clone()))
        .chain(s.limbo.iter().map(|l| l.packet_id.clone()))
        .chain(s.garage.iter().map(|g| g.packet_id.clone()))
        .collect();
    [
        (
            "dock",
            (inputs.dock_reading == Reading::Read).then(|| {
                s.dock
                    .iter()
                    .map(|d| d.id.clone())
                    .chain(trains_at("dock"))
                    .collect()
            }),
        ),
        (
            "gates",
            Some(
                s.gates
                    .active
                    .iter()
                    .filter(|g| !g.train.as_ref().is_some_and(|t| under_test.contains(t)))
                    .map(|g| g.packet_id.clone())
                    .chain(under_test.iter().cloned())
                    .collect(),
            ),
        ),
        ("track", Some(trains_at("track"))),
        (
            "shed",
            Some(ids_of(
                awaiting_proof(inputs.cars).into_iter().map(|(j, _)| j),
            )),
        ),
        (
            "arrivals",
            Some(ids_of(
                inputs
                    .closed_trains
                    .iter()
                    .map(|(j, _)| j)
                    .filter(|j| {
                        j.metadata.get("outcome").and_then(Value::as_str) == Some("arrived")
                    })
                    .filter(|j| closed_at(j).is_some_and(|t| w.current(t))),
            )),
        ),
        ("garage", Some(garage)),
        (
            "shop-floor",
            inputs.agent_runs.map(|runs| {
                ids_of(
                    runs.iter()
                        .map(|(j, _)| j)
                        .filter(|j| j.status == JobStatus::Open),
                )
            }),
        ),
        (
            "publish",
            inputs.publish_packets.map(|packets| {
                publish_prs(packets)
                    .into_iter()
                    .filter(PublishPr::awaiting)
                    .map(|p| p.packet_id)
                    .collect()
            }),
        ),
    ]
}

/// WHAT EACH OPEN TRAIN CARRIES, packet -> train: the cars it names in
/// `boarded_jobs`, and — while the train stands at the gates — its own
/// train gate, which is the train under test rather than a second packet
/// in the bay.
fn carried(inputs: &RegionInputs<'_>) -> std::collections::BTreeMap<String, String> {
    let s = inputs.status;
    let at_gates = |train: &str| {
        s.trains
            .iter()
            .any(|t| t.id == train && train_region(t.phase) == "gates")
    };
    let cars = inputs.open_trains.iter().flat_map(|(train, _)| {
        let id = train.id.to_string();
        train
            .metadata
            .get("boarded_jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(move |car| (car.to_string(), id.clone()))
    });
    let gates = s.gates.active.iter().filter_map(|g| {
        let train = g.train.as_ref().filter(|t| at_gates(t))?;
        Some((g.packet_id.clone(), train.clone()))
    });
    cars.chain(gates).collect()
}

/// WHAT [`place`] READS: everything one reading says about which packet
/// could stand where, built once per reading.
pub struct Lookups {
    /// The eight regions of their own, in claim order ([`claimed`]).
    claims: [(&'static str, Option<Ids>); 8],
    /// What the open trains carry ([`carried`]).
    aboard: std::collections::BTreeMap<String, String>,
    /// The open inbound packets nothing has taken in ([`taken_in`]).
    /// `None` when the inbound read failed.
    untaken: Option<Ids>,
    /// Every packet standing at a station. `None` when the station
    /// registry or the inbound read failed — without the second, which
    /// station packets are still in receiving cannot be told.
    stationed: Option<Ids>,
}

impl Lookups {
    pub fn of(inputs: &RegionInputs<'_>) -> Self {
        let untaken: Option<Ids> = inputs.inbound.map(|inbound| {
            inbound
                .iter()
                .filter(|(j, steps)| j.status == JobStatus::Open && !taken_in(steps))
                .map(|(j, _)| j.id.to_string())
                .collect()
        });
        let stationed = inputs
            .stations
            .filter(|_| untaken.is_some())
            .map(|st| st.iter().flat_map(|s| s.members.iter().cloned()).collect());
        Lookups {
            claims: claimed(inputs),
            aboard: carried(inputs),
            untaken,
            stationed,
        }
    }

    /// Whether `region` was read at all: `None` in [`members`] otherwise.
    fn read(&self, region: &str) -> bool {
        match region {
            "receiving" => self.untaken.is_some(),
            "marshalling" => self.stationed.is_some(),
            _ => self
                .claims
                .iter()
                .any(|(name, ids)| *name == region && ids.is_some()),
        }
    }

    /// Every packet any lookup names.
    fn packets(&self) -> Ids {
        self.claims
            .iter()
            .filter_map(|(_, ids)| ids.as_ref())
            .flatten()
            .chain(self.aboard.keys())
            .chain(self.untaken.iter().flatten())
            .chain(self.stationed.iter().flatten())
            .cloned()
            .collect()
    }
}

/// WHERE ONE PACKET STANDS — the placement function (design e765b3fc
/// §2a, car R1).
///
/// ORDER IS THE RULE. The eight regions of [`claimed`] are defined by
/// predicates of their own and claim their packets first, in map order;
/// then a train's cargo rides the train; then the upstream pair takes
/// the remainder. RECEIVING takes an open inbound packet not yet taken in
/// that nothing else holds; MARSHALLING takes what stands at a station
/// that neither receiving nor any other region holds. A packet matching
/// five predicates still stands in one place.
///
/// A region of its own wins because its predicate is the specific one:
/// an `agent-run` is an inbound kind by the registry's rule, and until
/// its `briefed` step completes it has taken nothing in — but it is a run
/// in flight, and the shop floor counts it. And a station's predicate
/// matches a STEP: a ready task lands on `q.platform-admin.task` whether
/// it is an untriaged item's `triage`, a triaged one's `build`, a landed
/// car's `proven` or a run's `briefed`; only the second is marshalling's.
pub fn place(packet: &str, lookups: &Lookups) -> Placed {
    let holds = |ids: &Option<Ids>| ids.as_ref().is_some_and(|ids| ids.contains(packet));
    if let Some((region, _)) = lookups.claims.iter().find(|(_, ids)| holds(ids)) {
        return Placed::At(region);
    }
    if let Some(train) = lookups.aboard.get(packet) {
        return Placed::Aboard(train.clone());
    }
    if holds(&lookups.untaken) {
        return Placed::At("receiving");
    }
    if holds(&lookups.stationed) {
        return Placed::At("marshalling");
    }
    Placed::Off
}

/// RECEIVING's packets: the open inbound packets [`place`] puts there.
/// `None` when the inbound read failed.
pub(crate) fn receiving_standing<'a>(inputs: &RegionInputs<'a>) -> Option<Vec<&'a Job>> {
    let inbound = inputs.inbound?;
    let lookups = Lookups::of(inputs);
    Some(
        inbound
            .iter()
            .filter(|(j, _)| place(&j.id.to_string(), &lookups) == Placed::At("receiving"))
            .map(|(j, _)| j)
            .collect(),
    )
}

/// MARSHALLING's view of the stations: each station with only the
/// packets [`place`] puts in marshalling — neither still in receiving,
/// nor riding a train, nor held by a region of their own. `Err` names the
/// read that failed — the station registry, or the inbound read without
/// which the line cannot be drawn.
pub(crate) fn marshalling_view(
    inputs: &RegionInputs<'_>,
) -> Result<Vec<StationReading>, &'static str> {
    let stations = inputs
        .stations
        .ok_or("the station registry could not be read")?;
    inputs.inbound.ok_or(MARSHALLING_NEEDS_INBOUND)?;
    let lookups = Lookups::of(inputs);
    Ok(stations
        .iter()
        .map(|s| StationReading {
            members: s
                .members
                .iter()
                .filter(|m| place(m, &lookups) == Placed::At("marshalling"))
                .cloned()
                .collect(),
            ..s.clone()
        })
        .collect())
}

/// WHAT EACH REGION COUNTS, by packet id, in [`REGIONS`] order — a fold
/// of [`place`] over every packet the reading names, and the partition
/// every region's `count` is pinned to (the tests
/// `no_job_id_is_counted_in_two_regions` and
/// `a_train_stands_where_its_active_step_is_and_what_it_carries_rides_it`).
/// `None` exactly where the region was not read.
pub fn members(inputs: &RegionInputs<'_>) -> Vec<(&'static str, Option<Ids>)> {
    let lookups = Lookups::of(inputs);
    let mut placed: std::collections::BTreeMap<&'static str, Ids> = Default::default();
    for packet in lookups.packets() {
        if let Placed::At(region) = place(&packet, &lookups) {
            placed.entry(region).or_default().insert(packet);
        }
    }
    REGIONS
        .iter()
        .map(|region| {
            (
                *region,
                lookups
                    .read(region)
                    .then(|| placed.remove(region).unwrap_or_default()),
            )
        })
        .collect()
}

/// How many packets the partition places in `region` — the count a
/// region whose members are not one list of its own reports (the dock,
/// the gates and the track, which trains and their cargo cross). `None`
/// where the region was not read.
pub(crate) fn count_in(inputs: &RegionInputs<'_>, region: &str) -> Option<usize> {
    members(inputs)
        .into_iter()
        .find(|(name, _)| *name == region)
        .and_then(|(_, ids)| ids.map(|ids| ids.len()))
}

/// The open trains [`train_region`] stands in `region`, with steps.
pub(crate) fn trains_in<'a>(inputs: &RegionInputs<'a>, region: &str) -> Vec<&'a (Job, Vec<Step>)> {
    inputs
        .open_trains
        .iter()
        .filter(|(j, _)| {
            let id = j.id.to_string();
            inputs
                .status
                .trains
                .iter()
                .any(|t| t.id == id && train_region(t.phase) == region)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// RECEIVING UNTIL THE INTAKE STEP COMPLETES, MARSHALLING ONLY AFTER.
    /// Measured 2026-09-24: 216 of the 232 open backlog-items had
    /// `triage` completed and waited on `build` at
    /// `q.platform-admin.task`, and every one was counted in receiving
    /// AND marshalling — "669 waiting at the borders" was 325 + 323 of
    /// largely the same packets. Both packets below stand at that one
    /// station, because a ready task lands there whether it is a triage
    /// or a build; each is counted in exactly one region.
    #[test]
    fn a_packet_is_in_receiving_until_its_intake_completes_and_in_marshalling_only_after() {
        let untriaged = admitted(
            "backlog-item",
            "untriaged",
            &[("triage", "task", StepStatus::Ready)],
        );
        let triaged = admitted(
            "backlog-item",
            "triaged",
            &[
                ("triage", "task", StepStatus::Completed),
                ("build", "task", StepStatus::Ready),
            ],
        );
        let (u, tr) = (untriaged.0.id.to_string(), triaged.0.id.to_string());
        let inbound = vec![untriaged, triaged];
        let stations = vec![holding("q.platform-admin.task", &[&u, &tr])];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&stations),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(1), "only the untriaged one: {}", r.why);
        let m = by_name(&out, "marshalling");
        assert_eq!(m.count, Some(1), "only the triaged one: {}", m.why);
        assert!(taken_in(&inbound[1].1) && !taken_in(&inbound[0].1));
        // A trigger alone takes nothing in, and neither does a step
        // skipped past.
        let (_, mut skipped) = admitted(
            "backlog-item",
            "skipped",
            &[("triage", "task", StepStatus::Ready)],
        );
        skipped[1].status = StepStatus::Skipped;
        assert!(!taken_in(&skipped));
    }

    /// A TRAIN STANDS WHERE ITS ACTIVE STEP IS (design e765b3fc §2a, car
    /// R1): made up at the dock (`collect`, `assemble`, `pr`), at the
    /// gates while its train gate runs (`ci`), on the track once it has
    /// merged. Its own gate rides with it — the train IS the thing under
    /// test, so the bay it holds is counted once, as the train — and the
    /// cars aboard ride it too. Each region's count is what the
    /// partition places there.
    #[test]
    fn a_train_stands_where_its_active_step_is_and_what_it_carries_rides_it() {
        let made_up = job("pr-train", "train made up", JobStatus::Open, json!({}));
        let made_up_steps = vec![step(&made_up, "collect", StepStatus::Ready, None)];
        let boarded_car = parked("fix/aboard", json!({}));
        let under_test = job(
            "pr-train",
            "train under test",
            JobStatus::Open,
            json!({ "boarded_jobs": [boarded_car.0.id.to_string()] }),
        );
        let under_test_steps = vec![
            step(
                &under_test,
                "collect",
                StepStatus::Completed,
                Some("2026-09-19T11:00:00Z"),
            ),
            step(
                &under_test,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&under_test, "ci", StepStatus::Ready, None),
            step(&under_test, "merged", StepStatus::Ready, None),
        ];
        let in_transit = job("pr-train", "train in transit", JobStatus::Open, json!({}));
        let in_transit_steps = vec![
            step(
                &in_transit,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T10:01:00Z"),
            ),
            step(
                &in_transit,
                "ci",
                StepStatus::Completed,
                Some("2026-09-19T10:20:00Z"),
            ),
            step(
                &in_transit,
                "merged",
                StepStatus::Completed,
                Some("2026-09-19T10:21:00Z"),
            ),
            step(&in_transit, "deployed", StepStatus::Ready, None),
        ];
        let (made_up_id, under_test_id, in_transit_id) = (
            made_up.id.to_string(),
            under_test.id.to_string(),
            in_transit.id.to_string(),
        );
        let open = vec![
            (made_up, made_up_steps),
            (under_test, under_test_steps),
            (in_transit, in_transit_steps),
        ];
        let mut status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        // Two bays in use: a car's gate, and the train's own.
        status.gates.active = vec![
            crate::yard::ActiveGate {
                branch: "fix/in-a-bay".into(),
                packet_id: "gate-car".into(),
                since: "2026-09-19T11:30:00Z".into(),
                stale: false,
                train: None,
            },
            crate::yard::ActiveGate {
                branch: "train/2026-09-19-1101".into(),
                packet_id: "gate-train".into(),
                since: "2026-09-19T11:02:00Z".into(),
                stale: false,
                train: Some(under_test_id.clone()),
            },
        ];
        let cars = vec![boarded_car.clone()];
        let i = inputs(&status, &open, &[], &cars, &[], Some(&[]), Some(&[]));

        let lookups = Lookups::of(&i);
        assert_eq!(place(&made_up_id, &lookups), Placed::At("dock"));
        assert_eq!(place(&under_test_id, &lookups), Placed::At("gates"));
        assert_eq!(place(&in_transit_id, &lookups), Placed::At("track"));
        assert_eq!(place("gate-car", &lookups), Placed::At("gates"));
        assert_eq!(
            place("gate-train", &lookups),
            Placed::Aboard(under_test_id.clone()),
            "the train's gate is the train under test, not a second packet in a bay"
        );
        assert_eq!(
            place(&boarded_car.0.id.to_string(), &lookups),
            Placed::Aboard(under_test_id.clone())
        );
        assert_eq!(place("nobody", &lookups), Placed::Off);

        // The partition still sums: every region's count is what it holds.
        let out = regions(&i);
        let held = members(&i);
        for r in &out.regions {
            let (_, ids) = held.iter().find(|(n, _)| *n == r.name).unwrap();
            assert_eq!(
                r.count,
                ids.as_ref().map(Ids::len),
                "{}: its count and its members disagree — {ids:?}",
                r.name
            );
        }
        assert_eq!(
            by_name(&out, "dock").count,
            Some(1),
            "the train being made up"
        );
        assert_eq!(
            by_name(&out, "gates").count,
            Some(2),
            "the car's bay and the train under test, its own bay counted once"
        );
        assert_eq!(by_name(&out, "track").count, Some(1), "the merged train");
    }

    /// NO JOB ID IS COUNTED IN TWO REGIONS, and every region's `count`
    /// is the size of what [`members`] says it holds — so the partition
    /// is the definition each card's number is pinned to, not a second
    /// opinion beside it (CLAUDE.md §9a).
    ///
    /// The fixture is the live overlap made total: ONE station holds a
    /// packet from every region that can stand at one — an untriaged and
    /// a triaged item, a run being briefed, a landed car's `proven`, a
    /// parked car's `review`, an open train, a gate-run in a bay, three
    /// garage lanes and a publish packet with its pull request open —
    /// because a station's predicate matches a STEP, and a ready task
    /// lands on it whoever it belongs to.
    #[test]
    fn no_job_id_is_counted_in_two_regions() {
        let untriaged = admitted(
            "backlog-item",
            "untriaged",
            &[("triage", "task", StepStatus::Ready)],
        );
        let triaged = admitted(
            "backlog-item",
            "triaged",
            &[
                ("triage", "task", StepStatus::Completed),
                ("build", "task", StepStatus::Ready),
            ],
        );
        // An agent-run is an inbound kind by the registry's rule, and
        // before `briefed` completes it has taken nothing in — but it is
        // a run in flight, and the shop floor holds it.
        let run = admitted(
            crate::agent_budget::RUN_KIND,
            "a run",
            &[("briefed", "task", StepStatus::Ready)],
        );
        // A publish packet whose pull request is open on the mirror.
        let publish = {
            let (j, mut steps) = admitted(
                PUBLISH_KIND,
                "Publish to the public mirror",
                &[("open-pr", "task", StepStatus::Completed)],
            );
            steps[1].metadata = json!({ "pr_url": "https://mirror/pull/9" });
            (j, steps)
        };
        let shed_car = {
            let j = job(
                "ship-a-change",
                "fix/landed",
                JobStatus::Open,
                json!({ "branch": "fix/landed" }),
            );
            let s = vec![step(&j, "proven", StepStatus::Ready, None)];
            (j, s)
        };
        let dock_car = parked("fix/parked", json!({}));
        let train = job("pr-train", "train #9", JobStatus::Open, json!({}));
        let train_steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&train, "ci", StepStatus::Ready, None),
        ];
        let open_trains = vec![(train, train_steps)];
        let arrived = job(
            "pr-train",
            "train #8",
            JobStatus::Closed,
            json!({ "outcome": "arrived", "closed_at": "2026-09-19T09:00:00Z" }),
        );
        let closed_trains = vec![(arrived, Vec::new())];

        let mut status = build_status_for(
            YardInputs {
                open_trains: &open_trains,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        status.dock = vec![crate::yard::dock_car(&dock_car.0)];
        status.gates.active = vec![crate::yard::ActiveGate {
            branch: "fix/in-a-bay".into(),
            packet_id: "gate-in-a-bay".into(),
            since: "2026-09-19T11:30:00Z".into(),
            stale: false,
            train: None,
        }];
        status.held = vec![crate::yard::HeldGreen {
            branch: "feat/held".into(),
            reason: "not yet".into(),
            since: "2026-09-19T02:00:00Z".into(),
            packet_id: "gate-held".into(),
            sha: None,
        }];
        status.limbo = vec![crate::yard::LimboCar {
            branch: "fix/lost".into(),
            verdict: "lost".into(),
            since: "2026-09-19T02:00:00Z".into(),
            packet_id: "gate-lost".into(),
            sha: None,
        }];
        status.garage = vec![crate::yard::GaragedCar {
            branch: "fix/red".into(),
            failed_check: None,
            failed_line: None,
            since: "2026-09-19T02:00:00Z".into(),
            packet_id: "gate-red".into(),
            sha: None,
        }];

        let id = |j: &Job| j.id.to_string();
        let everyone = [
            id(&untriaged.0),
            id(&triaged.0),
            id(&run.0),
            id(&publish.0),
            id(&shed_car.0),
            id(&dock_car.0),
            id(&open_trains[0].0),
            "gate-in-a-bay".to_string(),
            "gate-held".to_string(),
            "gate-lost".to_string(),
            "gate-red".to_string(),
        ];
        let at_the_station: Vec<&str> = everyone.iter().map(String::as_str).collect();
        let stations = vec![holding("q.platform-admin.task", &at_the_station)];
        let inbound = vec![
            untriaged.clone(),
            triaged.clone(),
            run.clone(),
            publish.clone(),
        ];
        let runs = vec![run.clone()];
        let publishes = vec![publish.clone()];
        let cars = vec![shed_car.clone(), dock_car.clone()];
        let mut i = inputs(
            &status,
            &open_trains,
            &closed_trains,
            &cars,
            &[],
            Some(&inbound),
            Some(&stations),
        );
        i.agent_runs = Some(&runs);
        i.publish_packets = Some(&publishes);

        let out = regions(&i);
        let held = members(&i);
        assert_eq!(
            held.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            REGIONS,
            "one entry per region, in map order"
        );

        // No id twice — naming the id and both regions when it drifts.
        let mut owner: std::collections::BTreeMap<&str, &str> = Default::default();
        for (region_name, ids) in &held {
            for pid in ids.iter().flatten() {
                if let Some(first) = owner.insert(pid.as_str(), region_name) {
                    panic!("{pid} is counted in {first} AND {region_name}");
                }
            }
        }
        // Every region's count IS its members.
        for r in &out.regions {
            let (_, ids) = held.iter().find(|(n, _)| *n == r.name).unwrap();
            assert_eq!(
                r.count,
                ids.as_ref().map(Ids::len),
                "{}: its count and its members disagree — {ids:?}",
                r.name
            );
        }
        // And the station's packets are all accounted for, each once.
        for pid in &everyone {
            assert!(owner.contains_key(pid.as_str()), "{pid} is in no region");
        }
        let region_of = |pid: &str| owner.get(pid).copied();
        assert_eq!(region_of(&id(&untriaged.0)), Some("receiving"));
        assert_eq!(region_of(&id(&triaged.0)), Some("marshalling"));
        assert_eq!(region_of(&id(&run.0)), Some("shop-floor"));
        assert_eq!(region_of(&id(&publish.0)), Some("publish"));
        assert_eq!(region_of(&id(&shed_car.0)), Some("shed"));
        assert_eq!(region_of(&id(&dock_car.0)), Some("dock"));
        assert_eq!(region_of("gate-in-a-bay"), Some("gates"));
        assert_eq!(region_of("gate-red"), Some("garage"));

        // The two borders out of the upstream pair wait on the same
        // partition: one packet each, never the sum of the overlap.
        let b = crate::borders::borders(&crate::borders::BorderInputs {
            regions: &i,
            firings: Some(&[]),
            dispatcher_firings: Some(&[]),
        });
        let waiting = |from: &str| {
            b.borders
                .iter()
                .find(|x| x.from == from && x.to != "garage")
                .and_then(|x| x.waiting)
        };
        assert_eq!(waiting("receiving"), Some(1));
        assert_eq!(waiting("marshalling"), Some(1));
    }
}
