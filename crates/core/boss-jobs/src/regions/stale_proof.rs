//! WHOSE MOVE A STALE PROOF IS (backlog 3881f5c9, adef5ddf): a car past
//! the shed's band either waits on the world, as its declaration says,
//! or on us — and only the second troubles the shed.

use super::*;

/// How long a landed car may stand unproven before the shed says so.
///
/// WHY A BOUND AT ALL (backlog 488d42e6). The shed troubled itself only
/// for a car with NO way to settle (`Unproven`) or a FAILING probe. A
/// car whose probe answers `not yet` — early, not wrong — was never
/// troubled however long it said so, and `not yet` is by far the
/// commonest place a car stops. Measured 2026-09-22: nine cars standing
/// at `proven`, every one `merged = true`, aged 9.0h to **105.7h**, and
/// the shed read `9 landed cars awaiting proof` — the same words it
/// prints ten minutes after a landing. The packet measured 58.3h as the
/// worst case two days earlier, so the age roughly doubled while the
/// COUNT fell from twelve to nine: proofs drain, just slower than they
/// accumulate. That is a rate to be seen, not a queue to be chased.
///
/// WHY 24 HOURS. Most probes are rechecked hourly and most of the
/// events they wait on happen daily, so a car that has not settled
/// inside a day is waiting on something that is not coming on its own.
/// It is a threshold for LOOKING, not a deadline: the car is still
/// correct, still landed, still retrying.
///
/// THE CLOCK IS THE CAR'S OWN `opened_at`, not a landing time, because
/// no landing time is recorded on the car — `merged` is a boolean and
/// the `merged` STEP cannot complete until `proven` does, which is the
/// very thing being waited for. So this over-reports by however long
/// the car took to build and land, and the wording says "open" rather
/// than "landed" for that reason.
pub const PROOF_STALE_HOURS: i64 = 24;

/// How long a probe may answer `not yet` WITHOUT A BREAK before the shed
/// stops calling it the world's move (backlog adef5ddf).
///
/// WHY A SECOND BOUND. [`PROOF_STALE_HOURS`] times the CAR; this times
/// the ANSWER, read from the streak each attempt carries
/// (`boss_jobs::car::not_yet_streak`). A probe that can never pass —
/// b8c4267f greps a literal a later car deliberately removed, 52e0287e
/// reads the wrong occurrence of a call — exits 75 exactly like a
/// patient one, and the shed said "waiting on the world, not on us" of
/// both.
///
/// WHY 72 HOURS, measured over the 1812 ops-requests on record
/// (2026-09-17 to 09-23). Of 37 not-yet streaks that ended in a pass,
/// the longest under the hourly recheck spanned 61h (a4a2118e, 46 runs)
/// and the longest at all 75h (8c4f8ed9, a weekly Stripe event, under
/// the old daily recheck). The six open cars still answering not-yet
/// were at 97h to 134h. So a day would have named six of the honest
/// waits that later passed, and three days names one (that 75h daily-
/// recheck wait) while every stuck car clears it. Like the bound above it is a threshold for LOOKING:
/// the probe is still rechecked hourly and may still pass. What changes
/// is whose move the sentence says it is.
///
/// ONLY FOR A CAR THAT NEVER SAID WHAT IT WAITS ON (backlog b461341d):
/// the triage of the six cars this bound measured found all six honest
/// waits on the world, so a car declaring `waits_on` is judged by
/// whether its declared event has been SEEN instead — see
/// `boss_jobs::car::starved`, the one predicate the shed and orient read.
pub const NOT_YET_STARVED_HOURS: i64 = 72;

/// WHOSE MOVE a landed car past [`PROOF_STALE_HOURS`] is — the shed's
/// classification (3881f5c9, adef5ddf, b461341d, e9b164a1), lifted out
/// of [`shed`]'s loop so the shed's sentence and the stuck block read ONE
/// answer about which car is ours (backlog 4142d821). Every variant but
/// [`StaleProof::Theirs`] is ours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StaleProof {
    /// No probe, no event: nothing mechanical can settle it.
    Unproven,
    /// Its probe ran and failed.
    Failing,
    /// A probe is recorded and nothing has ever run it.
    NeverProbed,
    /// Only prose names what it waits on, and no probe runs, so no `seen`
    /// check ever runs either: nothing observes it.
    ProseOnly,
    /// Told not yet without a break past [`NOT_YET_STARVED_HOURS`], with
    /// no declared wait.
    Starved(crate::car::NotYetStreak),
    /// Told not yet after what it declared it waits on was seen.
    Seen(String),
    /// Declared a wait with no `seen` check that no named actor owns.
    Unobserved,
    /// Told not yet with no declared wait.
    Undeclared,
    /// Declared and observed a wait, and named no owner.
    Unowned,
    /// Past the max wait it declared.
    Overdue(crate::car::OwnedWait),
    /// The one answer that is not ours: a declared, observed wait on its
    /// named owner, inside any patience it declared.
    Theirs(crate::car::OwnedWait),
}

