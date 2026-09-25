//! THE SHOP FLOOR (backlog 94c6ffd0): the region upstream of the dock,
//! where a car is still being built — the runs in flight against the
//! agents registry's capacity.

use super::*;

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
/// in flight" that does not read the steps. At the cap the floor is
/// FULL while runs keep reaching the gates and troubled when none has
/// (`out`, the rail leaving the floor — [`at_capacity`]). The trend is
/// the build duration — a run's own open-to-close, for the runs that
/// closed in each window.
pub(super) fn shop_floor(inputs: &RegionInputs<'_>, w: &Windows, out: &[OutRail]) -> Region {
    const UNIT: &str = "runs in flight";
    let bound = inputs.run_capacity.map(|c| (c, BoundKind::Capacity));
    let Some(runs) = inputs.agent_runs else {
        return region(
            "shop-floor",
            None,
            bound,
            UNIT,
            unread_settled("the agent-run packets could not be read", inputs.now),
            duration_trend("build duration", "minutes", Vec::new(), Vec::new()),
            vec![measure("runs in flight", None, "runs")],
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
    // When each finished-but-unreported run finished: its `building`
    // completion is the onset its hold is read against — the few
    // seconds before a handback lands flapped this region on 2026-09-24.
    let finished_at: Vec<Instant> = in_flight
        .iter()
        .filter(|(_, steps)| {
            step_done_at(find_step(steps, "reported", "Report recorded")).is_none()
        })
        .filter_map(|(_, steps)| step_done_at(find_step(steps, "building", "Building")))
        .collect();
    let unreported = finished_at.len();
    // A crew count is only stated where the sessions were READ: an
    // unread session list is not a floor with nobody on it, and the
    // clause is left off rather than printed as a zero.
    let crews = inputs
        .sessions
        .map(|s| plural(s.len(), "crew on the floor", "crews on the floor"));
    let count = in_flight.len();
    let mut findings = Vec::new();
    if unreported > 0 {
        findings.push(Finding::new(
            bands::SHOP_FLOOR_UNREPORTED,
            finished_at.iter().min().copied(),
            String::new(),
            format!(
                "{} finished and not reported — each holds a slot until the handback lands",
                plural(unreported, "run", "runs")
            ),
        ));
    }
    let what = [
        Some(plural(count, "run in flight", "runs in flight")),
        crews,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>()
    .join(", ");
    // AT THE CAP (design e765b3fc §4a, car F1): the floor building as
    // much as the registry allows is full while runs keep reaching the
    // gates, and stuck when none has. `shop-floor-at-cap` turned it amber
    // after 15m until 2026-09-25 — for being used. An undeclared cap
    // bounds nothing, so the rule says nothing without one.
    if let Some(cap) = inputs.run_capacity {
        findings.extend(at_capacity(
            count,
            cap,
            &what,
            onset_of_count(
                in_flight.iter().filter_map(|(j, _)| opened_at(j)).collect(),
                cap,
            ),
            out,
            w.hours,
            inputs.now,
        ));
    }
    let settled = settle(findings, what, inputs.now);
    // THE KPI (decision 9): runs in flight against the cap, and the
    // sessions silent past the crew board's idle line — each only where
    // it was read.
    let silent_after = CREW_IDLE_HOURS * 60;
    let mut kpi = vec![measure_said(
        "runs in flight",
        count_value(count),
        "runs",
        match inputs.run_capacity {
            Some(cap) => format!("{count} of {cap} runs in flight"),
            None => format!(
                "{} (no cap declared)",
                plural(count, "run in flight", "runs in flight")
            ),
        },
    )];
    if let Some(sessions) = inputs.sessions {
        let silent = sessions
            .iter()
            .filter(|s| {
                meta_instant(&s.metadata, "last_active_at")
                    .is_some_and(|beat| (inputs.now - beat).num_minutes() > silent_after)
            })
            .count();
        kpi.push(measure_said(
            "sessions silent",
            count_value(silent),
            "sessions",
            format!(
                "{} silent past {silent_after} minutes",
                plural(silent, "session", "sessions")
            ),
        ));
    }
    region("shop-floor", Some(count), bound, UNIT, settled, trend, kpi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

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

    /// Twenty-four gate-runs opened a quarter hour apart, the last at
    /// `last`: the floor's out-route (`shop-floor -> gates`) at a mean
    /// gap of an hour.
    fn gated_until(last: &str) -> Vec<Job> {
        let last = t(last);
        (0..24)
            .map(|i| {
                let at = (last - chrono::Duration::minutes(15 * i)).to_rfc3339();
                job(
                    "gate-run",
                    &format!("fix/{i}"),
                    JobStatus::Open,
                    json!({ "opened_at": at }),
                )
            })
            .collect()
    }

    /// AT THE CAP, RUNS REACHING THE GATES: FULL — the floor building as
    /// much as the registry allows, which until car F1 (design e765b3fc)
    /// turned amber after a quarter hour (`shop-floor-at-cap`). Both runs
    /// opened at 11:00, so full for an hour.
    #[test]
    fn a_floor_at_the_registrys_capacity_with_runs_reaching_the_gates_is_full() {
        let status = empty_status();
        let runs: Vec<(Job, Vec<Step>)> = (0..2)
            .map(|_| (run(true, "2026-09-19T11:00:00Z", None), Vec::new()))
            .collect();
        let sessions = vec![
            session("a@x", Some("2026-09-19T11:50:00Z")),
            session("b@x", Some("2026-09-19T09:00:00Z")),
        ];
        let gated = gated_until("2026-09-19T11:45:00Z");
        let base = inputs(&status, &[], &[], &[], &gated, Some(&[]), Some(&[]));
        let r = regions(&floor(base, &runs, &sessions, Some(2)));
        let f = by_name(&r, "shop-floor");
        assert_eq!(f.state, RegionState::Full, "{}", f.why);
        let band = f.band.as_ref().unwrap();
        assert_eq!(band.id, "full");
        assert_eq!(band.held_minutes, Some(60));
        assert!(f.why.starts_with("2 runs in flight"), "{}", f.why);
        assert!(bands::BANDS.iter().all(|b| b.id != "shop-floor-at-cap"));
        assert_eq!(f.kpi[0].text, "2 of 2 runs in flight");
        assert_eq!(
            f.kpi[1].text, "1 session silent past 60 minutes",
            "the crew board's idle line, in minutes"
        );
    }

    /// AT THE CAP, NOTHING REACHING THE GATES since 05:00 against an
    /// hourly gap: stuck, from when the four gaps ran out (09:00). Below
    /// the cap the same quiet rail says nothing.
    #[test]
    fn a_floor_at_the_cap_with_nothing_reaching_the_gates_is_stuck() {
        let status = empty_status();
        let runs: Vec<(Job, Vec<Step>)> = (0..2)
            .map(|_| (run(true, "2026-09-19T05:00:00Z", None), Vec::new()))
            .collect();
        let sessions = Vec::new();
        let gated = gated_until("2026-09-19T05:00:00Z");
        let base = inputs(&status, &[], &[], &[], &gated, Some(&[]), Some(&[]));
        let f = by_name(
            &regions(&floor(base.clone(), &runs, &sessions, Some(2))),
            "shop-floor",
        )
        .clone();
        assert_eq!(f.state, RegionState::Troubled, "{}", f.why);
        let band = f.band.unwrap();
        assert_eq!(band.id, "stuck-at-capacity");
        assert_eq!(band.since.as_deref(), Some("2026-09-19T09:00:00+00:00"));

        let f = by_name(
            &regions(&floor(base, &runs, &sessions, Some(3))),
            "shop-floor",
        )
        .clone();
        assert_eq!(f.state, RegionState::Clear, "{}", f.why);
    }

    /// THE FLAP THE REVIEW MEASURED (design 62de32ae, decision 2): a run
    /// is "finished and not reported" for the seconds between its build
    /// ending and its handback landing, and shop-floor went clear ->
    /// troubled -> clear across reads twenty seconds apart. Inside the
    /// band's ten minutes the floor stays clear and says it is settling;
    /// past them it is troubled, for as long as the record shows.
    #[test]
    fn a_run_finished_moments_ago_is_settling_and_never_flaps_the_floor_red() {
        let status = empty_status();
        let finished = |at: &str| {
            let j = run(true, "2026-09-19T09:00:00Z", None);
            let steps = vec![
                step(&j, "building", StepStatus::Completed, Some(at)),
                step(&j, "reported", StepStatus::Ready, None),
            ];
            vec![(j, steps)]
        };
        let sessions = Vec::new();
        let runs = finished("2026-09-19T11:59:40Z");
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let f = by_name(
            &regions(&floor(base, &runs, &sessions, Some(6))),
            "shop-floor",
        )
        .clone();
        assert_eq!(f.state, RegionState::Clear, "{}", f.why);
        assert!(f.why.contains("settling"), "{}", f.why);

        let runs = finished("2026-09-19T11:44:00Z");
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let f = by_name(
            &regions(&floor(base, &runs, &sessions, Some(6))),
            "shop-floor",
        )
        .clone();
        assert_eq!(f.state, RegionState::Troubled, "{}", f.why);
        let band = f.band.unwrap();
        assert_eq!(band.id, "shop-floor-unreported");
        assert_eq!(
            band.held_minutes,
            Some(16),
            "troubled for 16m, read off the record"
        );
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
