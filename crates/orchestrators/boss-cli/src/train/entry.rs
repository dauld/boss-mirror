//! Entry — the phases, the conductor's lock, and `run`.

use super::*;

/// Which slice of the conductor to run. `Run` is the timer entry
/// (reconcile + board); the others are the standalone verbs the
/// python argv flags (`--preflight`, `--reconcile-only`) selected.
/// `Cancel` is the operator's judgment call on a train that will not
/// arrive — close the PR unmerged, release the cars, record why.
pub enum Phase {
    Preflight,
    Reconcile,
    Board,
    Run,
    Cancel { handle: String, reason: String },
}

/// One thing the standing loop does. [`run`] walks [`loop_work`]'s
/// list for the phase it was given, so the table IS the dispatch — a
/// test that reads it reads what a tick does, not a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Work {
    /// Push the branches credential-less workspaces filed as
    /// publish-request packets (`crate::publish_requests`), so they
    /// are on the forge before this pass looks for cars.
    DrainPublishRequests,
    Reconcile,
    Board,
}

/// What each phase of the standing loop does, in order.
///
/// The drain rides EVERY phase that reconciles, not only `Run`. Until
/// 2026-09-15 it rode `Run` alone — correct when `run` was the
/// ten-minute tick, wrong once the cadence registry split it: rule
/// train-window fires `run` at 06:05 and 18:05 and rule train-reconcile
/// fires `reconcile` every ten minutes. Measured (backlog 03e81aa9):
/// publish-request 9f9fa486 filed 16:47Z, train-reconcile fired
/// 17:00:45Z rc 0 and left it at its publish step, next `run` 18:05Z —
/// a 78-minute wait for a one-commit bundle, up to 12 h at worst.
///
/// `Preflight` returns before the table is read; `Cancel` is an
/// operator's verb on one named train and is dispatched by name.
pub(crate) fn loop_work(phase: &Phase) -> &'static [Work] {
    match phase {
        Phase::Reconcile => &[Work::DrainPublishRequests, Work::Reconcile],
        Phase::Board => &[Work::Board],
        Phase::Run => &[Work::DrainPublishRequests, Work::Reconcile, Work::Board],
        Phase::Preflight | Phase::Cancel { .. } => &[],
    }
}

/// What a contended lock MEANS for the phase that gave up on it.
///
/// Whether to WAIT first is [`lock_wait_budget`]'s question. This one
/// is asked only once waiting is over: did leaving finish the job, or
/// abandon a request nobody else will pick up?
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Contended {
    /// The holder is doing this very work right now: reconcile and
    /// board are the standing loop, and a standalone preflight has
    /// nothing further to prove while the locomotive is demonstrably
    /// pulling. Nothing was abandoned — so a phase that left AT ONCE
    /// leaves at 0. (A starvable phase that spent its whole budget
    /// still never ran, and exits [`LOCK_CONTENDED_EXIT`] for that
    /// separate reason.)
    Covered,
    /// Leave at [`LOCK_CONTENDED_EXIT`], saying what did not happen.
    /// Nothing else in the system will do it.
    Abandoned(String),
}

/// Classify a contended lock for `phase`.
///
/// `Cancel` is the one phase carrying an operator's specific request:
/// release THESE cars from THAT train. No other run will cancel it, so
/// leaving abandons the request — and returning `Ok(())` from here told
/// the operator the opposite. That cost nothing the two times the verb
/// was run on 2026-09-10 because the lock happened to be free; had it
/// not been, the reader of "cancelled" would have believed three cars
/// were back on the dock while they were still aboard a dead train
/// holding the track.
pub(crate) fn contended(phase: &Phase) -> Contended {
    match phase {
        Phase::Cancel { handle, .. } => Contended::Abandoned(format!(
            "train {handle} NOT cancelled: another conductor run holds the lock. \
             No car was released and the PR is still open — re-run the cancel once \
             the run in progress finishes."
        )),
        Phase::Preflight | Phase::Reconcile | Phase::Board | Phase::Run => Contended::Covered,
    }
}