/// Classify a stale car, `hours` open. A failing or unproven car is ours
/// whatever wait it declares — the shed names those first and troubles
/// on them regardless — so they are split out before the wait is read.
pub(crate) fn stale_proof(md: &Value, hours: i64) -> StaleProof {
    use crate::car::Starved;
    match shed_place(md) {
        ShedPlace::Unproven => StaleProof::Unproven,
        ShedPlace::ProbePending { last: Some(_) } => StaleProof::Failing,
        ShedPlace::ProbePending { last: None } => StaleProof::NeverProbed,
        // No probe runs, so no `seen` check does either: a world event
        // here is observed by nothing and is ours. A named actor's act
        // is that actor's move all the same (3881f5c9) — the dev-door
        // login, David's to perform, is this shape.
        ShedPlace::WaitingOn(_) => match crate::car::owned_wait(md) {
            Some(o) if matches!(o.owner, crate::car::WaitOwner::Actor(_)) => owed(o, hours),
            _ => StaleProof::ProseOnly,
        },
        ShedPlace::ProbeNotYet { .. } => match crate::car::starved(md) {
            Some(Starved::Undeclared(streak)) => StaleProof::Starved(streak),
            Some(Starved::SeenWhileNotYet { on, .. }) => StaleProof::Seen(on),
            None => match crate::car::waits_on(md) {
                None => StaleProof::Undeclared,
                Some(w) => match crate::car::owned_wait(md) {
                    Some(o) => owed(o, hours),
                    None if w.seen.is_none() => StaleProof::Unobserved,
                    None => StaleProof::Unowned,
                },
            },
        },
    }
}

