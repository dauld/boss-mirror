//! Why a board departs nothing — the no-departure line and the declared ordering edge.

use super::*;

// ---------------------------------------------------------------------------
// A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET
//
// Ten hours of delivery went to this on 2026-09-10 (backlog 4860aff8).
// ONE car sat on the dock that the consist check refused. The board
// cadence is queue-depth based and re-fires every 60 seconds while the
// dock stays deep, and every attempt OPENED a pr-train Job and then
// cancelled it: `pr-train` total passed 982, the newest 100 rows all
// closed inside 99 minutes, 89 `outcome: cancelled`, and 100 of 100
// with no `boarded_jobs`. That is ~1,440 phantom packets a day from one
// unboardable car. It turned the branch sweep's 50-row window over in
// under an hour (02069932) and it lied to every window read — the yard,
// `boss orient`, and any count of how many trains ran.
//
// The refusal itself was already right: no PR opened, no CI spent. What
// it did not skip was the PACKET, because the Job was created BEFORE
// the check ran — for one reason, that the refusal wanted somewhere to
// record itself. The record is cheaper than that:
//
//   - each car keeps its own `skip_reason`, and for a consist refusal
//     the structured `consist_refusal` too — the lint output, on the
//     car whose boarding it blocked, where the operator already looks;
//   - the journal gets ONE line, this one, which names the reason and
//     says outright that no packet was opened, so nobody goes hunting
//     the yard for a train that never existed.
//
// A packet is a fact that something happened; a train that never
// departed is not one. And until it was cancelled it also HELD THE
// TRACK, so a cancel lost to one API blip wedged boarding behind a
// train that had never left the yard.
// ---------------------------------------------------------------------------

/// Why a board attempt departed no train. Every one of these used to
/// open a pr-train Job and immediately cancel it through the `empty`
/// marker; none of them opens anything now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoDeparture {
    /// The CI host is short of what a run needs — an infrastructure
    /// refusal, decided before a single car was collected.
    HostShort { reason: String },
    /// Nothing was parked and ready when the window opened.
    NothingParked,
    /// Every candidate conflicted on the assembled tree.
    AllConflicted { branches: String },
    /// The consist check refused the assembled tree. Nobody's car is at
    /// fault — each was green on its own branch — so every car stays
    /// boardable and unstruck.
    ConsistRefused { reason: String, cars: usize },
    /// Every car on the dock is held on a declared ordering edge
    /// (`metadata.boards_after`) — see `boards_after_outcome`.
    ///
    /// TWO LISTS, BECAUSE THEY ASK DIFFERENT THINGS OF THE READER. A car
    /// waiting on a predecessor that is still in flight needs nobody: the
    /// next board is 60 seconds away and it departs on its own. A car
    /// whose predecessor was abandoned, or whose edge names no Job at
    /// all, can NEVER depart on the edge it declares, and the window will
    /// refuse identically forever until a person clears it. An operator
    /// reading this line is deciding whether the pipeline is stuck, so
    /// the line has to answer that and not merely report a hold
    /// (d3320278).
    HeldOnEdges { cars: String, needs_human: String },
}

