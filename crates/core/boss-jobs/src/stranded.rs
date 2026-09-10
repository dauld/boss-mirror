//! THE ONE DEFINITION of "a green gate-run that never became a car".
//!
//! Three readers ask this question and each used to answer it with its
//! own copy of the marker list: the yard read-model
//! ([`crate::yard::stranded_greens`] / `held_greens`), the CLI census
//! (`boss packet census`, which `boss orient` renders), and the
//! conductor's stranded-green ALARM, which files a backlog-item an
//! operator must triage. The copies drifted exactly the way CLAUDE.md
//! §9a says they do: on 2026-09-08 the yard learned to exclude a
//! re-railed or held green (car 3c1f843f) and the alarm did not, so
//! four false "STRANDED GREEN" packets sat in the queue on 2026-09-09
//! for branches that had landed, been re-railed, or were deliberately
//! held (backlog e60398dc). The rule now lives HERE, once; the readers
//! differ only in how they hand this module a gate-run's metadata.
//!
//! WHAT THE MARKERS MEAN — every one of them is a fact only an operator
//! or another actor can know, so each arrives as an annotation on the
//! gate-run packet rather than as a derivation here:
//!
//! - `superseded` — the green is dead: the branch was deleted, or the
//!   same change landed by another branch (754b01b5).
//! - `hold` — the green is deliberately waiting: gated on purpose
//!   without a park, e.g. a car that must land at a David-timed
//!   restart. A brake deliberately on is not an alarm.
//! - `rerailed_to` — spent: `boss rerail --finish` moved the car onto
//!   another branch, and without this stamp the ORIGINAL branch read as
//!   stranded forever (69daaba2).
//! - `park_skipped` — the auto-park handler DECIDED not to file a car
//!   (the branch had already landed, or its car was already aboard a
//!   train). A recorded decision is the opposite of a strand: the
//!   handler ran, looked, and declined.
//!
//! `park_summary` is not a marker but an INTENT: its presence means the
//! gate carried `--park-*` flags, so `jobs.auto-park` owed this green a
//! car. Measured 2026-09-09 over the 12 most recent green-with-intent
//! gate-runs on the system of record: 11 had their car filed 0.6
//! SECONDS after the verdict. So an intent-carrying green with no car
//! and no `park_skipped` is not "waiting" — it is the handler having
//! failed, and the alarm says so rather than waiting out a threshold
//! built for the manual case.

use serde_json::Value;

/// A green gate-run no car claims, once the markers have spoken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnparkedGreen {
    /// The branch the gate-run gated.
    pub branch: String,
    /// `Some(reason)` when an operator deliberately held it — a held
    /// green lists apart from a stranded one and never alarms.
    pub hold: Option<String>,
    /// Did this gate carry a `--park-*` intent? True means
    /// `jobs.auto-park` owed it a car.
    pub park_intent: bool,
}

impl UnparkedGreen {
    /// A green nobody held: gated on purpose and then forgotten.
    pub fn is_stranded(&self) -> bool {
        self.hold.is_none()
    }
}

/// The verdict value that counts as a pass. The runner writes exactly
/// one verdict onto the `record-verdict` step.
pub const VERDICT_GREEN: &str = "green";

/// Metadata keys that make a gate-run SPENT — no longer a candidate to
/// become a car, whatever its verdict says.
const SPENT_MARKERS: [&str; 3] = ["superseded", "rerailed_to", "park_skipped"];

/// THE definition of "is this marker set", read the way `superseded`
/// has always been read: `null`, `false` and `""` are NO marker; `true`
/// is a marker with no reason; a non-blank string is the reason. Every
/// marker in this module shares this shape, because an operator writing
/// `hold: true` and an operator writing `hold: "waiting for the
/// restart"` mean the same thing.
///
/// Public because the shape is not local to gate-runs. The station
/// predicate language's `StepMatch::metadata_unmarked` — how the
/// loading-dock row says "a held car is not boardable" — calls this
/// rather than carrying its own rule, and so does the conductor's
/// `parked_ready`. The trap that makes the reuse load-bearing: a
/// RELEASED hold is written `hold: false`, so any rule built on "the
/// key is missing or null" reads a released car as held forever.
pub fn marked(md: &Value, key: &str) -> Option<String> {
    match md.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Bool(true)) => Some("no reason recorded".to_string()),
        Some(Value::Number(_)) => Some("no reason recorded".to_string()),
        _ => None,
    }
}

/// Why this gate-run is spent, if it is — the marker's name, for a
/// message that says what silenced it rather than silently skipping.
pub fn spent_reason(gate_run_metadata: &Value) -> Option<&'static str> {
    SPENT_MARKERS
        .into_iter()
        .find(|k| marked(gate_run_metadata, k).is_some())
}

/// The operator's hold reason, if the gate-run carries one. `true` with
/// no text reads "no reason recorded" — the same words the web approach
/// lens uses.
pub fn hold_reason(gate_run_metadata: &Value) -> Option<String> {
    marked(gate_run_metadata, "hold")
}

/// Did this gate carry a `--park-*` park intent (so `jobs.auto-park`
/// owed it a car)? `park_summary` is the flag the handler itself keys
/// on: no summary, no auto-park.
pub fn park_intent(gate_run_metadata: &Value) -> bool {
    gate_run_metadata
        .get("park_summary")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

/// Did any of these step metadata objects record a green verdict?
/// Callers hand in `&Step.metadata` (typed readers) or
/// `step["metadata"]` (JSON readers) — the only shape difference left
/// between them.
pub fn any_green<'a>(step_metadata: impl IntoIterator<Item = &'a Value>) -> bool {
    step_metadata
        .into_iter()
        .any(|m| m.get("verdict").and_then(Value::as_str) == Some(VERDICT_GREEN))
}

