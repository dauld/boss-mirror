//! THE GARAGE: what stopped short of the track — held cars, held and
//! stranded gate-runs, runs in limbo, and a repair already in flight.

use super::*;

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

pub(super) fn garage(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
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
    let mut findings = Vec::new();
    if stranded > 0 || limbo > 0 {
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
        // A green is stranded from the moment it went green; the hold is
        // the grace in which auto-park is still filing its car.
        let since = s
            .stranded
            .iter()
            .map(|g| g.since.as_str())
            .chain(s.limbo.iter().map(|l| l.since.as_str()))
            .filter_map(stamp_instant)
            .min();
        findings.push(Finding::new(
            bands::GARAGE_STRANDED,
            since,
            String::new(),
            parts.join("; "),
        ));
    }
    let waiting_why = format!("{held} held, {red} red awaiting rework");
    if held + red > 0 {
        let since = s
            .garage
            .iter()
            .map(|g| g.since.as_str())
            .chain(s.held.iter().map(|g| g.since.as_str()))
            .chain(s.held_cars.iter().map(|h| h.car.parked_since.as_str()))
            .filter_map(stamp_instant)
            .min();
        findings.push(Finding::new(
            bands::GARAGE_WAITING,
            since,
            String::new(),
            waiting_why.clone(),
        ));
    }
    let clear_why = if count > 0 {
        waiting_why
    } else {
        "nothing held, stranded or red".to_string()
    };
    let settled = settle(findings, clear_why, inputs.now);
    let kpi = vec![measure_said(
        "reds awaiting rework",
        count_value(red),
        "cars",
        format!("{} awaiting rework", plural(red, "red", "reds")),
    )];
    region(
        "garage",
        Some(count),
        None,
        "held, stranded or red",
        settled,
        trend,
        kpi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// A stranded green — a green gate no car claims — troubles the
    /// garage; a held car alone asks for attention past its hold.
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
}
