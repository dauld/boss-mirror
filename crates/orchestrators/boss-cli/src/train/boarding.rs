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
// ---------------------------------------------------------------------------

/// The structured marker a boarding-edge hold leaves on its `left_behind`
/// entry, so the window's own refusal line is composed from DATA and not
/// from sniffing the reason string back apart.
pub(crate) const EDGE_HOLD: &str = "edge_hold";
/// The predecessor is still in flight — self-clearing, no action.
pub(crate) const EDGE_HOLD_WAITING: &str = "waiting";
/// The edge can never be satisfied as declared — a person must act.
pub(crate) const EDGE_HOLD_NEEDS_HUMAN: &str = "needs_human";

/// What the conductor managed to learn about a car's declared
/// predecessor. `Unreadable` is a first-class answer, not an error:
/// "I could not ask" must be distinguishable from "it is not there".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Predecessor {
    /// The Job came back, judged by `boss_jobs::car`'s own predicates so
    /// the conductor cannot disagree with the rest of the system about
    /// what "landed" means.
    Found(Value),
    /// The jobs API answered that there is no such Job.
    Absent,
    /// The read itself failed — a blip, an outage, a malformed body.
    Unreadable(String),
}

/// Why a car may not board on its declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeHold {
    /// The reason, journal and Job chip alike — ONE string, as every
    /// other skip reason here is.
    pub(crate) reason: String,
    /// `EDGE_HOLD_WAITING` or `EDGE_HOLD_NEEDS_HUMAN`.
    pub(crate) kind: &'static str,
}

/// What boarding should do about a car's declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EdgeOutcome {
    /// Board it: the edge is satisfied, or there is none.
    Board,
    /// Board it, and SAY why the edge could not be judged. Fail-open by
    /// design — see the section comment.
    BoardUnjudged(String),
    /// Leave it behind, with the reason named on it.
    Hold(EdgeHold),
}

/// The predecessor a car declared, if it declared one. A blank value is
/// no declaration — the metadata door deletes a null key but a `""` is a
/// real stored value, and `jobs_clear_waiting` shows `""` is how an edge
/// gets cleared in practice.
pub(crate) fn declared_predecessor(car: &Value) -> Option<String> {
    car.pointer(&format!("/metadata/{}", car::BOARDS_AFTER))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// How to name a predecessor in a refusal an operator reads: its BRANCH
/// where we have it, never a bare id (MEMORY: refer by protocol + title).
/// The id8 rides along so the packet is still findable.
fn predecessor_name(declared: &str, pred: Option<&Value>) -> String {
    let branch = pred
        .and_then(|p| p.pointer("/metadata/branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() {
        format!("car {}", id8(declared))
    } else {
        format!("{branch} (car {})", id8(declared))
    }
}

/// Where a live predecessor actually is, so "still in flight" names a
/// place rather than asserting a mood. The three states are core's
/// (`is_boarded` / `is_parked` / `is_building`), in the order a car
/// passes through them backwards.
fn in_flight_at(pred: &Value) -> String {
    if car::is_boarded(pred) {
        let train = pred
            .pointer("/metadata/train")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if train.is_empty() {
            "aboard a train".to_string()
        } else {
            format!("aboard train {}", id8(train))
        }
    } else if car::is_parked(pred) {
        "parked at the dock".to_string()
    } else if car::is_building(pred) {
        "still building".to_string()
    } else {
        "open".to_string()
    }
}

/// How a spent predecessor ended, read off the packet rather than
/// guessed, so the refusal says what the record says.
fn spent_as(pred: &Value) -> String {
    let status = pred
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("not open");
    match pred.pointer("/metadata/outcome").and_then(Value::as_str) {
        Some(o) if !o.is_empty() => format!("{status}, outcome '{o}'"),
        _ => format!("{status}, no landing recorded"),
    }
}

/// PURE: what boarding does about one car's declared edge.
///
/// The four situations, and the exact words each gets. They are written
/// to be told apart at a glance by an operator scanning the journal:
/// "STILL IN FLIGHT" carries "no action needed", and both unsatisfiable
/// cases carry "a human must". That distinction is the feature — not the
/// hold (David on d3320278: "the refusal's wording matters as much as its
/// existence").
pub(crate) fn boards_after_outcome(declared: &str, pred: &Predecessor) -> EdgeOutcome {
    match pred {
        // FAIL-OPEN, LOUDLY. A car that would have boarded yesterday must
        // not be held because the system of record blipped while the
        // conductor asked about its edge.
        Predecessor::Unreadable(cause) => EdgeOutcome::BoardUnjudged(format!(
            "boards after car {}, and that packet could not be read ({cause}) — boarding \
             anyway: an unreadable edge is not evidence of a collision, and holding the \
             dock on a read failure would stop every train",
            id8(declared)
        )),
        Predecessor::Absent => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after car {}, which DOES NOT EXIST — a human must fix \
                 metadata.{} on this car (the edge is ref-checked at the write, so this \
                 id was stored before the edge was declared, or with ref-checking off)",
                id8(declared),
                car::BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
        }),
        Predecessor::Found(p) if car::is_landed(p) => EdgeOutcome::Board,
        Predecessor::Found(p) if car::is_open(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which is STILL IN FLIGHT ({}) — no action needed; this \
                 car boards on a later window once that one lands",
                predecessor_name(declared, Some(p)),
                in_flight_at(p)
            ),
            kind: EDGE_HOLD_WAITING,
        }),
        Predecessor::Found(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which was ABANDONED ({}) — the edge can never be \
                 satisfied; a human must clear metadata.{} on this car, or abandon it too",
                predecessor_name(declared, Some(p)),
                spent_as(p),
                car::BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
        }),
    }
}

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
// The declared ordering edge — the four refusals, and the fail-open.
// ---------------------------------------------------------------------------

