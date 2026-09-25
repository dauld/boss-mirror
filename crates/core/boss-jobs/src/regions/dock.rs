//! THE DOCK: cars gated green and parked, waiting for the boarding
//! depth — and the ordering edges that can hold one back, judged by the
//! conductor's own function (backlog 4142d821, design cf820810 Q7).

use super::*;

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
/// behind X)" and asks for attention (past its hold) rather than promising a train. An edge that
/// can NEVER clear — an abandoned or missing predecessor — troubles the
/// region: the conductor refuses it identically every window until a
/// person acts, and a troubled thing must look troubled.
pub(super) fn dock(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
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
    // The dock's bound is the BOARDING DEPTH — a threshold at which a
    // train is due, not room for that many cars (decision 5).
    let bound = status
        .boarding
        .dock_threshold
        .and_then(|t| usize::try_from(t).ok())
        .map(|b| (b, BoundKind::Threshold));
    const UNIT: &str = "cars parked";
    match inputs.dock_reading {
        Reading::Unread => region(
            "dock",
            None,
            bound,
            UNIT,
            unread_settled("the loading-dock station row could not be read", inputs.now),
            trend,
            vec![measure("cars that cannot board", None, "cars")],
        ),
        Reading::Read => {
            use crate::car::{EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome};
            let depth = status.dock.len();
            let parked = plural(depth, "car parked", "cars parked");
            let edges = dock_edges(inputs);
            let held: Vec<(&crate::yard::DockCar, &crate::car::EdgeHold)> = edges
                .iter()
                .filter_map(|(car, o)| match o {
                    EdgeOutcome::Hold(h) => Some((*car, h)),
                    _ => None,
                })
                .collect();
            let holds: Vec<&crate::car::EdgeHold> = held.iter().map(|(_, h)| *h).collect();
            let unjudged = edges
                .iter()
                .filter(|(_, o)| matches!(o, EdgeOutcome::BoardUnjudged(_)))
                .count();
            let waiting = behind_of(&holds, EDGE_HOLD_WAITING);
            let stuck = behind_of(&holds, EDGE_HOLD_NEEDS_HUMAN);
            // A held car has been unable to board at least since it
            // parked — the car's own `gate` completion, else the dock
            // row's stamp.
            let parked_at = |car: &crate::yard::DockCar| {
                inputs
                    .cars
                    .iter()
                    .find(|(j, _)| j.id.to_string() == car.id)
                    .and_then(|(_, s)| {
                        step_done_at(find_step(s, crate::car::GATE_SLUG, crate::car::GATE))
                    })
                    .or_else(|| stamp_instant(&car.parked_since))
            };
            let onset = |kind: &str| {
                held.iter()
                    .filter(|(_, h)| h.kind == kind)
                    .filter_map(|(car, _)| parked_at(car))
                    .min()
            };
            let mut findings = Vec::new();
            if !stuck.is_empty() {
                findings.push(Finding::new(
                    bands::DOCK_EDGE_NEVER_CLEARS,
                    onset(EDGE_HOLD_NEEDS_HUMAN),
                    String::new(),
                    format!(
                        "{parked}, {} cannot board — stuck behind {}, an edge that can never be \
                         satisfied; a human must clear it",
                        holds.len(),
                        stuck.join(", ")
                    ),
                ));
            }
            if !waiting.is_empty() {
                findings.push(Finding::new(
                    bands::DOCK_CANNOT_BOARD,
                    onset(EDGE_HOLD_WAITING),
                    String::new(),
                    format!(
                        "{parked}, {} cannot board (waiting behind {})",
                        holds.len(),
                        waiting.join(", ")
                    ),
                ));
            }
            // A TRAIN DUE IS CLEAR (decision 1): the boarding depth met is
            // the designed state two minutes after any departure, and it
            // painted the dock amber on every read.
            let clear_why = if !holds.is_empty() {
                format!("{parked}, {} waiting behind an edge", holds.len())
            } else if status.boarding.threshold_met == Some(true) {
                format!("{parked} — the boarding depth is met, a train is due")
            } else {
                parked
            };
            let settled = settle(findings, clear_why, inputs.now);
            // Unknown is not zero: an edge nobody could judge boards (the
            // conductor fails open on it too), and the region says so.
            let settled = if unjudged > 0 {
                Settled {
                    why: format!(
                        "{} · {} could not be read — the conductor boards {} anyway",
                        settled.why,
                        plural(unjudged, "ordering edge", "ordering edges"),
                        if unjudged == 1 {
                            "its car"
                        } else {
                            "their cars"
                        }
                    ),
                    ..settled
                }
            } else {
                settled
            };
            region(
                "dock",
                Some(depth),
                bound,
                UNIT,
                settled,
                trend,
                vec![measure_said(
                    "cars that cannot board",
                    count_value(holds.len()),
                    "cars",
                    format!("{} cannot board", plural(holds.len(), "car", "cars")),
                )],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

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

    /// THE INCIDENT'S SURFACE (backlog 4142d821, design cf820810 Q7).
    /// Three cars sat unable to board for nine and a half hours while
    /// this region said "the boarding depth is met, a train is due" —
    /// the sentence it says two minutes after a healthy departure —
    /// because only the conductor ever asked whether a car could board.
    /// A car held on its declared ordering edge is counted, named by
    /// what it waits behind, and — once it has stood past the band's
    /// hold — the region asks for ATTENTION without promising a train.
    #[test]
    fn a_car_waiting_behind_its_edge_asks_for_attention_and_promises_no_train() {
        let first = parked("fix/first-half", json!({}));
        let pred_id = first.0.id.to_string();
        let second = parked("fix/second-half", json!({ "boards_after": pred_id }));
        let cars = vec![first.clone(), second];
        let mut status = dock_at_depth(&cars);
        status.boarding.dock_threshold = Some(2);
        let preds = vec![(
            pred_id.clone(),
            crate::car::Predecessor::Found(packet_value(&first.0, &first.1)),
        )];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let dock = by_name(&out, "dock");
        // Parked since the dock row's date (no `gate` stamp on this car),
        // so twelve hours past the 60m hold.
        assert_eq!(dock.state, RegionState::Attention, "{}", dock.why);
        assert_eq!(dock.count, Some(2), "parked is still parked");
        let behind = format!("fix/first-half (car {})", &pred_id[..8]);
        assert_eq!(
            dock.why,
            format!("2 cars parked, 1 cannot board (waiting behind {behind})")
        );
        let band = dock.band.as_ref().expect("the band that decided it");
        assert_eq!(band.id, "dock-cannot-board");
        assert_eq!(band.held_minutes, Some(12 * 60));
        // The boarding depth is a THRESHOLD, not room for two cars.
        assert_eq!(dock.bound, Some(2));
        assert_eq!(dock.bound_kind, Some(BoundKind::Threshold));
        assert_eq!(dock.kpi[0].text, "1 car cannot board");
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
        // A train due is the dock WORKING (design 62de32ae, decision 1).
        assert_eq!(dock.state, RegionState::Clear, "{}", dock.why);
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

    /// HYSTERESIS, READ FROM THE RECORD (design 62de32ae, decision 2). A
    /// car that parked twenty minutes ago behind a predecessor still in
    /// flight has not yet held the dock band's hour: the region stays
    /// clear and SAYS the condition is settling, with how long of how
    /// long — never hidden, never flipped early.
    #[test]
    fn a_car_held_inside_the_bands_hold_is_settling_and_the_dock_stays_clear() {
        let first = parked("fix/first-half", json!({}));
        let pred_id = first.0.id.to_string();
        let (j, mut s) = parked("fix/second-half", json!({ "boards_after": pred_id }));
        s.push(step(
            &j,
            crate::car::GATE_SLUG,
            StepStatus::Completed,
            Some("2026-09-19T11:40:00Z"),
        ));
        let cars = vec![first.clone(), (j, s)];
        let status = dock_at_depth(&cars);
        let preds = vec![(
            pred_id,
            crate::car::Predecessor::Found(packet_value(&first.0, &first.1)),
        )];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let dock = by_name(&out, "dock");
        assert_eq!(dock.state, RegionState::Clear, "{}", dock.why);
        assert!(dock.band.is_none());
        assert!(
            dock.why.contains("settling: 2 cars parked, 1 cannot board")
                && dock.why.contains("held 20m of the 60m it must hold"),
            "{}",
            dock.why
        );
    }
}
