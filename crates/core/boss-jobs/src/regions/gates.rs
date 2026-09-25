//! THE GATES: the bays in use, read against the policy's bound.

use super::*;

/// THE GATES: bays in use, of the policy's bound. Troubled when a bay
/// holds a corpse (`stale` — active past the gate Job's own deadline,
/// `yard::GATE_MAX_ACTIVE_HOURS`); attention when the bays have been
/// full, or a line for a slot has stood, past the band's hold. The
/// trend is the gate duration, opened to judged, for runs judged in
/// each window.
pub(super) fn gates(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
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
    // AT THE BOUND, OR A LINE FOR A BAY. Full bays have held since the
    // bay that filled them opened ([`onset_of_count`]); a line has
    // stood since its first run queued. Either being true is the
    // condition, so it has held since the earlier of the two.
    let full = (capacity > 0 && active >= capacity).then(|| {
        onset_of_count(
            g.active
                .iter()
                .filter_map(|a| stamp_instant(&a.since))
                .collect(),
            capacity,
        )
    });
    let line = (!g.queued.is_empty()).then(|| {
        g.queued
            .iter()
            .filter_map(|q| stamp_instant(&q.queued_at))
            .min()
    });
    if full.is_some() || line.is_some() {
        let since = [full.flatten(), line.flatten()].into_iter().flatten().min();
        let why = if g.queued.is_empty() {
            format!("{active} of {capacity} bays in use — at the bound")
        } else {
            format!(
                "{active} of {capacity} bays in use, {} waiting for a slot",
                plural(g.queued.len(), "run", "runs")
            )
        };
        findings.push(Finding::new(
            bands::GATES_AT_BOUND,
            since,
            String::new(),
            why,
        ));
    }
    let settled = settle(
        findings,
        format!("{active} of {capacity} bays in use"),
        inputs.now,
    );
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

    /// BAYS AT THE BOUND ARE CAPACITY, NOT TROUBLE — until they have been
    /// full for the band's half hour. The onset is the bay that FILLED
    /// them (the third-oldest start of three), read off the runs' own
    /// stamps, so the same rows answer the same state on every read.
    #[test]
    fn full_bays_ask_for_attention_only_after_the_band_holds() {
        let gate = |branch: &str, since: &str| crate::yard::ActiveGate {
            branch: branch.into(),
            packet_id: branch.into(),
            since: since.into(),
            stale: false,
            train: None,
        };
        let read = |starts: [&str; 3]| {
            let mut status = empty_status();
            status.gates.capacity = 3;
            status.gates.active = starts
                .iter()
                .enumerate()
                .map(|(i, s)| gate(&format!("fix/{i}"), s))
                .collect();
            let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
            by_name(&out, "gates").clone()
        };
        // Filled five minutes ago: clear, and settling.
        let g = read([
            "2026-09-19T11:00:00Z",
            "2026-09-19T11:10:00Z",
            "2026-09-19T11:55:00Z",
        ]);
        assert_eq!(g.state, RegionState::Clear, "{}", g.why);
        assert!(g.why.contains("held 5m of the 30m"), "{}", g.why);
        // Filled forty minutes ago: attention, for 40m.
        let g = read([
            "2026-09-19T11:20:00Z",
            "2026-09-19T11:00:00Z",
            "2026-09-19T11:10:00Z",
        ]);
        assert_eq!(g.state, RegionState::Attention, "{}", g.why);
        let band = g.band.unwrap();
        assert_eq!(band.id, "gates-at-bound");
        assert_eq!(band.held_minutes, Some(40));
        assert_eq!(band.since.as_deref(), Some("2026-09-19T11:20:00+00:00"));
        assert_eq!(g.bound_kind, Some(BoundKind::Capacity));
    }
}
