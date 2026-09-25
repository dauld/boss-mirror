//! THE SHED: landed cars awaiting proof, and the three places they stand
//! in — `boss orient`'s `shed_place`, moved here so the CLI and the read
//! share one classification.

use super::*;

/// Where a landed, unproven car stands — the yard's inspection shed
/// read in words (`apps/web/src/it/yard/yard-shed.ts`, `shedPlace`).
/// The three places are disjoint and total, the probe winning over an
/// event (a probe can be run; an event has to happen). `Unproven` is
/// the forgotten case, and the only troubled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShedPlace {
    /// A probe is recorded; `last` is the failed attempt's `why`, if
    /// any (a succeeding attempt completes the step and the car leaves).
    ProbePending { last: Option<String> },
    /// The probe ran and exited 75: NOT YET — early, not wrong.
    ProbeNotYet { said: String },
    /// Only an event is recorded: prose naming what has to happen.
    WaitingOn(String),
    /// Neither — no probe, no event; nothing mechanical can settle it.
    Unproven,
}

/// Classify a car's metadata into its shed place.
pub fn shed_place(md: &Value) -> ShedPlace {
    let probe = md_str(md, crate::car::PROOF_PROBE);
    let event = md_str(md, crate::car::PROOF_EVENT);
    if !probe.is_empty() {
        let attempt = md.get("proof_attempt");
        let not_yet = attempt.is_some_and(crate::car::attempt_said_not_yet);
        let last = attempt
            .and_then(|a| a.get("why"))
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        if not_yet {
            return ShedPlace::ProbeNotYet {
                said: last.unwrap_or_else(|| "the probe said not yet".to_string()),
            };
        }
        return ShedPlace::ProbePending { last };
    }
    if !event.is_empty() {
        return ShedPlace::WaitingOn(event.to_string());
    }
    ShedPlace::Unproven
}

/// The cars standing in the inspection shed: open cars whose live step
/// is `proven`. ONE predicate (CLAUDE.md §9a) — the shed region reads
/// it for its count, and the arrivals -> shed border
/// (`crate::borders`) reads it for what is waiting to cross.
pub(crate) fn awaiting_proof(cars: &[(Job, Vec<Step>)]) -> Vec<&(Job, Vec<Step>)> {
    cars.iter()
        .filter(|(j, s)| {
            j.status == boss_core::job::JobStatus::Open
                && s.iter()
                    .find(|st| matches!(st.status, StepStatus::Ready | StepStatus::Active))
                    .is_some_and(|st| st.spec_slug.as_deref() == Some("proven"))
        })
        .collect()
}

