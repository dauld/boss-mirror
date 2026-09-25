//! ARRIVALS: trains that arrived in the window — and a cancelled train
//! whose cars are still awaiting repair.

use super::*;

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
/// none and is not trouble) — still has a car [`awaiting_repair`]; clear
/// (arrivals arriving) while an arrival's siding is still converging. The trend is
/// arrivals per day; a cancellation stays in the record, not the state.
pub(super) fn arrivals(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
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
    let mut findings = Vec::new();
    if !red.is_empty() {
        findings.push(Finding::new(
            bands::ARRIVALS_RED_UNREPAIRED,
            None,
            String::new(),
            format!(
                "{} cancelled with cars aboard still awaiting repair: {}",
                plural(red.len(), "train", "trains"),
                red.join(", ")
            ),
        ));
    }
    // Sidings still converging are arrivals ARRIVING — good news, and
    // clear (decision 1); the review found them painted amber.
    let clear_why = if converging > 0 {
        format!(
            "{} in {}h, {} still converging",
            plural(cur, "train arrived", "trains arrived"),
            w.hours,
            plural(converging, "siding", "sidings")
        )
    } else {
        format!(
            "{} in {}h",
            plural(cur, "train arrived", "trains arrived"),
            w.hours
        )
    };
    let settled = settle(findings, clear_why, inputs.now);
    // THE KPI (decision 9): trains AND cars, each in its own unit — "35
    // arrivals" beside "17 landed" said neither which was which. A car
    // landed in the window is a landed car (`car::is_landed`) that
    // closed inside it.
    let landed = inputs
        .cars
        .iter()
        .filter(|(j, _)| serde_json::to_value(j).is_ok_and(|v| crate::car::is_landed(&v)))
        .filter_map(|(j, _)| closed_at(j))
        .filter(|t| w.current(*t))
        .count();
    let kpi = vec![
        measure_said(
            "trains arrived",
            count_value(cur),
            "trains",
            format!(
                "{} in {}h",
                plural(cur, "train arrived", "trains arrived"),
                w.hours
            ),
        ),
        measure_said(
            "cars landed",
            count_value(landed),
            "cars",
            format!(
                "{} in {}h",
                plural(landed, "car landed", "cars landed"),
                w.hours
            ),
        ),
    ];
    region(
        "arrivals",
        Some(cur),
        None,
        "trains arrived",
        settled,
        trend,
        kpi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

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
}