/// An owned wait, inside or past the patience it declared.
fn owed(o: crate::car::OwnedWait, hours: i64) -> StaleProof {
    if o.overdue(hours) {
        StaleProof::Overdue(o)
    } else {
        StaleProof::Theirs(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// A STALE PROOF IS EITHER ON US OR ON THE WORLD, and the shed said
    /// neither.
    ///
    /// Measured 2026-09-22, an hour after the staleness signal above
    /// shipped: the shed read "8 of 12 open past 24h — oldest 108h" and
    /// three rechecks of the oldest cars each came back "not yet: no
    /// real prune since convergence", "not yet: no sponsorship packet
    /// opened since the change converged", "not yet: no answered
    /// sweep-archive-branches request". Those cars are HEALTHY and
    /// blocked on a qualifying event the world has not produced — the
    /// probe asked and was answered. A car whose probe has never been
    /// run is the opposite, and the same sentence covered both.
    ///
    /// Reporting them identically is the decay CLAUDE.md names under
    /// "a check nobody reads": a colour that fires on a state nobody
    /// can act on teaches the reader to discount it. The distinction
    /// already exists in the type — `ShedPlace::ProbeNotYet` versus a
    /// `ProbePending` with no attempt recorded — and only the stale
    /// branch threw it away.
    #[test]
    fn a_stale_proof_says_whether_it_waits_on_us_or_on_the_world() {
        // NOW is 2026-09-19T12:00:00Z; both cars are two days old, so
        // age cannot be what separates them.
        let aged = |branch: &str, attempt: Value| {
            let mut md = json!({
                "branch": branch,
                "merged": true,
                "opened_at": "2026-09-17T10:00:00Z",
                "proof_probe": "true",
            });
            if !attempt.is_null() {
                md["proof_attempt"] = attempt;
            }
            let j = job("ship-a-change", branch, JobStatus::Open, md);
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

        // ON THE WORLD: the probe ran and was told not yet.
        let asked = vec![aged("feat/asked", json!({ "not_yet": true, "exit": 75 }))];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &asked,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(
            shed.state,
            RegionState::Troubled,
            "still troubled — a car stuck two days is worth a look either way: {}",
            shed.why
        );
        assert!(
            shed.why.contains("told not yet"),
            "but it says the probe asked and the world answered: {}",
            shed.why
        );
        assert!(
            !shed.why.contains("never"),
            "and does not accuse anyone of neglecting it: {}",
            shed.why
        );

        // ON US, same age, same everything else: no attempt recorded,
        // so nothing has ever run this car's probe. THE CONTROL — it is
        // what stops the clause reading as always-waiting-on-the-world.
        let never = vec![aged("feat/never", Value::Null)];
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &never,
            &[],
            Some(&[]),
            Some(&[]),
        ));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("never probed"),
            "the same age with no attempt on record reads as on us: {}",
            shed.why
        );

        // MIXED: the count has to survive both being present, or the
        // commonest real shed (a few of each) gets one of the two
        // sentences and the other half goes unmentioned.
        let both = vec![
            aged("feat/asked", json!({ "not_yet": true, "exit": 75 })),
            aged("feat/never", Value::Null),
        ];
        let out = regions(&inputs(&status, &[], &[], &both, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert!(
            shed.why.contains("1 never probed"),
            "names how many are on us, alongside the rest: {}",
            shed.why
        );
    }

    /// A PROBE THAT HAS ANSWERED NOT-YET FOR DAYS IS NOT WAITING ON THE
    /// WORLD (backlog adef5ddf). The split above read every told-not-yet
    /// car as the world's move, and two of the six measured at 86+
    /// consecutive not-yets could never pass: b8c4267f greps a literal a
    /// later car removed, 52e0287e reads the wrong occurrence of a call.
    /// Exit 75 cannot tell them apart from a patient probe; the length of
    /// the streak can, so a streak past [`NOT_YET_STARVED_HOURS`] is
    /// named as ours to read — and a short one, same car age, is not.
    #[test]
    fn a_not_yet_that_has_lasted_days_is_ours_to_read_not_the_worlds() {
        // NOW is 2026-09-19T12:00:00Z; both cars opened four days ago,
        // so the car's age cannot be what separates them — only the
        // streak the attempt carries.
        let aged = |branch: &str, since: &str, runs: u64| {
            let md = json!({
                "branch": branch,
                "merged": true,
                "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z",
                    "not_yet": true,
                    "exit": 75,
                    "probe": "true",
                    (crate::car::NOT_YET_SINCE): since,
                    (crate::car::NOT_YET_RUNS): runs,
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

        // STARVED: 96 runs across 96 hours, all not-yet.
        let shed = read(&[aged("fix/starved", "2026-09-15T11:00:00Z", 96)]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            !shed.why.contains("waiting on the world"),
            "a streak of days is not the world's move: {}",
            shed.why
        );
        assert!(
            shed.why.contains("ours to read") && shed.why.contains("fix/starved"),
            "it names the car, as ours to read: {}",
            shed.why
        );
        assert!(
            shed.why.contains("96h") && shed.why.contains("96 runs"),
            "carrying the streak, which is the finding: {}",
            shed.why
        );

        // THE CONTROL: same age, a six-hour streak — not starved. It
        // declared no wait, so it is still not the world's (3881f5c9):
        // nothing on the car says whose move it is.
        let shed = read(&[aged("fix/patient", "2026-09-19T05:00:00Z", 7)]);
        assert!(
            shed.why.contains("no declared wait") && !shed.why.contains("waiting on the world"),
            "a short undeclared streak is not starved, and not the world's either: {}",
            shed.why
        );
        assert!(!shed.why.contains("ours to read"), "{}", shed.why);

        // MIXED: both counts survive together.
        let shed = read(&[
            aged("fix/starved", "2026-09-15T11:00:00Z", 96),
            aged("fix/patient", "2026-09-19T05:00:00Z", 7),
        ]);
        assert!(
            shed.why.contains("1 told not yet without a break")
                && shed.why.contains("1 told not yet with no declared wait"),
            "names the starved one and the undeclared one apart: {}",
            shed.why
        );
    }

    /// A CAR THAT SAID WHAT IT WAITS ON IS NOT STARVED BY DURATION
    /// (backlog b461341d). All six cars adef5ddf's streak measured were
    /// honest waits on the world; with `waits_on` declared, a 96h streak
    /// is the world's — until the declared event is SEEN in the record
    /// while the probe still says not-yet, which is ours at once.
    #[test]
    fn a_declared_wait_is_the_worlds_until_its_event_is_seen() {
        let car = |branch: &str, seen_at: Option<&str>| {
            let mut attempt = json!({
                "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                (crate::car::NOT_YET_RUNS): 96,
            });
            if let Some(s) = seen_at {
                attempt[crate::car::WAITS_ON_SEEN_AT] = json!(s);
            }
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true", "proof_attempt": attempt,
                (crate::car::WAITS_ON): {
                    "on": "a real Stripe sponsorship charge", "seen": "true",
                    (crate::car::WAITS_ON_OWNER): "world",
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

        let shed = read(&[car("fix/declared", None)]);
        assert!(
            shed.why.contains("waiting on the world") && !shed.why.contains("ours to read"),
            "a declared wait not yet seen is the world's, at 96h: {}",
            shed.why
        );
        assert_eq!(shed.state, RegionState::Attention, "{}", shed.why);

        let shed = read(&[car("fix/contradicted", Some("2026-09-19T10:00:00Z"))]);
        assert!(
            shed.why.contains("ours to read")
                && shed.why.contains("fix/contradicted")
                && shed.why.contains("a real Stripe sponsorship charge"),
            "seen in the record, still not yet — ours, naming what it waited on: {}",
            shed.why
        );
    }

    /// A DECLARED WAIT WITH NO OBSERVER IS COUNTED, NOT SILENT (backlog
    /// e9b164a1). b461341d exempts a declared wait from the streak bound
    /// and hands its judgement to the `seen` check — so a declaration
    /// with `seen` null has handed it to nothing, and without this count
    /// it read exactly like an observed wait: "waiting on the world",
    /// forever. All six cars declared on 2026-09-23 were in that shape.
    #[test]
    fn a_declared_wait_with_no_seen_check_is_counted_as_ours() {
        let car = |branch: &str, seen: Option<&str>| {
            let md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": "a real Stripe sponsorship charge", "seen": seen,
                    (crate::car::WAITS_ON_OWNER): "world",
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

        let shed = read(&[car("fix/unobserved", None)]);
        assert!(
            shed.why.contains("1 declared a wait with no seen check")
                && shed.why.contains("fix/unobserved")
                && !shed.why.contains("waiting on the world"),
            "a declaration nothing observes is ours, and counted: {}",
            shed.why
        );

        let shed = read(&[
            car("fix/unobserved", None),
            car("fix/observed", Some("true")),
        ]);
        assert!(
            shed.why.contains("1 declared a wait with no seen check")
                && shed.why.contains("the rest wait on their declared owner")
                && shed.why.contains("waiting on the world"),
            "an observed wait stays the world's beside it: {}",
            shed.why
        );

        let shed = read(&[car("fix/observed", Some("true"))]);
        assert!(
            !shed.why.contains("no seen check"),
            "an observed wait is not counted: {}",
            shed.why
        );
    }

    /// A NAMED PERSON'S ACT IS THEIRS WITH OR WITHOUT A PROBE (backlog
    /// 3881f5c9, fix shape (2)). Measured 2026-09-24 16:42Z: GET
    /// /api/yard/regions read the shed TROUBLED on exactly one car,
    /// fix/the-dev-door-maps-the-access-principal-to-root — no probe, a
    /// prose event, and a `waits_on` declaring `owner: emp-david` for
    /// the Access SSH CA ceremony. Its next move is David's, declared as
    /// data, and the shed called it ours because nothing observed it. A
    /// WORLD event still needs an observer to be the world's; an actor
    /// is its own. The control is the same car with its owner set to
    /// the world, which stays ours.
    #[test]
    fn a_declared_act_of_a_named_actor_is_theirs_even_with_no_probe_to_observe_it() {
        let car = |branch: &str, owner: &str, max: Value, probe: bool| {
            let mut md = json!({
                "branch": branch, "merged": true, "opened_at": "2026-09-15T10:00:00Z",
                (crate::car::WAITS_ON): {
                    "on": "the Access SSH CA ceremony",
                    (crate::car::WAITS_ON_OWNER): owner,
                    (crate::car::WAITS_ON_MAX_WAIT_HOURS): max,
                },
            });
            if probe {
                md["proof_probe"] = json!("true");
                md["proof_attempt"] = json!({
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true",
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                });
            } else {
                md["proof_event"] = json!("David completes the Access SSH CA ceremony");
            }
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

        // The live shape: prose event, no probe, David's act.
        let shed = read(&[car("fix/dev-door", "emp-david", Value::Null, false)]);
        assert_eq!(shed.state, RegionState::Attention, "{}", shed.why);
        assert_eq!(shed.kpi[0].value, Some(0.0), "{}", shed.why);
        assert!(
            shed.why
                .contains("waiting on emp-david: the Access SSH CA ceremony (fix/dev-door)")
                && !shed.why.contains("only prose names"),
            "shown as waiting on its owner, not as ours: {}",
            shed.why
        );

        // The same act behind a probe that says not yet, no seen check.
        let shed = read(&[car("fix/probed", "emp-david", Value::Null, true)]);
        assert_eq!(shed.state, RegionState::Attention, "{}", shed.why);
        assert!(!shed.why.contains("no seen check"), "{}", shed.why);

        // CONTROL: the world's event with nothing observing it is ours,
        // in both shapes.
        let shed = read(&[car("fix/world-prose", "world", Value::Null, false)]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("only prose names"), "{}", shed.why);
        let shed = read(&[car("fix/world-probed", "world", Value::Null, true)]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("no seen check"), "{}", shed.why);

        // PATIENCE STILL BINDS: 98h open against the 48h David's car
        // declared is ours again.
        let shed = read(&[car("fix/late", "emp-david", json!(48), false)]);
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(
            shed.why.contains("past the max wait they declared") && shed.why.contains("fix/late"),
            "{}",
            shed.why
        );
    }
}