// ---------------------------------------------------------------------------
// THE CONDUCTOR'S LOCK — A LOSER THAT LEAVES MUST EVENTUALLY WIN
//
// One lock file serializes every conductor verb, and the loser logs and
// leaves. That is correct for a verb with a designed retry, and wrong
// for one that can be starved — which is what happened for ten hours on
// 2026-09-10 (backlog 4860aff8):
//
//   - the board cadence fires every 60 seconds and spends 12–14 seconds
//     in the consist check, so it holds the lock for a fifth of every
//     minute while a car sits on the dock;
//   - the 10-minute reconcile fires about one second after it and so
//     lost the lock EVERY time — 55 consecutive "another conductor run
//     holds the lock — leaving", each recorded `rc=0 in 0s`;
//   - the reconcile is what merges a green train, runs the stall
//     sentinel, writes arrival reports and sweeps landed branches. All
//     four were dead for nine hours, and nothing said so, because
//     nothing needed a merge in that window. It surfaced when the next
//     car tried to land — the verb that lands a fix being the verb that
//     was starved.
//
// The contention is deterministic, not a race: the board always fires
// first and always wins. So the fix is not a bigger lock, it is
// FAIRNESS — the starvable side waits a bounded time for its turn:
//
//   - RECONCILE and RUN wait (`CONDUCTOR_LOCK_WAIT`). Waiting 14 seconds
//     out of a 600-second period costs nothing, and the cadence loop's
//     one-in-flight-run-per-rule guard means a waiting pass cannot stack
//     up behind itself.
//   - BOARD and PREFLIGHT do not wait. A board must never queue behind a
//     reconcile that is deploying (job 9c5871fa: 30+ minutes), and it
//     does not need to — its firing records boarded-nothing, which
//     releases the queue-depth cooldown, and the next tick re-fires.
//
// The two alternatives were weighed and rejected. Serializing board and
// reconcile into one cadence slot (`boss train run` already does
// reconcile-then-board) fixes only the pair of rules that happen to
// collide today and leaves every other caller — a hand-run verb, a
// second conductor — starvable by the same mechanism. Shortening the
// board's hold means shortening the consist check, which is the thing
// doing the work.
//
// The budget is a constant and not delivery-policy data deliberately:
// the lock is taken before the jobs API is read, so a registry value
// would have to be fetched by a run that has not yet proved it may run.
// ---------------------------------------------------------------------------

/// How long a starvable phase waits for the conductor's lock. Two
/// minutes: comfortably longer than the 12–14s a refusing board holds it
/// and than a board that departs a train (push + PR + per-car writes),
/// and a fifth of the reconcile's own 10-minute period, so a pass that
/// waits its whole budget has still left the next window clear.
pub(crate) const CONDUCTOR_LOCK_WAIT: Duration = Duration::from_secs(120);

/// How often the waiter retries. The hold it waits out is seconds long,
/// so a one-second poll wins within a second of the lock being freed.
const LOCK_POLL: Duration = Duration::from_secs(1);

/// Preflight refused this box — distinct from a crash, loud in the
/// unit's status. Named rather than written as a bare `3` at the exit
/// site so the one test that has to know it is distinct from
/// [`LOCK_CONTENDED_EXIT`] reads the code itself, not a literal copy
/// of it.
pub(crate) const PREFLIGHT_FAIL_EXIT: i32 = 3;

/// Exit code for an invocation that gave up on the lock without doing
/// the work it came to do — either because it waited its whole budget
/// and never ran, or because it abandoned a request nobody else will
/// pick up (see [`Contended`]).
///
/// Distinct from 0 because the cadence loop records the child's exit
/// code as the firing's `rc`, and 55 starved passes recording `rc=0 in
/// 0s` is precisely how nine hours of dead maintenance read as nine
/// hours of successes — and because a `boss train cancel` that released
/// no car printed success-shaped output at 0. Distinct from
/// [`PREFLIGHT_FAIL_EXIT`] because two causes must not share one code.
pub(crate) const LOCK_CONTENDED_EXIT: i32 = 4;

/// How long this phase waits for the lock before leaving.
pub(crate) fn lock_wait_budget(phase: &Phase) -> Duration {
    match phase {
        // Starvable by construction: it fires on a fixed interval
        // against a board that fires more often and holds longer.
        Phase::Reconcile | Phase::Run => CONDUCTOR_LOCK_WAIT,
        // A board has its own retry one tick later, and must not queue
        // behind a long reconcile. Preflight proves nothing a running
        // conductor has not already proved. A cancel is an operator
        // standing at the prompt, who can see the line and re-run.
        Phase::Preflight | Phase::Board | Phase::Cancel { .. } => Duration::ZERO,
    }
}

/// Announced once, when a phase starts waiting rather than leaving.
pub(crate) fn lock_waiting_line(budget: Duration) -> String {
    format!(
        "another conductor run holds the lock — waiting up to {}s for it",
        budget.as_secs()
    )
}

/// The win. Journalled with the waited time because "how long was the
/// reconcile held off" is the number that was missing for nine hours.
pub(crate) fn lock_acquired_line(waited: Duration) -> String {
    format!(
        "took the conductor's lock after waiting {}s",
        waited.as_secs()
    )
}