/// WHAT AN OPERATOR DEPENDS ON HERE IS THE WORDING, so the wording is
/// what these assert. David on d3320278: *"the refusal's wording matters
/// as much as its existence: the dock's no-departure line is read by an
/// operator deciding whether the pipeline is stuck, so 'held: boards
/// after <car>, which is abandoned' has to be distinguishable from
/// 'held: boards after <car>, still in flight' — the first needs a
/// human, the second does not."*
///
/// A test that only checked "it held" would let the four collapse into
/// one message a release later, which is the quiet hold the feature
/// exists to remove.
#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome, NoDeparture, Predecessor,
        boards_after_outcome, declared_predecessor, empty_dock_refusal, no_departure_line,
    };
    use serde_json::{Value, json};

    const PRED: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    /// A predecessor packet: open, with a branch, and whatever extra
    /// metadata / steps the situation needs.
    fn pred(status: &str, md: Value, steps: Value) -> Value {
        let mut metadata = json!({"branch": "fix/the-predecessor"});
        if let (Some(dst), Some(src)) = (metadata.as_object_mut(), md.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({"id": PRED, "status": status, "metadata": metadata, "steps": steps})
    }

    fn review(status: &str) -> Value {
        json!([{"spec_slug": "review", "status": status}])
    }

    fn hold_reason(declared: &str, p: &Predecessor) -> String {
        match boards_after_outcome(declared, p) {
            EdgeOutcome::Hold(h) => h.reason,
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    /// (1) STILL IN FLIGHT — nobody needs to do anything, and the line
    /// says so outright. It also names WHERE the predecessor is, because
    /// "in flight" alone sends the reader to the yard to find out.
    #[test]
    fn a_predecessor_in_flight_holds_and_asks_for_nobody() {
        let p = Predecessor::Found(pred(
            "open",
            json!({"train": "77777777-aaaa-bbbb-cccc-dddddddddddd"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(
            r.contains("STILL IN FLIGHT") && r.contains("aboard train 77777777"),
            "it must name the state AND where: {r}"
        );
        assert!(
            r.contains("no action needed"),
            "an operator deciding whether the pipeline is stuck must be told it is not: {r}"
        );
        assert!(
            !r.contains("human"),
            "a self-clearing hold must never read as one that needs a person: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_WAITING
        );
    }

    /// The dock and the build are in-flight states too, and each names
    /// itself — a car waiting on one still building is a different wait
    /// from one waiting on a car about to merge.
    #[test]
    fn in_flight_names_the_dock_and_the_build_separately() {
        let parked = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        assert!(parked.contains("parked at the dock"), "{parked}");
        let building = hold_reason(
            PRED,
            &Predecessor::Found(pred(
                "open",
                json!({}),
                json!([{"spec_slug": "gate", "status": "ready"}]),
            )),
        );
        assert!(building.contains("still building"), "{building}");
    }

    /// (2) LANDED — the edge is satisfied and the car boards. If this
    /// ever holds, the bug is in the filter and not on the dock.
    #[test]
    fn a_landed_predecessor_satisfies_the_edge() {
        for landed in [
            pred("closed", json!({"outcome": "merged"}), json!([])),
            pred("open", json!({"merged": "true"}), review("ready")),
        ] {
            assert_eq!(
                boards_after_outcome(PRED, &Predecessor::Found(landed.clone())),
                EdgeOutcome::Board,
                "a landed predecessor must board its successor: {landed}"
            );
        }
    }

    /// (3) ABANDONED — it can NEVER clear, so the line says a human must
    /// act, says what to do, and reports how the record says it ended.
    #[test]
    fn an_abandoned_predecessor_names_a_human_and_what_to_clear() {
        let p = Predecessor::Found(pred(
            "closed",
            json!({"outcome": "abandoned"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(r.contains("ABANDONED"), "{r}");
        assert!(
            r.contains("can never be satisfied"),
            "waiting is futile and the line must say so: {r}"
        );
        assert!(
            r.contains("a human must clear metadata.boards_after"),
            "name the fix, not just the fault: {r}"
        );
        assert!(
            r.contains("closed, outcome 'abandoned'"),
            "report what the record says, not a guess: {r}"
        );
        assert!(
            !r.contains("no action needed"),
            "this one DOES need action: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_NEEDS_HUMAN
        );
    }

    /// A cancelled predecessor is spent, not landed — the same refusal,
    /// and it must not be read as in flight just because `outcome` is
    /// missing.
    #[test]
    fn a_cancelled_predecessor_is_spent_not_in_flight() {
        let r = hold_reason(
            PRED,
            &Predecessor::Found(pred("cancelled", json!({}), review("ready"))),
        );
        assert!(
            r.contains("ABANDONED") && r.contains("cancelled, no landing recorded"),
            "{r}"
        );
    }

    /// (4) NO SUCH JOB — a human must fix the REFERENCE, which is a
    /// different repair from breaking a live edge, so it gets different
    /// words. The line also says this should have been impossible, so the
    /// reader knows to suspect the write path and not the car.
    #[test]
    fn a_dangling_edge_says_the_job_does_not_exist() {
        let r = hold_reason(PRED, &Predecessor::Absent);
        assert!(r.contains("DOES NOT EXIST"), "{r}");
        assert!(
            r.contains("a human must fix metadata.boards_after"),
            "fix the reference, do not break the edge: {r}"
        );
        assert!(r.contains("ref-checked"), "say why this is surprising: {r}");
    }

    /// THE ASSERTION THE FEATURE IS TRUSTED ON: no two of the four read
    /// the same, and each side of the needs-a-human line is recognisable
    /// without reading the whole sentence.
    #[test]
    fn the_four_situations_are_told_apart_by_their_words() {
        let in_flight = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        let abandoned = hold_reason(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "abandoned"}), json!([]))),
        );
        let absent = hold_reason(PRED, &Predecessor::Absent);
        let unjudged = match boards_after_outcome(PRED, &Predecessor::Unreadable("boom".into())) {
            EdgeOutcome::BoardUnjudged(note) => note,
            other => panic!("an unreadable edge must still board: {other:?}"),
        };
        let landed = boards_after_outcome(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "merged"}), json!([]))),
        );

        let all = [&in_flight, &abandoned, &absent, &unjudged];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two situations read identically");
            }
        }
        assert_eq!(landed, EdgeOutcome::Board, "landed is not a refusal at all");
        // The one-glance test: does this need a person?
        assert!(!in_flight.contains("human") && in_flight.contains("no action needed"));
        assert!(abandoned.contains("a human must") && !abandoned.contains("no action needed"));
        assert!(absent.contains("a human must") && !absent.contains("no action needed"));
        assert!(unjudged.contains("boarding anyway"));
    }

    /// THE HAZARD THIS CAR WAS WARNED ABOUT. A read failure must not
    /// hold the dock: the conductor boards the car it cannot judge and
    /// says why, loudly. Refusing everything it could not evaluate would
    /// freeze every landing, and the gate does not run the conductor.
    #[test]
    fn an_unreadable_edge_boards_the_car_and_says_why() {
        let note = match boards_after_outcome(
            PRED,
            &Predecessor::Unreadable("HTTP 503 Service Unavailable".into()),
        ) {
            EdgeOutcome::BoardUnjudged(n) => n,
            other => panic!("fail-open is the whole point: {other:?}"),
        };
        assert!(note.contains("HTTP 503"), "carry the cause: {note}");
        assert!(note.contains("boarding anyway"), "{note}");
        assert!(
            note.contains("would stop every train"),
            "say why fail-open is the right choice here: {note}"
        );
    }

    /// THE REGRESSION THAT MATTERS MOST: every car in flight today has
    /// no edge, and must behave exactly as it did before this car.
    #[test]
    fn a_car_with_no_edge_declares_no_predecessor() {
        for md in [
            json!({"branch": "fix/x"}),
            json!({"branch": "fix/x", "boards_after": ""}),
            json!({"branch": "fix/x", "boards_after": "   "}),
            json!({"branch": "fix/x", "boards_after": Value::Null}),
        ] {
            let car = json!({"id": "c", "status": "open", "metadata": md});
            assert_eq!(
                declared_predecessor(&car),
                None,
                "no edge, or a cleared one, is not a constraint: {car}"
            );
        }
        let declared = json!({"id": "c", "metadata": {"boards_after": PRED}});
        assert_eq!(declared_predecessor(&declared).as_deref(), Some(PRED));
    }

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