/// THE definition. `None` when the gate-run is spent (superseded,
/// re-railed, or skipped by auto-park with a reason), recorded no green
/// verdict, names no branch, or a car already claims its branch.
/// Otherwise the branch, its hold reason if any, and whether a park
/// intent was stamped.
///
/// `claimed` answers "does a car already carry this branch?" — the
/// callers hold that set in different containers and read it off
/// `metadata.branch`, never the car's subject (a re-railed car's
/// subject is the branch it was FILED under, not the one it carries).
pub fn unparked_green<'a>(
    gate_run_metadata: &Value,
    step_metadata: impl IntoIterator<Item = &'a Value>,
    claimed: impl Fn(&str) -> bool,
) -> Option<UnparkedGreen> {
    if spent_reason(gate_run_metadata).is_some() {
        return None;
    }
    if !any_green(step_metadata) {
        return None;
    }
    let branch = gate_run_metadata
        .get("branch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|b| !b.is_empty())?;
    if claimed(branch) {
        return None;
    }
    Some(UnparkedGreen {
        branch: branch.to_string(),
        hold: hold_reason(gate_run_metadata),
        park_intent: park_intent(gate_run_metadata),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn green() -> Vec<Value> {
        vec![json!({"verdict": "green"})]
    }

    fn call(md: &Value, steps: &[Value], cars: &[&str]) -> Option<UnparkedGreen> {
        unparked_green(md, steps.iter(), |b| cars.contains(&b))
    }

    /// The base case: a green gate-run whose branch no car carries is
    /// an unparked green, and with no hold it is STRANDED.
    #[test]
    fn a_green_with_no_car_is_stranded() {
        let got = call(&json!({"branch": "fix/a"}), &green(), &[]).expect("unparked");
        assert_eq!(got.branch, "fix/a");
        assert!(got.is_stranded());
        assert!(!got.park_intent);
    }

    /// A car carrying the branch answers the question: nothing to
    /// rescue.
    #[test]
    fn a_claimed_branch_is_not_unparked() {
        assert!(call(&json!({"branch": "fix/a"}), &green(), &["fix/a"]).is_none());
    }

    /// No green verdict, no claim — an open or red gate-run is not a
    /// strand.
    #[test]
    fn a_run_with_no_green_verdict_is_not_unparked() {
        let red = [json!({"verdict": "failed"})];
        assert!(call(&json!({"branch": "fix/a"}), &red, &[]).is_none());
        assert!(call(&json!({"branch": "fix/a"}), &[], &[]).is_none());
    }

    /// The three markers that make a green SPENT, each in the shape an
    /// operator actually writes. This is the drift that filed four
    /// false alarms on 2026-09-09 (e60398dc).
    #[test]
    fn a_spent_green_is_not_unparked() {
        for md in [
            json!({"branch": "fix/a", "superseded": true}),
            json!({"branch": "fix/a", "superseded": "landed via another branch"}),
            json!({"branch": "fix/a", "rerailed_to": "fix/a-rerail"}),
            json!({"branch": "fix/a", "park_skipped": "landed"}),
        ] {
            assert!(call(&md, &green(), &[]).is_none(), "{md} should be spent");
            assert!(spent_reason(&md).is_some());
        }
    }

    /// A blank or false marker is NO marker — the empty string a
    /// metadata merge leaves behind must not silence a real strand.
    #[test]
    fn a_blank_marker_is_no_marker() {
        for md in [
            json!({"branch": "fix/a", "superseded": false}),
            json!({"branch": "fix/a", "superseded": null}),
            json!({"branch": "fix/a", "rerailed_to": "  "}),
            json!({"branch": "fix/a", "park_skipped": ""}),
        ] {
            assert!(spent_reason(&md).is_none(), "{md} carries no marker");
            assert!(call(&md, &green(), &[]).is_some(), "{md} is still stranded");
        }
    }

    /// A HELD green is unparked but never stranded, and it carries the
    /// operator's reason. `true` with no text still holds.
    #[test]
    fn a_held_green_is_unparked_but_not_stranded() {
        let md = json!({"branch": "fix/a", "hold": "waiting for a timed restart"});
        let got = call(&md, &green(), &[]).expect("unparked");
        assert!(!got.is_stranded());
        assert_eq!(got.hold.as_deref(), Some("waiting for a timed restart"));

        let bare = json!({"branch": "fix/a", "hold": true});
        let got = call(&bare, &green(), &[]).expect("unparked");
        assert_eq!(got.hold.as_deref(), Some("no reason recorded"));
    }

    /// The park intent rides through, so a reader can tell "the handler
    /// owed this a car" from "a human gated by hand".
    #[test]
    fn a_park_intent_rides_through() {
        let md = json!({"branch": "fix/a", "park_summary": "does the thing"});
        assert!(call(&md, &green(), &[]).expect("unparked").park_intent);
        let blank = json!({"branch": "fix/a", "park_summary": ""});
        assert!(!call(&blank, &green(), &[]).expect("unparked").park_intent);
    }

    /// A gate-run with no branch names nothing an operator could
    /// rescue.
    #[test]
    fn a_run_with_no_branch_is_not_unparked() {
        assert!(call(&json!({}), &green(), &[]).is_none());
        assert!(call(&json!({"branch": ""}), &green(), &[]).is_none());
    }
}
