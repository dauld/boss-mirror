//! THE GATES: the bays in use, read against the policy's bound.

use super::*;

/// THE GATES: bays in use, of the policy's bound. Troubled when a bay
/// holds a corpse (`stale` — active past the gate Job's own deadline,
/// `yard::GATE_MAX_ACTIVE_HOURS`) or every bay is in use and no verdict
/// is landing; FULL when every bay is in use and verdicts keep landing
/// (`out`, the rails leaving the gates — [`at_capacity`]); attention when
/// the line for a bay has stood past twice the gate duration. The trend
/// is the gate duration, opened to judged, for runs judged in each
/// window.
pub(super) fn gates(inputs: &RegionInputs<'_>, w: &Windows, out: &[OutRail]) -> Region {
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
    let stale: Vec<&crate::yard::ActiveGate> = g.active.iter().filter(|a| a.stale).collect();
    let mut findings = Vec::new();
    if !stale.is_empty() {
        findings.push(Finding::new(
            bands::GATES_CORPSE,
            // A run became a corpse the moment it outlived the deadline.
            stale
                .iter()
                .filter_map(|a| stamp_instant(&a.since))
                .min()
                .map(|s| s + chrono::Duration::hours(crate::yard::GATE_MAX_ACTIVE_HOURS)),
            format!(
                "{} active past {}h",
                plural(stale.len(), "run", "runs"),
                crate::yard::GATE_MAX_ACTIVE_HOURS
            ),
            format!(
                "{} active past the gate deadline — a corpse holding a bay",
                plural(stale.len(), "run", "runs")
            ),
        ));
    }
    let what = if g.queued.is_empty() {
        format!("{active} of {capacity} bays in use")
    } else {
        format!(
            "{active} of {capacity} bays in use, {} waiting for a slot",
            plural(g.queued.len(), "run", "runs")
        )
    };
    // EVERY BAY IN USE is the gates doing their job (design e765b3fc
    // §4a, car F1): full while verdicts keep landing, stuck when they do
    // not. The bays filled when the bay that filled them opened
    // ([`onset_of_count`]). `gates-at-bound` turned this amber after 30m
    // until 2026-09-25 — most of a working day, for being used.
    findings.extend(at_capacity(
        active,
        capacity,
        &what,
        onset_of_count(
            g.active
                .iter()
                .filter_map(|a| stamp_instant(&a.since))
                .collect(),
            capacity,
        ),
        out,
        w.hours,
        inputs.now,
    ));
    // THE LINE, not the bays: worth a look only when its oldest wait has
    // stood past twice what a gate takes now, which is a line growing
    // faster than the bays drain it. A gate duration nobody measured in
    // the window judges no line — a wait against an unknown is not long.
    if let Some(service) = trend.current {
        let oldest = g
            .queued
            .iter()
            .filter_map(|q| stamp_instant(&q.queued_at))
            .min();
        #[allow(clippy::cast_possible_truncation)]
        let limit = (service * bands::LINE_PAST_SERVICE_TIMES as f64).ceil() as i64;
        if let Some(oldest) = oldest.filter(|o| (inputs.now - *o).num_minutes() > limit) {
            let waited = (inputs.now - oldest).num_minutes();
            findings.push(Finding::new(
                bands::GATES_LINE_LONG,
                Some(oldest + chrono::Duration::minutes(limit)),
                format!("oldest wait {}", bands::duration_text(waited)),
                format!(
                    "{} waiting for a bay, the oldest for {} — past {}× the {} minutes a gate takes now",
                    plural(g.queued.len(), "run", "runs"),
                    bands::duration_text(waited),
                    bands::LINE_PAST_SERVICE_TIMES,
                    number_text(service)
                ),
            ));
        }
    }
    let settled = settle(findings, what, inputs.now);
    let kpi = vec![
        measure_said(
            "bays in use",
            count_value(active),
            "bays",
            format!("{active} of {capacity} bays in use"),
        ),
        match trend.current {
            Some(m) => measure_said(
                "gate duration",
                Some(m),
                "minutes",
                format!("gates take {} minutes (median)", number_text(m)),
            ),
            None => measure_said(
                "gate duration",
                None,
                "minutes",
                format!("no gate judged in {}h", w.hours),
            ),
        },
    ];
    region(
        "gates",
        Some(active),
        Some((capacity, BoundKind::Capacity)),
        "bays in use",
        settled,
        trend,
        kpi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// The gates: a stale bay is trouble; full bays settle into attention; the trend is
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
        // The run became a corpse when it outlived the deadline: opened
        // 01:00, deadline 3h, so a corpse since 04:00 — eight hours.
        let band = gates.band.as_ref().unwrap();
        assert_eq!(band.id, "gates-corpse");
        assert_eq!(band.held_minutes, Some(8 * 60));
        assert_eq!(gates.trend.metric, "gate duration");
        assert_eq!(gates.trend.current, Some(30.0));
        assert_eq!(gates.trend.previous, Some(60.0));
        assert_eq!(gates.kpi[0].text, "1 of 3 bays in use");
        assert_eq!(gates.kpi[1].text, "gates take 30 minutes (median)");
        // The garage's trend: reds per day.
        let garage = by_name(&out, "garage");
        assert_eq!(garage.trend.metric, "reds");
        assert_eq!(garage.trend.current, Some(1.0));
        assert_eq!(garage.trend.previous, Some(1.0));
    }

    // --- full, stuck, and the line (design e765b3fc §4a, car F1) ------

    /// A red gate-run judged at `closed`, twenty minutes after it opened:
    /// one crossing of `gates -> garage`, and one gate-duration sample.
    fn red(closed: &str) -> Job {
        let closed_at = t(closed);
        let opened = (closed_at - chrono::Duration::minutes(20)).to_rfc3339();
        job(
            "gate-run",
            &format!("fix/red-{closed}"),
            JobStatus::Closed,
            json!({ "opened_at": opened, "closed_at": closed, "outcome": "failed" }),
        )
    }

    /// Twenty-four verdicts a quarter hour apart, the last at `last` — a
    /// mean gap of an hour over the day's window.
    fn verdicts_until(last: &str) -> Vec<Job> {
        let last = t(last);
        (0..24)
            .map(|i| red(&(last - chrono::Duration::minutes(15 * i)).to_rfc3339()))
            .collect()
    }

    /// The gates with every bay in use, filled by the bay that opened at
    /// `filled`, and `line` runs waiting since `queued`.
    fn every_bay(filled: &str, line: &[&str]) -> YardStatus {
        let gate = |branch: String, since: &str| crate::yard::ActiveGate {
            packet_id: branch.clone(),
            branch,
            since: since.into(),
            stale: false,
            train: None,
        };
        let mut status = empty_status();
        status.gates.capacity = 3;
        status.gates.active = ["2026-09-19T05:00:00Z", "2026-09-19T05:10:00Z", filled]
            .iter()
            .enumerate()
            .map(|(i, s)| gate(format!("fix/{i}"), s))
            .collect();
        status.gates.queued = line
            .iter()
            .enumerate()
            .map(|(i, q)| crate::yard::QueuedGate {
                branch: format!("fix/q{i}"),
                packet_id: format!("q{i}"),
                queued_at: (*q).into(),
                position: i32::try_from(i + 1).unwrap(),
                waiting_seconds: None,
                estimated_wait_seconds: None,
                train: None,
            })
            .collect();
        status
    }

    fn read_gates(status: &YardStatus, runs: &[Job]) -> Region {
        by_name(
            &regions(&inputs(status, &[], &[], &[], runs, Some(&[]), Some(&[]))),
            "gates",
        )
        .clone()
    }

    /// EVERY BAY IN USE, VERDICTS LANDING: FULL — the good state, which
    /// until car F1 turned amber after half an hour ("gates-at-bound"). A
    /// line behind it is expected; it stays a pile on the approach.
    #[test]
    fn every_bay_in_use_with_verdicts_landing_reads_full() {
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:50:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
        let band = g.band.as_ref().expect("full names its band");
        assert_eq!(band.id, "full");
        assert_eq!(
            band.held_minutes,
            Some(40),
            "full since the bay that filled it"
        );
        assert!(
            g.why
                .starts_with("3 of 3 bays in use, 1 run waiting for a slot"),
            "{}",
            g.why
        );
        assert_eq!(g.bound_kind, Some(BoundKind::Capacity));
        // The retired band cannot be named any more.
        assert!(bands::BANDS.iter().all(|b| b.id != "gates-at-bound"));
    }

    /// EVERY BAY IN USE, NO VERDICT FOR SIX HOURS against an hourly gap:
    /// stuck — ours, and not moving — dated from when the four gaps ran
    /// out after the later of the last verdict (05:45) and the fill
    /// (05:50).
    #[test]
    fn every_bay_in_use_with_no_verdict_landing_is_stuck() {
        let status = every_bay("2026-09-19T05:50:00Z", &[]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T05:45:00Z"));
        assert_eq!(g.state, RegionState::Troubled, "{}", g.why);
        let band = g.band.as_ref().unwrap();
        assert_eq!(band.id, "stuck-at-capacity");
        assert_eq!(band.since.as_deref(), Some("2026-09-19T09:50:00+00:00"));
        assert_eq!(band.held_minutes, Some(130));
    }

    /// THE LINE, NOT THE BAYS: a run waiting an hour when a gate takes
    /// twenty minutes has stood past twice the gate duration — attention,
    /// which outranks full, dated from when it crossed (11:00 + 40m).
    #[test]
    fn a_line_past_twice_the_gate_duration_asks_for_attention() {
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:00:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Attention, "{}", g.why);
        let band = g.band.as_ref().unwrap();
        assert_eq!(band.id, "gates-line-long");
        assert_eq!(band.reads, "oldest wait 1h > 2× the median gate duration");
        assert_eq!(band.since.as_deref(), Some("2026-09-19T11:40:00+00:00"));
        assert!(g.why.contains("20 minutes a gate takes now"), "{}", g.why);
        // The same line inside twice the duration asks for nothing.
        let status = every_bay("2026-09-19T11:20:00Z", &["2026-09-19T11:30:00Z"]);
        let g = read_gates(&status, &verdicts_until("2026-09-19T11:55:00Z"));
        assert_eq!(g.state, RegionState::Full, "{}", g.why);
    }

    /// Below the bound the rule says nothing: two of three bays in use is
    /// clear, however long ago the last verdict landed.
    #[test]
    fn bays_below_the_bound_are_clear_whatever_the_out_route_does() {
        let mut status = every_bay("2026-09-19T05:50:00Z", &[]);
        status.gates.active.pop();
        let g = read_gates(&status, &verdicts_until("2026-09-19T01:00:00Z"));
        assert_eq!(g.state, RegionState::Clear, "{}", g.why);
        assert!(g.band.is_none());
    }
}