/// Will this refusal still be here on the next window, unchanged?
///
/// WHY THE DISTINCTION IS THE WHOLE ALARM (backlog 6baabd43). From
/// 04:27Z to 13:49Z on one day no train departed. The conductor never
/// stopped and never failed: it fired every minute, took its lock, ran
/// preflight, evaluated all three parked cars and logged in full —
/// naming all three branches and the conflicting files for each. Nine
/// and a half hours of perfect diagnosis with zero reach: no packet, no
/// alarm, no surface. Meanwhile the yard rendered `3 cars parked — the
/// boarding depth is met, a train is due`, which is exactly what it says
/// two minutes after a healthy departure.
///
/// AND AN ALARM ON "NO TRAIN DEPARTED" ALONE WOULD BE NOISE. Most
/// windows refuse for reasons that clear themselves within a minute —
/// an idle dock, a car waiting on a predecessor still in flight. The
/// packet is explicit that a dock-depth alarm "would fire on every
/// healthy busy dock, which is how a check becomes noise and then
/// becomes unread". So the signal is not "nothing departed"; it is
/// "nothing departed FOR A REASON THAT WILL NOT CLEAR ITSELF".
///
/// The enum already carries that fact, which is why this is a total
/// match and not a heuristic:
///
/// - `NothingParked` — an idle window. The next parked car departs.
/// - `HeldOnEdges` with nobody needing a human — each car boards by
///   itself once the car it named has landed.
/// - `HostShort` — an infrastructure refusal that clears when the host
///   does, and which says nothing about any branch.
///
/// against the three that repeat identically until a person acts:
///
/// - `AllConflicted` — every candidate conflicts on the assembled tree,
///   and will again on the next window, and the next.
/// - `ConsistRefused` — the assembled tree is refused; nobody's car is
///   at fault and nothing on the dock can change it.
/// - `HeldOnEdges` with `needs_human` — an edge that can never be
///   satisfied; the window refuses identically forever.
pub(crate) fn refusal_persists(refusal: &NoDeparture) -> bool {
    match refusal {
        NoDeparture::NothingParked | NoDeparture::HostShort { .. } => false,
        NoDeparture::HeldOnEdges { needs_human, .. } => !needs_human.is_empty(),
        NoDeparture::AllConflicted { .. } | NoDeparture::ConsistRefused { .. } => true,
    }
}

// ---------------------------------------------------------------------------
// A SKIP THAT REPEATS IS A STALL (backlog 94896e74)
//
// A single skip is ROUTINE and must never fire anything: measured
// 2026-09-22, 52 of 132 trains (39%) carried a skipped branch and still
// departed, and one branch was skipped 23 consecutive times and landed
// fine. What has no escalation is REPETITION. On 2026-09-22 the
// conductor refused to board from 07:01Z to 14:21Z — roughly 420
// identical firings on one conflicted car — diagnosed it perfectly,
// filed ONE alarm at 07:01Z, deduplicated correctly by staying open,
// and nothing read it for seven hours.
//
// So the repair verb is not what is missing (`boss rerail` exists, and
// would not have saved that day: the conflict was real and stopped for
// a human by design). What is missing is the SIGNAL, in two places:
//
//   - the car counts its consecutive skips (`next_skip_count`), so a
//     repeatedly-skipped car LOOKS troubled where an operator already
//     reads the dock, rather than hiding behind one reason string that
//     says nothing about how many windows have refused it;
//   - the alarm ESCALATES on a ladder (`stall_escalation`) instead of
//     merely persisting, so an unread packet's number grows with the
//     wait.
//
// AND THE DEDUP IS NOT BROKEN BY ANY OF IT. The alarm deduplicates by
// staying open, and closing it is what re-arms it; an escalation that
// filed twins would be worse than the silence it replaces. Every write
// here lands on THAT SAME packet, and the ladder is finite, so the
// escalation costs at most three writes however long the stall runs.
// ---------------------------------------------------------------------------

/// How many consecutive windows have skipped this car, counting the one
/// being stamped now.
///
/// Read off the car's own `skips` stamp, which boarding CLEARS in the
/// same write that clears `skip_reason` — so the count is consecutive by
/// construction and a car that rides a train starts again at one. A
/// stamp that is not a non-negative integer reads as no stamp: a
/// malformed value must not paint repetition that did not happen, the
/// same reading `red_trains_of` gives the strike count (2bb0d014).
pub(crate) fn next_skip_count(car: &Value) -> u64 {
    car.get("metadata")
        .and_then(|m| m.get("skips"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1)
}

/// The escalation ladder for an open boarding-stall alarm, in minutes
/// since it was filed. Three rungs, not a rung a minute: the packet is
/// already `priority: urgent` when it is filed, so an escalation is
/// worth a write only when the NUMBER on it has meaningfully grown.
/// The measured stall (07:01Z → 14:21Z) reaches the top rung.
pub(crate) const STALL_ESCALATION_MINS: [i64; 3] = [30, 120, 360];

/// A rung of that ladder, reached and not yet recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StallEscalation {
    /// Which rung: 1, 2 or 3.
    pub level: u64,
    /// How long the alarm has been open, in minutes — the number that
    /// grows, and the one an operator is actually owed.
    pub minutes: i64,
}