/// The loss. Unchanged for a phase that does not wait — the line
/// operators and `cadence.rs`'s own doc comment already grep for — and
/// named with the waited time for one that did.
///
/// Whole seconds decide which form it takes, not `is_zero`: a phase with
/// a zero budget still spends a few hundred nanoseconds between taking
/// the clock and failing the try, and "leaving after waiting 0s" would
/// be a wait nobody waited.
pub(crate) fn lock_contended_line(waited: Duration) -> String {
    if waited.as_secs() == 0 {
        "another conductor run holds the lock — leaving".to_string()
    } else {
        format!(
            "another conductor run holds the lock — leaving after waiting {}s",
            waited.as_secs()
        )
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

pub async fn run(phase: Phase, dry: bool, now: DateTime<Utc>) -> Result<()> {
    let cfg = Config::from_env(dry);
    // The forge adapter is built before anything else — the python
    // conductor constructed FORGE at import, so a misconfigured
    // BOSS_TRAIN_FORGE fails every entry loudly, not just the boarding
    // that needed it.
    let forge = make_forge(&cfg)?;
    fs::create_dir_all(&cfg.home)?;
    let lock = File::create(Path::new(&cfg.home).join("lock"))?;
    // A held lock means a conductor run is active right now. A phase with
    // its own retry leaves at once; a starvable one waits its bounded
    // turn — see "THE CONDUCTOR'S LOCK" above.
    let budget = lock_wait_budget(&phase);
    let since = std::time::Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => {
                // Reaching a second attempt means a poll was slept, so
                // an elapsed time of one poll or more IS a wait.
                let waited = since.elapsed();
                if waited >= LOCK_POLL {
                    log(lock_acquired_line(waited));
                }
                break;
            }
            Err(TryLockError::WouldBlock) => {
                let waited = since.elapsed();
                if waited >= budget {
                    // The line every journal reader and cadence.rs's own
                    // doc comment greps for goes out first, for every
                    // phase. A phase that abandoned a request then says
                    // WHAT it abandoned.
                    log(lock_contended_line(waited));
                    match contended(&phase) {
                        Contended::Covered => {
                            if budget > Duration::ZERO {
                                // Waited the whole budget and never ran:
                                // the firing must not record this as
                                // rc=0.
                                std::process::exit(LOCK_CONTENDED_EXIT);
                            }
                            // Leaving at once finished the job: the
                            // holder is doing this very work.
                            return Ok(());
                        }
                        // But an operator's cancel is nobody else's
                        // work. Say what did not happen, and exit
                        // non-zero so a script, a unit or a person
                        // reading the output cannot mistake it for done.
                        Contended::Abandoned(msg) => {
                            log(&msg);
                            // (The lock releases with the process;
                            // destructors are moot.)
                            std::process::exit(LOCK_CONTENDED_EXIT);
                        }
                    }
                }
                if waited < LOCK_POLL {
                    log(lock_waiting_line(budget));
                }
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).context("locking the conductor's lock file");
            }
        }
    }
    let problems = preflight(&cfg)?;
    if !problems.is_empty() {
        for p in &problems {
            log(format!("preflight FAIL: {p}"));
        }
        // (The lock releases with the process; destructors are moot.)
        std::process::exit(PREFLIGHT_FAIL_EXIT);
    }
    log("preflight ok");
    if matches!(phase, Phase::Preflight) {
        return Ok(());
    }
    // POLICY IS RESOLVED ONCE, HERE, and threaded from this point on.
    // One read per invocation means every decision in this run is taken
    // against one coherent set of rules, and the version is a fact the
    // journal and the train's own record can both name.
    let conductor = Conductor::new(cfg, forge)?;
    let policy = conductor.resolve_policy().await;
    let conductor = conductor.with_policy(policy);
    if let Phase::Cancel { handle, reason } = &phase {
        return conductor.cancel(handle, reason).await;
    }
    for work in loop_work(&phase) {
        match work {
            Work::DrainPublishRequests => drain_publish_requests(&conductor, dry, now).await,
            Work::Reconcile => conductor.reconcile(now).await?,
            Work::Board => conductor.board(now).await?,
        }
    }
    Ok(())
}