/// THE SHED: landed cars awaiting proof — open cars whose live step is
/// `proven`. Troubled when one is UNPROVEN (no probe, no event: nothing
/// mechanical can settle it) or its probe is FAILING, or when a car
/// past [`PROOF_STALE_HOURS`] waits on something that is OURS; attention
/// when a car is past the band and none is ours, clear while none is past it. The trend is cars proven per day.
///
/// TROUBLED MEANS OURS (backlog 3881f5c9). Until 2026-09-23 every stale
/// car troubled the shed, so it read red while its own words said
/// "waiting on the world, not on us" — six honest waits (a Stripe
/// charge, a release David opens, his destructive prune, a tenant
/// publish, a red crawl, a failed publish) painted exactly like a broken
/// probe. A stale wait is someone else's move only when the car
/// DECLARES it and names the OWNER, and — for a wait on the world —
/// something OBSERVES it (`boss_jobs::car::owned_wait`; a named actor's
/// act needs no observer, 2026-09-24); it stays theirs until the event is
/// seen while the probe still says not yet, or it outlives a max wait
/// the car itself declared.
pub(super) fn shed(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let awaiting = awaiting_proof(inputs.cars);
    let mut unproven = Vec::new();
    let mut failing = Vec::new();
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        match shed_place(&j.metadata) {
            ShedPlace::Unproven => unproven.push(branch),
            ShedPlace::ProbePending { last: Some(_) } => failing.push(branch),
            _ => {}
        }
    }
    let proven = inputs
        .cars
        .iter()
        .filter_map(|(_, s)| step_done_at(find_step(s, "proven", "Proven in production")));
    let (cur, prev) = count_split(w, proven);
    let trend = rate_trend("proven", w, cur, prev);
    // STALE: awaiting proof for longer than a day, whatever its place.
    // Collected over every awaiting car rather than only the `not yet`
    // ones, so an old car is named even when its place would otherwise
    // read as healthy progress.
    let mut stale: Vec<(i64, &str)> = Vec::new();
    // Of the stale ones, how many have NEVER had their probe run. A
    // probe that ran and said `not yet` is the world answering; a probe
    // with no attempt on record is us not asking. Same age, opposite
    // meaning, and until 2026-09-22 the same sentence.
    let mut never_probed = 0usize;
    // Of the stale ones that were told not yet, those told so without a
    // break for longer than [`NOT_YET_STARVED_HOURS`] — a probe that
    // cannot pass looks exactly like this, so it is ours to read, not
    // the world's to answer (adef5ddf). Longest streak first.
    let mut starved: Vec<(crate::car::NotYetStreak, &str)> = Vec::new();
    // Of those told not yet, the ones whose OWN declared wait is already
    // in the record (b461341d) — the probe cannot see what the car said
    // it was waiting for, which is ours at any streak length. A declared
    // wait not yet seen lands in neither list: its patience is stated.
    let mut seen: Vec<(String, &str)> = Vec::new();
    // Of those told not yet, the ones that DECLARED a wait with no
    // `seen` check (e9b164a1). The declaration exempts them from the
    // streak bound and hands the judgement to that check — so without
    // one nothing can ever say the event arrived, and the exemption is
    // an escape hatch that silences the label forever. Writing the
    // check is ours, so they are counted, not folded into the world's.
    let mut unobserved: Vec<&str> = Vec::new();
    // WHOSE MOVE (3881f5c9): of those told not yet, the ones with NO
    // declared wait (not yet starved — nothing on the car says whose
    // move it is), the ones that declared and observe a wait but name no
    // owner, the ones past the max wait they declared, and — the only
    // ones that are not ours — those waiting on their declared owner.
    let mut undeclared: Vec<&str> = Vec::new();
    let mut prose_only: Vec<&str> = Vec::new();
    let mut unowned: Vec<&str> = Vec::new();
    let mut overdue: Vec<(crate::car::OwnedWait, i64, &str)> = Vec::new();
    let mut theirs: Vec<(crate::car::OwnedWait, &str)> = Vec::new();
    // The age of the oldest stale car that is OURS — what the ours band
    // measures. Until 2026-09-24 it read the oldest car of all: "oldest
    // open 168h > the 24h proof band, ours to move", held 6d, where the
    // 168h car was a Stripe wait on the world and the one car that was
    // ours was 40h old (3881f5c9).
    let mut oldest_ours: Option<i64> = None;
    for (j, _) in &awaiting {
        let branch = j
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or(j.title.as_str());
        let Some(opened) = j
            .metadata
            .get("opened_at")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        else {
            continue;
        };
        let hours = (inputs.now - opened.with_timezone(&chrono::Utc)).num_hours();
        if hours >= PROOF_STALE_HOURS {
            stale.push((hours, branch));
            let class = stale_proof(&j.metadata, hours);
            if !matches!(
                class,
                StaleProof::Theirs(_) | StaleProof::Unproven | StaleProof::Failing
            ) {
                oldest_ours = oldest_ours.max(Some(hours));
            }
            match class {
                // Named by the sentences above this one, which lead
                // whenever any car is unproven or failing.
                StaleProof::Unproven | StaleProof::Failing => {}
                StaleProof::NeverProbed => never_probed += 1,
                StaleProof::ProseOnly => prose_only.push(branch),
                StaleProof::Starved(streak) => starved.push((streak, branch)),
                StaleProof::Seen(on) => seen.push((on, branch)),
                StaleProof::Unobserved => unobserved.push(branch),
                StaleProof::Undeclared => undeclared.push(branch),
                StaleProof::Unowned => unowned.push(branch),
                StaleProof::Overdue(o) => overdue.push((o, hours, branch)),
                StaleProof::Theirs(o) => theirs.push((o, branch)),
            }
        }
    }
    stale.sort_by_key(|(hours, _)| std::cmp::Reverse(*hours));
    starved.sort_by_key(|(s, _)| std::cmp::Reverse(s.hours));

    let n = awaiting.len();
    // THE KPI (decision 9): cars owed a proof whose move is OURS — the
    // unproven, the failing, and every stale car whose wait is not a
    // declared, observed, owned one.
    let ours_stale = never_probed
        + starved.len()
        + seen.len()
        + unobserved.len()
        + undeclared.len()
        + prose_only.len()
        + unowned.len()
        + overdue.len();
    let ours = unproven.len() + failing.len() + ours_stale;
    let kpi = vec![measure_said(
        "cars owed a proof that are ours",
        count_value(ours),
        "cars",
        format!(
            "{} owed a proof {} ours",
            plural(ours, "car", "cars"),
            if ours == 1 { "is" } else { "are" }
        ),
    )];
    // When the stale band was crossed: a car crosses it PROOF_STALE_HOURS
    // after it opened, so the oldest stale car's crossing is the onset.
    let stale_since = stale
        .first()
        .map(|(hours, _)| inputs.now - chrono::Duration::hours(*hours - PROOF_STALE_HOURS));
    // Which band the shed's sentence falls under, and its onset. The
    // unproven and the failing carry no onset the record holds — the
    // car's landing is not stamped on it — so they are stated at once.
    let (decided_by, why): (Option<(bands::Band, Option<Instant>, String)>, String) = if !unproven
        .is_empty()
    {
        (
            Some((bands::SHED_UNPROVEN, None, String::new())),
            format!("UNPROVEN — no probe, no event: {}", unproven.join(", ")),
        )
    } else if !failing.is_empty() {
        (
            Some((bands::SHED_FAILING, None, String::new())),
            format!("probe FAILING: {}", failing.join(", ")),
        )
    } else if let Some((oldest, branch)) = stale.first().copied() {
        // The age is the finding, so it leads — and the oldest car is
        // named, because "9 awaiting proof" sends a reader to a list
        // while "105h, fix/x" sends them to a car.
        // WHOSE MOVE IS IT. The age says something is stuck; this says
        // whether anyone here can unstick it, and since 3881f5c9 it
        // also decides the colour: only a declared, observed, owned
        // wait is someone else's move, and a shed of nothing else is
        // attention, not troubled. Every other answer is ours, each named
        // for what there is to do — run the probe, read one that cannot
        // pass (adef5ddf), read one blind to its own event (b461341d),
        // write the seen check (e9b164a1), or declare whose move it is.
        let mut ours: Vec<String> = Vec::new();
        if never_probed > 0 {
            ours.push(format!(
                "{never_probed} never probed — nothing has run their proof, which is on us"
            ));
        }
        if let Some((longest, branch)) = starved.first() {
            ours.push(format!(
                "{} told not yet without a break past {NOT_YET_STARVED_HOURS}h — longest {}h \
                 over {} runs, {branch}; a probe that cannot pass says exactly this, so it is \
                 ours to read, not the world's",
                starved.len(),
                longest.hours,
                longest.runs
            ));
        }
        if let Some((on, branch)) = seen.first() {
            ours.push(format!(
                "{} told not yet AFTER what they declared they wait on was seen in the record \
                 — {branch}, waiting on {on}; the probe cannot see its own event, so it is ours \
                 to read",
                seen.len()
            ));
        }
        if let Some(branch) = unobserved.first() {
            ours.push(format!(
                "{} declared a wait with no seen check — {branch}; nothing can ever say its \
                 event arrived, so writing one is ours (boss car waits-on --seen)",
                unobserved.len()
            ));
        }
        if let Some(branch) = undeclared.first() {
            ours.push(format!(
                "{} told not yet with no declared wait — {branch}; saying whose move it is \
                 is ours (boss car waits-on)",
                undeclared.len()
            ));
        }
        if let Some(branch) = prose_only.first() {
            ours.push(format!(
                "{} wait on an event only prose names, with no probe — {branch}; nothing \
                 observes it, so it is ours",
                prose_only.len()
            ));
        }
        if let Some(branch) = unowned.first() {
            ours.push(format!(
                "{} declared no owner for their wait — {branch}; naming one (world, or the \
                 actor whose act it is) is ours",
                unowned.len()
            ));
        }
        if let Some((o, hours, branch)) = overdue.first() {
            ours.push(format!(
                "{} past the max wait they declared — {branch}, waiting on {}: {}, {hours}h \
                 against {}h; ours to read",
                overdue.len(),
                o.owner,
                o.on,
                o.max_wait_hours.unwrap_or_default()
            ));
        }
        // Named one by one: a wait on the world and a wait on David's
        // act are different errands, and "6 waiting" sends no one to
        // either.
        let waits = theirs
            .iter()
            .map(|(o, branch)| format!("waiting on {}: {} ({branch})", o.owner, o.on))
            .collect::<Vec<_>>()
            .join("; ");
        // Past the band and none ours is ATTENTION, not trouble: the
        // line is crossed, and the move is the declared owner's.
        let (band, whose) = if ours.is_empty() {
            (bands::SHED_THEIRS_STALE, format!("none ours — {waits}"))
        } else if theirs.is_empty() {
            (bands::SHED_OURS_STALE, ours.join("; "))
        } else {
            (
                bands::SHED_OURS_STALE,
                format!(
                    "{}; the rest wait on their declared owner — {waits}",
                    ours.join("; ")
                ),
            )
        };
        // Each band measures the cars it names: the ours band the oldest
        // car that is ours, from when THAT car crossed the line.
        let (since, measured) = match oldest_ours {
            Some(h) if band == bands::SHED_OURS_STALE => (
                Some(inputs.now - chrono::Duration::hours(h - PROOF_STALE_HOURS)),
                format!("oldest ours open {h}h"),
            ),
            _ => (stale_since, format!("oldest open {oldest}h")),
        };
        (
            Some((band, since, measured)),
            format!(
                "{} of {n} open past {PROOF_STALE_HOURS}h — oldest {oldest}h, {branch} — {whose}",
                stale.len()
            ),
        )
    } else if n > 0 {
        // Landed and inside the day, each with a way to settle: the shed
        // working (decision 1).
        (
            None,
            plural(n, "landed car awaiting proof", "landed cars awaiting proof"),
        )
    } else {
        (None, "every landed car is proven".to_string())
    };
    let settled = match decided_by {
        Some((band, since, measured)) => settle(
            vec![Finding::new(band, since, measured, why)],
            String::new(),
            inputs.now,
        ),
        None => settle(Vec::new(), why, inputs.now),
    };
    // THE SHED'S THREE PLACES (the rest of decision 5), disjoint and
    // summing to the head: the review drew five wagons under SHED 11,
    // and with these the region map can say which place its slice
    // falls short in rather than leave the reader to recount.
    let places = SHED_PLACES
        .iter()
        .map(|name| Place {
            name: (*name).to_string(),
            count: awaiting
                .iter()
                .filter(|(j, _)| shed_station(&shed_place(&j.metadata)) == *name)
                .count(),
        })
        .collect();
    Region {
        places,
        ..region(
            "shed",
            Some(n),
            None,
            "cars awaiting proof",
            settled,
            trend,
            kpi,
        )
    }
}