/// Which rung an open boarding-stall alarm has reached, when that is
/// HIGHER than the rung it already records — `None` otherwise, which is
/// most windows.
///
/// Dated from the alarm's own `stalled_since` metadata rather than the
/// Job's `opened_on`, which is a DATE and cannot answer a 30-minute
/// question. An alarm filed before this stamp existed has no
/// `stalled_since` and is not judged here at all: the caller stamps it
/// and the next window judges it, which is the honest answer rather
/// than a duration invented from a date.
pub(crate) fn stall_escalation(alarm: &Value, now: DateTime<Utc>) -> Option<StallEscalation> {
    let md = alarm.get("metadata")?;
    let since = md
        .get("stalled_since")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())?;
    let minutes = (now - since).num_minutes();
    let recorded = md
        .get("escalation_level")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let level = STALL_ESCALATION_MINS
        .iter()
        .filter(|rung| minutes >= **rung)
        .count() as u64;
    (level > recorded).then_some(StallEscalation { level, minutes })
}

/// The message an escalation writes onto the alarm packet — the wait,
/// the repair, and the current window's own refusal line, so a reader
/// who opens the packet at the escalation needs no second read.
pub(crate) fn stall_escalation_message(escalation: &StallEscalation, line: &str) -> String {
    let StallEscalation { level, minutes } = escalation;
    let rungs = STALL_ESCALATION_MINS.len();
    format!(
        "STILL STALLED — {minutes} minutes after this packet was filed the identical \
         refusal is still firing, and nobody has acted (escalation {level} of {rungs}). \
         The window's line right now:\n\n{line}\n\n\
         REPAIR: each skipped car carries its own skip_reason naming the conflict, and \
         its skips count naming how many windows have refused it. `boss rerail <car>` \
         puts a conflict-skipped car back aboard — new branch from current main, rebase, \
         gate, receipt copied onto the car — and stops for a person on a REAL conflict, \
         which is what that stop is for. Closing this packet re-arms the alarm.\n\n\
         This is an escalation of the packet that was already open, not a new one: no \
         twin was filed, and the ladder is finite, so a stall costs at most {rungs} of \
         these however long it runs (backlog 94896e74)."
    )
}

/// The journal line a refused board leaves. It is the only record of
/// the window now, so it carries the reason AND the fact that no packet
/// was opened; a reader who greps `no train departed` gets every
/// non-departure, whatever refused it.
pub(crate) fn no_departure_line(refusal: &NoDeparture) -> String {
    match refusal {
        NoDeparture::HostShort { reason } => format!(
            "no train departed — boarding refused before any car was collected: {reason}. \
             No train packet opened, no PR, no CI spent."
        ),
        NoDeparture::NothingParked => "no train departed — no car was parked and ready when the \
             window opened: an idle window, not a failure. No train packet opened."
            .to_string(),
        NoDeparture::AllConflicted { branches } => format!(
            "no train departed — every candidate was skipped on merge conflicts: {branches}. \
             No train packet opened, no PR, no CI spent; each car carries its own skip_reason."
        ),
        NoDeparture::ConsistRefused { reason, cars } => format!(
            "no train departed — {reason}. No train packet opened, no PR, no CI spent — \
             {cars} car(s) stay boardable and unstruck."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } if needs_human.is_empty() => format!(
            "no train departed — every car on the dock is held on the ordering edge it \
             declared: {cars}. No train packet opened. NOTHING NEEDS DOING: each boards by \
             itself on a later window, once the car it named has landed."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } => format!(
            "no train departed — every car on the dock is held on the ordering edge it \
             declared: {cars}. No train packet opened. A HUMAN IS NEEDED for {needs_human}: \
             that edge can never be satisfied, so this window will refuse identically until \
             someone clears it — each car's own skip_reason names which."
        ),
    }
}