/// Drain publish-request packets before the pass looks for cars, so a
/// branch a credential-less workspace filed since the last tick is on
/// the forge before reconcile/board see the dock — gateable in this
/// cycle instead of the next one. Same clone the conductor assembles
/// in; same `fork` remote `publish_car_branch` pushes car branches to.
///
/// Same failure posture as the branch sweep: the drain is a feeder,
/// not the train. A packet that will not drain (or a jobs API that is
/// away) is journaled and retried next tick; the conductor run that
/// called it stands — which is why this returns `()` and not a
/// `Result`. (A fallible write that could fail the loop froze every
/// landing once; see the sweep's own note.)
async fn drain_publish_requests(conductor: &Conductor, dry: bool, now: DateTime<Utc>) {
    if let Err(e) = crate::publish_requests::run(&conductor.cfg.clone, "fork", dry, now).await {
        log(format!("publish-request drain failed (run stands): {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // A cancel that cancelled nothing must not exit 0.
    // ---------------------------------------------------------------

    /// THE DEFECT (found 2026-09-10 by the builder of
    /// `fix/a-refused-consist-opens-no-train`). Losing the conductor's
    /// lock returned `Ok(())` from every phase, so `boss train cancel`
    /// printed success-shaped output and exited 0 without releasing a
    /// single car. An operator reads "cancelled", believes three cars
    /// are back on the dock, and they are still aboard a dead train
    /// holding the track — §Diagnosis's component that answered instead
    /// of erroring.
    ///
    /// Leaving AT ONCE is correct and deliberate; cancel must never
    /// wait on a contended lock. Only the report was wrong — so this
    /// pins BOTH halves, because a fix to either one alone is wrong:
    /// a zero budget that reports success is the defect, and a truthful
    /// report bought by making the operator's verb queue behind the
    /// loop is the regression.
    #[test]
    fn a_cancel_leaves_at_once_and_does_not_report_success() {
        let phase = Phase::Cancel {
            handle: "abcd1234".to_string(),
            reason: "CI red on a consist nobody can fix".to_string(),
        };
        assert_eq!(
            lock_wait_budget(&phase),
            Duration::ZERO,
            "an operator is standing at the prompt — cancel never waits"
        );
        let Contended::Abandoned(msg) = contended(&phase) else {
            panic!("a cancel that released no car has not succeeded");
        };
        // The operator must be able to read WHAT did not happen from
        // the line itself, without re-deriving it from the train.
        assert!(msg.contains("abcd1234"), "names the train: {msg}");
        assert!(msg.contains("NOT cancelled"), "names the omission: {msg}");
        assert!(msg.contains("lock"), "names the cause: {msg}");
    }

    /// The standing loop's phases are a different case, and stay quiet.
    /// The lock holder is running reconcile + board right now, so the
    /// work this invocation came to do is being done by the process
    /// that beat it here; a preflight has nothing further to prove
    /// while the locomotive is demonstrably pulling. Nothing else in
    /// the system will cancel an operator's named train.
    ///
    /// `Covered` is about abandonment, not about the exit code: a
    /// reconcile that spent its whole budget is still covered by the
    /// holder, and exits [`LOCK_CONTENDED_EXIT`] because it never ran.
    #[test]
    fn the_standing_loop_leaves_quietly_because_the_holder_covers_it() {
        for phase in [Phase::Preflight, Phase::Reconcile, Phase::Board, Phase::Run] {
            assert_eq!(contended(&phase), Contended::Covered);
        }
    }

    // ---------------------------------------------------------------
    // The publish-request drain rides every reconcile (03e81aa9).
    // ---------------------------------------------------------------

    /// THE DEFECT, measured 2026-09-15: publish-request 9f9fa486 was
    /// filed 16:47Z from a credential-less dev pod; train-reconcile
    /// fired at 17:00:45Z (rc 0) and the packet stayed at its publish
    /// step, because only `Phase::Run` drained — and the cadence
    /// registry fires `run` at 06:05 and 18:05 (rule train-window)
    /// while `reconcile` fires every ten minutes (rule
    /// train-reconcile). A 78-minute wait for a one-commit bundle; up
    /// to 12 h in the worst case. This is the table `run` walks, so
    /// what it asserts is the dispatch, not a description of it.
    #[test]
    fn a_reconcile_tick_drains_publish_requests_before_it_looks_for_cars() {
        assert_eq!(
            loop_work(&Phase::Reconcile),
            &[Work::DrainPublishRequests, Work::Reconcile],
            "the 10-minute tick must push a filed branch before it reconciles"
        );
        assert_eq!(
            loop_work(&Phase::Run),
            &[Work::DrainPublishRequests, Work::Reconcile, Work::Board],
            "the window still drains first, then reconciles, then boards"
        );
        // A board has its own 60-second retry and must not queue
        // behind a forge push; preflight returns before the table is
        // read; cancel is an operator's verb on one named train.
        assert_eq!(loop_work(&Phase::Board), &[Work::Board]);
        assert!(loop_work(&Phase::Preflight).is_empty());
        let cancel = Phase::Cancel {
            handle: "t".into(),
            reason: "r".into(),
        };
        assert!(loop_work(&cancel).is_empty());
    }
}
