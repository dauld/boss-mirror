//! THE PARTITION (design 62de32ae decision 4, decided 2026-09-24): every
//! packet a region counts, it counts ALONE. The eight regions whose own
//! predicates name what they hold claim first ([`claimed`]); receiving
//! and marshalling are the remainder ([`members`]).

use super::*;

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
/// the dock or in the shed, a gate-run in a bay or the garage, a train on
/// the track or arrived, a run on the shop floor, a pull request on the
/// mirror — each as the ids its count counts. `None` exactly where the
/// region's count is `None`: an unread region claims nothing, and says so
/// on its own card.
fn claimed(inputs: &RegionInputs<'_>) -> [(&'static str, Option<Ids>); 8] {
    fn ids_of<'j>(rows: impl Iterator<Item = &'j Job>) -> Ids {
        rows.map(|j| j.id.to_string()).collect()
    }
    let s = inputs.status;
    let w = Windows::of(inputs.now, inputs.window_hours);
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
            (inputs.dock_reading == Reading::Read)
                .then(|| s.dock.iter().map(|d| d.id.clone()).collect()),
        ),
        (
            "gates",
            Some(s.gates.active.iter().map(|g| g.packet_id.clone()).collect()),
        ),
        (
            "track",
            Some(s.trains.iter().map(|t| t.id.clone()).collect()),
        ),
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

/// Every id the eight regions of [`claimed`] hold between them.
fn claimed_ids(inputs: &RegionInputs<'_>) -> Ids {
    claimed(inputs)
        .into_iter()
        .filter_map(|(_, ids)| ids)
        .flatten()
        .collect()
}

/// RECEIVING's packets: the open inbound packets nothing has taken in
/// ([`taken_in`]) that no region of their own holds. `None` when the
/// inbound read failed.
///
/// A region of its own wins because its predicate is the specific one:
/// an `agent-run` is an inbound kind by the registry's rule, and until
/// its `briefed` step completes it has taken nothing in — but it is a run
/// in flight, and the shop floor counts it.
pub(crate) fn receiving_standing<'a>(inputs: &RegionInputs<'a>) -> Option<Vec<&'a Job>> {
    let inbound = inputs.inbound?;
    let elsewhere = claimed_ids(inputs);
    Some(
        inbound
            .iter()
            .filter(|(j, steps)| {
                j.status == JobStatus::Open
                    && !taken_in(steps)
                    && !elsewhere.contains(&j.id.to_string())
            })
            .map(|(j, _)| j)
            .collect(),
    )
}

/// MARSHALLING's view of the stations: each station with only the
/// packets that are marshalling's — neither still in receiving nor held
/// by a region of their own. A station's predicate matches a STEP, and a
/// ready task lands on `q.platform-admin.task` whether it is an
/// untriaged item's `triage`, a triaged one's `build`, a landed car's
/// `proven` or a run's `briefed`; only the second is waiting to be
/// dispatched. `Err` names the read that failed — the station registry,
/// or the inbound read without which the line cannot be drawn.
pub(crate) fn marshalling_view(
    inputs: &RegionInputs<'_>,
) -> Result<Vec<StationReading>, &'static str> {
    let stations = inputs
        .stations
        .ok_or("the station registry could not be read")?;
    let receiving = receiving_standing(inputs).ok_or(MARSHALLING_NEEDS_INBOUND)?;
    let mut elsewhere = claimed_ids(inputs);
    elsewhere.extend(receiving.iter().map(|j| j.id.to_string()));
    Ok(stations
        .iter()
        .map(|s| StationReading {
            members: s
                .members
                .iter()
                .filter(|m| !elsewhere.contains(*m))
                .cloned()
                .collect(),
            ..s.clone()
        })
        .collect())
}

/// WHAT EACH REGION COUNTS, by packet id, in [`REGIONS`] order — the
/// partition every region's `count` is pinned to (the test
/// `no_job_id_is_counted_in_two_regions`). `None` exactly where the
/// region's count is `None`.
///
/// ORDER IS THE RULE. The eight regions of [`claimed`] are defined by
/// predicates of their own and claim their packets first; the upstream
/// pair is the remainder. RECEIVING takes the open inbound packets not
/// yet taken in that nothing else holds ([`receiving_standing`]);
/// MARSHALLING takes what stands at a station that neither receiving nor
/// any other region holds ([`marshalling_view`]). A packet matching five
/// predicates is still counted once.
pub fn members(inputs: &RegionInputs<'_>) -> Vec<(&'static str, Option<Ids>)> {
    let mut all: Vec<(&'static str, Option<Ids>)> = claimed(inputs).into_iter().collect();
    all.push((
        "receiving",
        receiving_standing(inputs).map(|js| js.iter().map(|j| j.id.to_string()).collect()),
    ));
    all.push((
        "marshalling",
        marshalling_view(inputs)
            .ok()
            .map(|st| st.into_iter().flat_map(|s| s.members).collect()),
    ));
    REGIONS
        .iter()
        .filter_map(|name| all.iter().find(|(n, _)| n == name).cloned())
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