// ---------------------------------------------------------------------------
// THE DECLARED ORDERING EDGE — a car names the car it boards AFTER
//
// Four cars were held by hand on 2026-09-10 and every one of them was an
// ordering constraint (backlog d3320278; design doc 364f892e, all three
// questions accepted as proposed). `fix/the-reclaim-follows-the-build`
// was gated `--hold` and parked by hand when the dock happened to be
// empty, purely so it would board SOLO as a gate-blind ci.yml change;
// `feat/a-deleted-manifest-leaves-no-object` spent a day waiting for a
// human to notice both halves of a two-part sequence were satisfied. The
// operator WAS the mechanism, and the mechanism was re-reading the dock.
//
// FILTER, DO NOT PLAN (decision 3). Boarding skips a car whose declared
// predecessor has not landed; it does not compute a multi-train
// sequence. A plan buys nothing the filter does not, because the next
// board happens on its own inside the cooldown. If the dock ever grows
// deep enough that planning is visibly better, that is a second car.
//
// AN UNSATISFIABLE EDGE REFUSES AND IS NAMED (decision 2). Boarding
// anyway after a timeout was rejected: it reintroduces exactly the
// collision the edge prevents. So the refusal is LOUD on every 60-second
// board attempt, and it is FOUR refusals rather than one, because an
// operator reading the dock is deciding whether the pipeline is stuck:
//
//   still in flight  — nobody does anything; it departs on its own
//   landed           — satisfied; the car boards (no refusal at all)
//   abandoned        — a human must break the edge; it will never clear
//   no such Job      — a human must fix the reference
//
// Collapsing those into "dependency not met" would build the quiet hold
// this feature exists to remove (CLAUDE.md §Diagnosis: quiet is a loan
// against the next diagnosis).
//
// AND IT MUST NEVER FREEZE A LANDING. A fallible read inside the board
// loop that refused everything it could not evaluate would stop every
// train, and the gate does not run the conductor, so the gate would not
// catch it. Every way the read can fail therefore DEGRADES TOWARD
// BOARDING, loudly: `Unreadable` is a journal line and the car rides.
// The edge's job is to stop a known collision, not to become a new way
// for the pipeline to stop.
//
// THE JUDGEMENT ITSELF LIVES IN CORE since backlog 4142d821 (design
// cf820810 Q7): `boss_jobs::car::boards_after_outcome`, with the four
// situations' words and the fail-open. The dock region asks the same
// question of the same edge, and it said "a train is due" while this
// conductor refused the car every window, because until then only this
// file could answer. What stays here is the conductor's half: the READ
// (`edge_hold` in conductor.rs, and `is_no_such_job` below) and the
// window's own refusal line (`empty_dock_refusal`).
// ---------------------------------------------------------------------------

pub(crate) use boss_jobs::car::{
    EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeHold, EdgeOutcome, Predecessor,
    boards_after_outcome, declared_predecessor,
};

/// PURE: is this jobs-API error the ANSWER "there is no such Job",
/// rather than "I could not ask"? The difference decides whether a car
/// is held for a person to fix or boarded anyway, so it is worth a named
/// function and a test instead of an inline `.contains` at the call site.
///
/// THE PAIR THIS PINS (CLAUDE.md §9a). The status lives in
/// `ApiFailure.kind` as `Failure::Http(404)`, but `api` returns
/// `anyhow::Error` — the classifier's structure is gone by the time a
/// caller sees it, and surfacing it would mean a second shape for every
/// call in this file. So the status is read back out of the message
/// `api_once` builds, and `a_404_is_read_back_out_of_the_message_api_once_builds`
/// constructs that message the way `api_once` does so the two cannot
/// drift silently. Collapsing this properly means `api` handing back the
/// `Failure`; that is a wider change than this car.
pub(crate) fn is_no_such_job(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string().contains("HTTP 404"))
}