/// The shed's places as the floor names its stations
/// (`apps/web/src/it/yard/yard-shed.ts`, `ShedPlace`), in its order.
pub const SHED_PLACES: [&str; 3] = ["inspection-shed", "siding-event", "siding-no-probe"];

/// Which of [`SHED_PLACES`] a car stands in: a probe, run or not, is the
/// inspection shed; an event alone is the event siding; neither is the
/// no-probe siding — the floor's `shedPlace`, over [`shed_place`].
pub fn shed_station(place: &ShedPlace) -> &'static str {
    match place {
        ShedPlace::ProbePending { .. } | ShedPlace::ProbeNotYet { .. } => SHED_PLACES[0],
        ShedPlace::WaitingOn(_) => SHED_PLACES[1],
        ShedPlace::Unproven => SHED_PLACES[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// The shed counts open cars at `proven`; an unproven one troubles
    /// it; the trend is cars proven per day.
    #[test]
    fn the_shed_counts_cars_awaiting_proof_and_an_unproven_one_troubles_it() {
        let car = |branch: &str, md: Value, proven_at: Option<&str>| {
            let mut md = md;
            md["branch"] = json!(branch);
            let open = proven_at.is_none();
            let j = job(
                "ship-a-change",
                branch,
                if open {
                    JobStatus::Open
                } else {
                    JobStatus::Closed
                },
                md,
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-19T06:00:00Z"),
                ),
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-19T07:00:00Z"),
                ),
                if let Some(at) = proven_at {
                    step(&j, "proven", StepStatus::Completed, Some(at))
                } else {
                    step(&j, "proven", StepStatus::Ready, None)
                },
            ];
            (j, s)
        };
        let cars = vec![
            car("fix/a", json!({ "proof_probe": "true" }), None),
            car(
                "fix/b",
                json!({ "proof_event": "the next red train" }),
                None,
            ),
            car("fix/c", json!({}), Some("2026-09-19T08:00:00Z")),
            car("fix/d", json!({}), Some("2026-09-18T08:00:00Z")),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(2));
        // Landed inside the day, each with a way to settle: the shed
        // working, which is clear (design 62de32ae, decision 1).
        assert_eq!(shed.state, RegionState::Clear, "{}", shed.why);
        assert_eq!(shed.unit, "cars awaiting proof");
        assert_eq!(shed.kpi[0].text, "0 cars owed a proof are ours");
        assert_eq!(shed.trend.metric, "proven");
        assert_eq!(shed.trend.current, Some(1.0));
        assert_eq!(shed.trend.previous, Some(1.0));

        let mut with_unproven = cars.clone();
        with_unproven.push(car("fix/e", json!({}), None));
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &with_unproven,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(3));
        assert_eq!(shed.state, RegionState::Troubled);
        assert!(
            shed.why.contains("UNPROVEN") && shed.why.contains("fix/e"),
            "{}",
            shed.why
        );
    }

    /// A DISPROVED CAR LEAVES THE SHED, AND IS NOT COUNTED PROVEN (backlog
    /// 08664157). Car 6b23d135 landed and its claim was measured false;
    /// with no terminal it stood at `proven`, unproven and two days old,
    /// "ours" for ever. Closed through `disproved` — `proven` skipped, the
    /// terminal completed, `merged` still "true" — it is in neither the
    /// count nor the ours KPI, and the proven-per-day trend does not
    /// credit it with a proof it never had. It is still in `cars`: the
    /// record keeps it; only the shed stops reading it as owed.
    #[test]
    fn a_disproved_car_leaves_the_shed_and_is_not_counted_proven() {
        let md = json!({
            "branch": "fix/dev-pod-not-first-evicted",
            "merged": "true",
            "opened_at": "2026-09-17T12:00:00Z",
        });
        let standing = {
            let j = job("ship-a-change", "fix/dev-pod", JobStatus::Open, md.clone());
            let s = vec![
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-18T07:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
                step(&j, "disproved", StepStatus::Pending, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let before = vec![standing];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &before,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(1));
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);

        let mut closed_md = md;
        closed_md["outcome"] = json!("disproved");
        closed_md["disproved"] = json!("true");
        let disproved = {
            let j = job("ship-a-change", "fix/dev-pod", JobStatus::Closed, closed_md);
            let s = vec![
                step(
                    &j,
                    "review",
                    StepStatus::Completed,
                    Some("2026-09-18T07:00:00Z"),
                ),
                step(
                    &j,
                    "proven",
                    StepStatus::Skipped,
                    Some("2026-09-19T11:00:00Z"),
                ),
                step(
                    &j,
                    "disproved",
                    StepStatus::Completed,
                    Some("2026-09-19T11:00:00Z"),
                ),
            ];
            (j, s)
        };
        let after = vec![disproved];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &after,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.count, Some(0), "{}", shed.why);
        assert_eq!(shed.state, RegionState::Clear, "{}", shed.why);
        assert_eq!(shed.kpi[0].text, "0 cars owed a proof are ours");
        assert_eq!(shed.trend.current, Some(0.0), "a disproof is not a proof");
    }

    /// THE SHED'S COLOUR FOLLOWS WHOSE MOVE THE WAIT IS (backlog
    /// 3881f5c9). Measured 2026-09-23 ~16:05Z: the shed read troubled
    /// with the words "every one asked and was told not yet — waiting on
    /// the world, not on us" — the region said the waits were not ours
    /// and painted them red anyway, so red stopped meaning ours to fix.
    /// Troubled is now only ours; a declared, observed, owned wait —
    /// on the world or on a named actor's act — asks for attention (never trouble) and says whose,
    /// until it runs past a max wait the car itself declared.
    #[test]
    fn the_shed_is_troubled_only_by_waits_that_are_ours() {
        let car = |branch: &str, on: &str, owner: Value, max: Value| {
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": on, "seen": "true",
                    (crate::car::WAITS_ON_OWNER): owner,
                    (crate::car::WAITS_ON_MAX_WAIT_HOURS): max,
                },
            });
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-15T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let read = |cars: &[(Job, Vec<Step>)]| {
            let out = regions(&inputs(&status, &[], &[], cars, &[], Some(&[]), Some(&[])));
            by_name(&out, "shed").clone()
        };
        let world = || {
            car(
                "feat/stripe",
                "a Stripe charge",
                json!("world"),
                Value::Null,
            )
        };
        let david = || {
            car(
                "feat/release",
                "a cut-a-release packet",
                json!("emp-david"),
                Value::Null,
            )
        };

        // THEIRS: the world's event and a named actor's act, 98h open.
        let shed = read(&[world(), david()]);
        // Past the band and not ours: ATTENTION, never trouble — and the
        // band that decided it is named, with how long it has been past.
        assert_eq!(
            shed.state,
            RegionState::Attention,
            "declared, observed, owned waits are not ours: {}",
            shed.why
        );
        let band = shed
            .band
            .as_ref()
            .expect("a non-clear state names its band");
        assert_eq!(band.id, "shed-theirs-stale");
        assert!(band.reads.contains("oldest open 98h"), "{}", band.reads);
        assert_eq!(
            band.held_minutes,
            Some(74 * 60),
            "98h open, past 24h for 74h"
        );
        assert_eq!(shed.kpi[0].value, Some(0.0), "none of them is ours");
        assert!(
            shed.why
                .contains("waiting on the world: a Stripe charge (feat/stripe)")
                && shed
                    .why
                    .contains("waiting on emp-david: a cut-a-release packet"),
            "and it says whose move each one is: {}",
            shed.why
        );

        // PAST ITS OWN DECLARED PATIENCE: 98h open against a 72h max.
        let late = car("feat/late", "a Stripe charge", json!("world"), json!(72));
        let shed = read(&[late, david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("past the max wait they declared")
                && shed.why.contains("feat/late")
                && shed.why.contains("the rest wait on their declared owner"),
            "{}",
            shed.why
        );

        // NO OWNER: the prose may name David, the field does not.
        let unowned = car(
            "feat/unowned",
            "a release (David opens it)",
            Value::Null,
            Value::Null,
        );
        let shed = read(&[unowned]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("declared no owner") && shed.why.contains("feat/unowned"),
            "an owner is declared, never read out of prose: {}",
            shed.why
        );

        // AN EVENT ONLY PROSE NAMES: no probe runs, so no `seen` check
        // does either — nothing observes it (the dev-door login car,
        // 2026-09-23, is this shape).
        let (mut j, s) = world();
        j.metadata = json!({
            "branch": "feat/prose", "merged": true, "opened_at": "2026-09-15T10:00:00Z",
            "proof_event": "David logs in through the dev door",
        });
        let shed = read(&[(j, s), david()]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("only prose names") && shed.why.contains("feat/prose"),
            "{}",
            shed.why
        );
    }

    /// THE OURS BAND MEASURES THE OLDEST CAR THAT IS OURS (backlog
    /// 3881f5c9). Measured 2026-09-24 16:42Z: the band read "oldest open
    /// 168h > the 24h proof band, ours to move", held 6d — and the 168h
    /// car was the Stripe wait, declared on the world, while the one car
    /// that was ours was 40h old. The header's number must be the number
    /// of the thing it names, or checking the state against it (the
    /// point of naming the band) checks it against the wrong car.
    #[test]
    fn the_ours_band_reads_the_age_of_the_oldest_car_that_is_ours() {
        // NOW is 2026-09-19T12:00:00Z.
        let car = |branch: &str, opened: &str, owner: Value| {
            let j = job(
                "ship-a-change",
                branch,
                JobStatus::Open,
                json!({
                    "branch": branch, "merged": true, "opened_at": opened,
                    "proof_probe": "true",
                    "proof_attempt": {
                        "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75,
                        "probe": "true",
                        (crate::car::NOT_YET_SINCE): "2026-09-19T10:00:00Z",
                        (crate::car::NOT_YET_RUNS): 2,
                    },
                    (crate::car::WAITS_ON): {
                        "on": "an event", "seen": "true",
                        (crate::car::WAITS_ON_OWNER): owner,
                    },
                }),
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-12T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();
        let cars = vec![
            car("feat/theirs-old", "2026-09-12T12:00:00Z", json!("world")),
            car("fix/ours-young", "2026-09-17T20:00:00Z", Value::Null),
        ];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        let band = shed.band.as_ref().expect("a troubled shed names its band");
        assert_eq!(band.id, "shed-ours-stale");
        assert!(
            band.reads.contains("oldest ours open 40h") && !band.reads.contains("168h"),
            "the ours band measures the ours car: {}",
            band.reads
        );
        assert_eq!(
            band.held_minutes,
            Some(16 * 60),
            "40h open, past 24h for 16h — not the theirs car's 6d"
        );
        assert!(
            shed.why.contains("oldest 168h, feat/theirs-old"),
            "the sentence still leads with the oldest car of all: {}",
            shed.why
        );
    }

    /// A car awaiting proof PAST A DAY troubles the shed, even when its
    /// probe is answering `not yet` — early, not wrong, and the
    /// commonest place a car stops (backlog 488d42e6).
    ///
    /// Measured 2026-09-22: nine cars at `proven`, every one merged,
    /// aged 9.0h to 105.7h, and the shed said `9 landed cars awaiting
    /// proof` — the words it prints ten minutes after a landing. The
    /// age had roughly doubled since the packet was filed while the
    /// COUNT fell, so the rate was the finding and nothing showed it.
    #[test]
    fn a_car_awaiting_proof_past_a_day_troubles_the_shed() {
        // NOW is 2026-09-19T12:00:00Z.
        let aged = |branch: &str, opened: &str| {
            let j = job(
                "ship-a-change",
                branch,
                JobStatus::Open,
                json!({
                    "branch": branch,
                    "merged": true,
                    "opened_at": opened,
                    "proof_probe": "true",
                    // `not yet` — the place that was never troubled.
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let status = empty_status();

        // FRESH: landed this morning, still working. Must stay clear, or
        // the signal fires on every landing and stops meaning anything.
        let fresh = vec![aged("fix/fresh", "2026-09-19T06:00:00Z")];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &fresh,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Clear,
            "a car six hours old is working, not troubled: {}",
            shed.why
        );

        // STALE: two days at `not yet`.
        let stale = vec![
            aged("fix/fresh", "2026-09-19T06:00:00Z"),
            aged("feat/two-days", "2026-09-17T10:00:00Z"),
        ];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &stale,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Troubled,
            "a car past {PROOF_STALE_HOURS}h must trouble the shed: {}",
            shed.why
        );
        assert!(
            shed.why.contains("feat/two-days"),
            "and NAME the oldest — a count sends a reader to a list, a branch sends them \
             to a car: {}",
            shed.why
        );
        assert!(
            shed.why.contains("50h"),
            "carrying its age, which is the finding: {}",
            shed.why
        );
        assert!(
            !shed.why.contains("fix/fresh"),
            "and not the fresh one, which is not the problem: {}",
            shed.why
        );

        // A CAR WITH NO `opened_at` IS SKIPPED, not treated as
        // infinitely old. An absent timestamp is not evidence of age,
        // and reading it as one would trouble the shed for a missing
        // field — the zero-means-unknown defect, in a region card.
        let no_clock = {
            let j = job(
                "ship-a-change",
                "fix/no-clock",
                JobStatus::Open,
                json!({
                    "branch": "fix/no-clock",
                    "merged": true,
                    "proof_probe": "true",
                    "proof_attempt": { "not_yet": true, "exit": 75 },
                }),
            );
            let st = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-17T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            vec![(j, st)]
        };
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &no_clock,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Clear,
            "a car with no opened_at has no age, so it is not past any band — never \
             troubled for a field it does not carry: {}",
            shed.why
        );
    }

    /// The shed classification is total and the probe wins over an
    /// event — the same rule `boss orient` prints by (it now calls this).
    #[test]
    fn the_shed_place_is_total_and_the_probe_wins() {
        assert_eq!(shed_place(&json!({})), ShedPlace::Unproven);
        assert_eq!(
            shed_place(&json!({ "proof_event": "next red train" })),
            ShedPlace::WaitingOn("next red train".into())
        );
        assert_eq!(
            shed_place(&json!({ "proof_probe": "true", "proof_event": "x" })),
            ShedPlace::ProbePending { last: None }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 75, "why": "not yet: no train" } })
            ),
            ShedPlace::ProbeNotYet {
                said: "not yet: no train".into()
            }
        );
        assert_eq!(
            shed_place(
                &json!({ "proof_probe": "true", "proof_attempt": { "exit": 1, "why": "FAILED" } })
            ),
            ShedPlace::ProbePending {
                last: Some("FAILED".into())
            }
        );
    }

    /// THE SHED'S PLACES ARE DISJOINT AND SUM TO ITS COUNT — the three
    /// the floor draws, named as the floor names them, so the region map
    /// can say which of them its slice falls short in (the review drew 5
    /// wagons under a SHED 11).
    #[test]
    fn the_shed_says_how_many_cars_stand_in_each_of_its_three_places() {
        let awaiting = |branch: &str, md: Value| {
            let mut md = md;
            md["branch"] = json!(branch);
            let j = job("ship-a-change", branch, JobStatus::Open, md);
            let s = vec![
                step(
                    &j,
                    "gate",
                    StepStatus::Completed,
                    Some("2026-09-19T06:00:00Z"),
                ),
                step(&j, "proven", StepStatus::Ready, None),
            ];
            (j, s)
        };
        let cars = vec![
            awaiting("fix/a", json!({ "proof_probe": "true" })),
            awaiting(
                "fix/b",
                json!({ "proof_probe": "true", "proof_event": "both" }),
            ),
            awaiting("fix/c", json!({ "proof_event": "the next red train" })),
            awaiting("fix/d", json!({})),
        ];
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        let places: Vec<(&str, usize)> = shed
            .places
            .iter()
            .map(|p| (p.name.as_str(), p.count))
            .collect();
        assert_eq!(
            places,
            vec![
                ("inspection-shed", 2),
                ("siding-event", 1),
                ("siding-no-probe", 1)
            ]
        );
        assert_eq!(
            Some(shed.places.iter().map(|p| p.count).sum::<usize>()),
            shed.count
        );
    }
}