/// PURE: the refusal an empty candidate list deserves.
///
/// `NothingParked` says "an idle window, not a failure" — true when the
/// dock is empty, a LIE when the dock is full of cars the edge filter
/// held, and the difference is exactly what an operator is reading the
/// line to learn. Composed from the structured `EDGE_HOLD` marker on
/// each left-behind entry, never from re-parsing the reason prose.
pub(crate) fn empty_dock_refusal(left_behind: &[Value]) -> NoDeparture {
    let named = |want: &str| -> Vec<String> {
        left_behind
            .iter()
            .filter(|e| e.get(EDGE_HOLD).and_then(Value::as_str) == Some(want))
            .filter_map(|e| e.get("car_id_short").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    };
    let waiting = named(EDGE_HOLD_WAITING);
    let needs_human = named(EDGE_HOLD_NEEDS_HUMAN);
    if waiting.is_empty() && needs_human.is_empty() {
        return NoDeparture::NothingParked;
    }
    let mut cars = waiting;
    cars.extend(needs_human.iter().cloned());
    NoDeparture::HeldOnEdges {
        cars: cars.join(", "),
        needs_human: needs_human.join(", "),
    }
}

#[cfg(test)]
mod no_departure_tests {
    use super::{
        CONDUCTOR_LOCK_WAIT, LOCK_CONTENDED_EXIT, NoDeparture, PREFLIGHT_FAIL_EXIT, Phase,
        lock_acquired_line, lock_contended_line, lock_wait_budget, lock_waiting_line,
        no_departure_line,
    };
    use std::time::Duration;

    /// THE FLOOD, in one assertion. Ten hours on 2026-09-10 (4860aff8):
    /// one car the consist check refused, a board firing every 60
    /// seconds, and every attempt opened a pr-train Job and cancelled
    /// it — 982 trains, the newest 100 closed inside 99 minutes, 100 of
    /// 100 with no `boarded_jobs`. The refusal already spends no PR and
    /// no CI; it must spend no PACKET either, and the journal line is
    /// now the only place an operator learns a window refused, so it
    /// has to say all three.
    #[test]
    fn a_refused_consist_opens_no_train_packet_and_says_so() {
        let line = no_departure_line(&NoDeparture::ConsistRefused {
            reason: "consist check: the-live-rules-are-the-authored-rules failed".to_string(),
            cars: 3,
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(
            line.contains("the-live-rules-are-the-authored-rules"),
            "the refusal names the check that refused: {line}"
        );
        assert!(
            line.contains("No train packet opened"),
            "an operator must not go looking for a train that was never opened: {line}"
        );
        assert!(
            line.contains("3 car(s) stay boardable"),
            "a combination failure strikes nobody: {line}"
        );
    }

    /// An idle window is not a failure, and it is not a train either.
    #[test]
    fn an_idle_window_departs_nothing_and_opens_nothing() {
        let line = no_departure_line(&NoDeparture::NothingParked);
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("not a failure"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// Every candidate conflicted: the cars carry their own
    /// `skip_reason`, and the line names them so the journal answers
    /// "which branches" without a second read.
    #[test]
    fn an_all_conflicted_window_names_the_branches() {
        let line = no_departure_line(&NoDeparture::AllConflicted {
            branches: "fix/a, fix/b".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("fix/a, fix/b"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// An infrastructure refusal is not a consist failure — it says so,
    /// and it carries the host reason the readiness check measured.
    #[test]
    fn a_host_refusal_carries_the_measured_reason() {
        let line = no_departure_line(&NoDeparture::HostShort {
            reason: "forge: 3.1 GB free, floor is 20 GB".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("3.1 GB free"), "{line}");
        assert!(
            line.contains("before any car was collected"),
            "nobody's car is at fault: {line}"
        );
    }

    /// A lock whose loser logs and leaves is correct ONLY if it
    /// eventually wins. The board fires every 60 seconds and holds the
    /// lock 12–14 seconds in the consist check; the 10-minute reconcile
    /// fired about a second later and lost 55 times in a row, so no
    /// merge, no stall sentinel, no arrival report and no branch sweep
    /// ran for nine hours. The starvable side waits; the side that must
    /// never queue behind a 30-minute deploy does not.
    #[test]
    fn only_the_starvable_phases_wait_for_the_lock() {
        assert_eq!(lock_wait_budget(&Phase::Reconcile), CONDUCTOR_LOCK_WAIT);
        assert_eq!(lock_wait_budget(&Phase::Run), CONDUCTOR_LOCK_WAIT);
        assert_eq!(
            lock_wait_budget(&Phase::Board),
            Duration::ZERO,
            "a board must not queue behind a long reconcile — its firing records \
             boarded-nothing and the next tick re-fires"
        );
        assert_eq!(lock_wait_budget(&Phase::Preflight), Duration::ZERO);
        assert!(
            CONDUCTOR_LOCK_WAIT >= Duration::from_secs(60),
            "the budget has to outlast a board that departs a train, not just one that refuses"
        );
    }

    /// The three lines a contended lock can leave. Each names the
    /// waited time, because "leaving" with no number is what made nine
    /// hours of starvation look like nine hours of 0-second successes.
    #[test]
    fn the_lock_lines_name_the_waited_time() {
        let waiting = lock_waiting_line(Duration::from_secs(120));
        assert!(waiting.contains("120s"), "{waiting}");
        let acquired = lock_acquired_line(Duration::from_secs(13));
        assert!(acquired.contains("13s"), "{acquired}");
        // The zero-wait case keeps the line every journal reader and
        // cadence.rs's own doc comment already greps for.
        let left_at_once = lock_contended_line(Duration::ZERO);
        assert_eq!(
            left_at_once, "another conductor run holds the lock — leaving",
            "the no-wait line is unchanged"
        );
        // A zero-budget phase still burns nanoseconds between reading
        // the clock and failing the try — that is not a wait, and the
        // line must not claim one.
        assert_eq!(
            lock_contended_line(Duration::from_nanos(400)),
            left_at_once,
            "a sub-second elapsed is not a wait"
        );
        let timed_out = lock_contended_line(Duration::from_secs(120));
        assert!(
            timed_out.contains("120s") && timed_out.contains("holds the lock"),
            "{timed_out}"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, 0,
            "a pass that waited its whole budget and still never ran must not record rc=0"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, PREFLIGHT_FAIL_EXIT,
            "preflight failure already owns its code — two causes must not share one exit"
        );
    }
}

// ---------------------------------------------------------------------------
// The declared ordering edge — the conductor's half: the read and the
// window's line. The four refusals' words and the fail-open are judged in
// core now, and pinned there (`boss_jobs::car::boards_after_tests`).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, NoDeparture, empty_dock_refusal,
        no_departure_line,
    };
    use serde_json::json;

    const PRED: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    /// "NO SUCH JOB" AND "I COULD NOT ASK" MUST NOT BE CONFUSED: the
    /// first holds a car for a person to fix, the second boards it. The
    /// message below is built the way `api_once` builds it, which is what
    /// keeps this classification honest while `api` still flattens its
    /// `Failure` into an `anyhow::Error` (see `is_no_such_job`).
    #[test]
    fn a_404_is_read_back_out_of_the_message_api_once_builds() {
        // Verbatim shape from `api_once`:
        //   anyhow!("{method} {path}: HTTP {status}: {}", body.trim())
        let not_found = anyhow::anyhow!(
            "GET /api/jobs/{PRED}: HTTP 404 Not Found: {{\"error\":\"job not found\"}}"
        );
        assert!(super::is_no_such_job(&not_found));

        for blip in [
            anyhow::anyhow!("GET /api/jobs/{PRED}: HTTP 503 Service Unavailable: "),
            anyhow::anyhow!("error sending request for url (http://sor:7900/api/jobs/x)")
                .context("GET /api/jobs/x"),
            anyhow::anyhow!("job {PRED} came back empty"),
        ] {
            assert!(
                !super::is_no_such_job(&blip),
                "a blip must never read as an absence — it would hold a car for a person \
                 over a transient: {blip}"
            );
        }
    }

    /// An empty dock with no edge holds is still the idle window it
    /// always was — the pre-existing line, unchanged.
    #[test]
    fn an_empty_dock_with_no_edge_hold_is_an_idle_window() {
        assert_eq!(empty_dock_refusal(&[]), NoDeparture::NothingParked);
        let other = json!({"car_id_short": "aaaaaaaa", "reason": "branch fix/x not on fork"});
        assert_eq!(
            empty_dock_refusal(&[other]),
            NoDeparture::NothingParked,
            "a left-behind for some other reason is not an edge hold"
        );
    }

    /// A DOCK FULL OF HELD CARS IS NOT AN IDLE WINDOW, and the window's
    /// own line has to say which half of the hold it is — that is the
    /// decision an operator is reading it to make.
    #[test]
    fn an_empty_dock_held_on_edges_says_whether_a_human_is_needed() {
        let waiting = json!({"car_id_short": "aaaaaaaa", EDGE_HOLD: EDGE_HOLD_WAITING});
        let stuck = json!({"car_id_short": "cccccccc", EDGE_HOLD: EDGE_HOLD_NEEDS_HUMAN});

        let self_clearing = empty_dock_refusal(std::slice::from_ref(&waiting));
        assert_eq!(
            self_clearing,
            NoDeparture::HeldOnEdges {
                cars: "aaaaaaaa".into(),
                needs_human: String::new()
            }
        );
        let line = no_departure_line(&self_clearing);
        assert!(line.contains("no train departed"), "greppable: {line}");
        assert!(line.contains("NOTHING NEEDS DOING"), "{line}");
        assert!(line.contains("aaaaaaaa"), "name the car: {line}");

        let needs_human = empty_dock_refusal(&[waiting, stuck]);
        let line = no_departure_line(&needs_human);
        assert!(line.contains("A HUMAN IS NEEDED for cccccccc"), "{line}");
        assert!(
            !line.contains("NOTHING NEEDS DOING"),
            "one stuck car means the window is not self-clearing: {line}"
        );
        assert!(
            line.contains("aaaaaaaa"),
            "the waiting car is still listed as held: {line}"
        );
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    /// THE TOTAL SPLIT, asserted case by case so a new variant cannot
    /// be added without deciding which side it falls on — the compiler
    /// forces the match, and this forces the judgement.
    #[test]
    fn a_refusal_that_clears_itself_is_not_a_stall() {
        assert!(
            !refusal_persists(&NoDeparture::NothingParked),
            "an idle window is not a stall — the next parked car departs, and alarming \
             here is how a check becomes noise"
        );
        assert!(
            !refusal_persists(&NoDeparture::HostShort {
                reason: "9GB free, need 12GB".into()
            }),
            "an infrastructure refusal clears when the host does and says nothing about \
             any branch"
        );
        assert!(
            !refusal_persists(&NoDeparture::HeldOnEdges {
                cars: "fix/a".into(),
                needs_human: String::new()
            }),
            "a car waiting on a predecessor still in flight departs on its own, 60 \
             seconds later"
        );
    }

    /// The three that repeat identically until a person acts. These are
    /// the nine-and-a-half hours.
    #[test]
    fn a_refusal_that_repeats_until_someone_acts_is_a_stall() {
        assert!(
            refusal_persists(&NoDeparture::AllConflicted {
                branches: "fix/a, fix/b, fix/c".into()
            }),
            "every candidate conflicting on the assembled tree will conflict again on \
             the next window, and the next — this is the measured case (6baabd43)"
        );
        assert!(
            refusal_persists(&NoDeparture::ConsistRefused {
                reason: "two rule cars on one train".into(),
                cars: 3
            }),
            "the assembled tree is refused and nothing on the dock can change it"
        );
        assert!(
            refusal_persists(&NoDeparture::HeldOnEdges {
                cars: "fix/a, fix/b".into(),
                needs_human: "fix/b".into()
            }),
            "an edge that can never be satisfied refuses identically forever"
        );
    }

    /// The same variant falls on BOTH sides depending on its content,
    /// which is the reason this is a function over the value rather
    /// than a list of variant names.
    #[test]
    fn held_on_edges_splits_on_whether_anyone_is_needed() {
        let waiting = NoDeparture::HeldOnEdges {
            cars: "fix/a".into(),
            needs_human: String::new(),
        };
        let stuck = NoDeparture::HeldOnEdges {
            cars: "fix/a".into(),
            needs_human: "fix/a".into(),
        };
        assert!(!refusal_persists(&waiting));
        assert!(refusal_persists(&stuck));
        assert_ne!(
            refusal_persists(&waiting),
            refusal_persists(&stuck),
            "a classifier keyed on the variant alone would get one of these wrong"
        );
    }
}

#[cfg(test)]
mod repetition_tests {
    use super::*;

    #[test]
    fn a_first_skip_counts_one_and_a_repeat_counts_up() {
        let fresh = json!({"metadata": {"branch": "fix/a"}});
        assert_eq!(next_skip_count(&fresh), 1, "the first skip is one skip");
        let again = json!({"metadata": {"skips": 1}});
        assert_eq!(next_skip_count(&again), 2);
        let twenty_three = json!({"metadata": {"skips": 22}});
        assert_eq!(next_skip_count(&twenty_three), 23);
    }

    #[test]
    fn a_malformed_count_reads_as_a_first_skip_rather_than_painting_a_stall() {
        for stamp in [json!("lots"), json!(-3), json!(1.5), json!(null)] {
            let car = json!({"metadata": {"skips": stamp}});
            assert_eq!(
                next_skip_count(&car),
                1,
                "a count that is not a count must not invent repetition"
            );
        }
    }

    fn alarm(stalled_since: &str, level: u64) -> Value {
        json!({"metadata": {"stalled_since": stalled_since, "escalation_level": level}})
    }

    fn at(mins: i64) -> DateTime<Utc> {
        "2026-09-22T07:01:00Z".parse::<DateTime<Utc>>().unwrap() + chrono::Duration::minutes(mins)
    }

    #[test]
    fn a_stall_inside_the_first_rung_does_not_escalate() {
        let a = alarm("2026-09-22T07:01:00Z", 0);
        assert!(
            stall_escalation(&a, at(29)).is_none(),
            "the packet was filed 29 minutes ago and says so already — a second write \
             adds nothing"
        );
    }

    #[test]
    fn a_stall_that_outlives_a_rung_escalates_once_and_then_stays_put() {
        let a = alarm("2026-09-22T07:01:00Z", 0);
        let e = stall_escalation(&a, at(35)).expect("30 minutes unread is rung one");
        assert_eq!(e.level, 1);
        assert_eq!(e.minutes, 35);
        let escalated = alarm("2026-09-22T07:01:00Z", 1);
        assert!(
            stall_escalation(&escalated, at(45)).is_none(),
            "the rung is already recorded: escalating again every window would file the \
             noise this packet exists to avoid"
        );
        assert_eq!(
            stall_escalation(&escalated, at(130))
                .expect("two hours is rung two")
                .level,
            2
        );
        assert_eq!(
            stall_escalation(&alarm("2026-09-22T07:01:00Z", 2), at(440))
                .expect("the measured seven hours is the top rung")
                .level,
            3
        );
        assert!(
            stall_escalation(&alarm("2026-09-22T07:01:00Z", 3), at(600)).is_none(),
            "the ladder ends — an unbounded escalation is a write every window forever"
        );
    }

    #[test]
    fn an_alarm_with_no_stamp_cannot_be_judged_and_says_so() {
        let a = json!({"metadata": {}});
        assert!(
            stall_escalation(&a, at(600)).is_none(),
            "an alarm filed by an older conductor has no stalled_since; the caller \
             stamps it rather than guessing a duration"
        );
        let bad = json!({"metadata": {"stalled_since": "not a time"}});
        assert!(stall_escalation(&bad, at(600)).is_none());
    }

    #[test]
    fn the_escalation_sentence_names_the_wait_and_the_repair() {
        let msg = stall_escalation_message(
            &StallEscalation {
                level: 3,
                minutes: 440,
            },
            "no train departed — every candidate was skipped on merge conflicts: fix/a.",
        );
        assert!(msg.contains("440 minutes"), "{msg}");
        assert!(msg.contains("boss rerail"), "the repair verb: {msg}");
        assert!(
            msg.contains("fix/a"),
            "the current window's own line rides along: {msg}"
        );
        assert!(
            msg.contains("no twin"),
            "dedup is by this packet staying open, and the escalation must say it \
             has not broken that: {msg}"
        );
    }
}
