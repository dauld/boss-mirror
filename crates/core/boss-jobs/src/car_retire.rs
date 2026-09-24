//! The writes that RETIRE a car whose work another car carries — the
//! pure half of `boss car retire` (backlog 87f1c86a).
//!
//! WHY. On 2026-09-24 four parked cars were twins of work that had
//! already landed: car D's commit rode inside car E, the two device-shop
//! cars inside the refurb car (all on train #634), and an old car G was
//! replaced by its rebuild. Nothing could close them. `boss car` had
//! only `open` and `waits-on`; `boss merged` answered UNKNOWN (main
//! matched 2/7, 29/33 and 73/75 of their files, because a train
//! squashes and a replay resolves conflicts); `boss rerail` refuses a
//! vanished branch. So they were held by hand to stop the conductor
//! leaving them behind every train, and the dock read HELD 4 and the
//! garage stayed amber — red that lingers and trains its readers to
//! ignore it.
//!
//! TWO RETIREMENTS, TWO TERMINALS, BOTH THROUGH THE DESIGNED PATH.
//!
//! - CARRIED — the car's commits landed inside another car. Its
//!   terminal is `landed-twin` (ship-a-change), whose two required
//!   fields — `carried_by` and `evidence` — are written onto the step
//!   BEFORE the marker readies it, so the terminal cannot be reached
//!   without the proof. What counts as proof is the CLI's to decide
//!   (git lives there); this module only refuses to write a blank one.
//! - SUPERSEDED — the car was replaced by another car for the same
//!   item, and its own work never landed. That is the `abandoned`
//!   terminal, which has said exactly that since the protocol began; it
//!   gains a named successor rather than a new outcome.
//!
//! Each is the same four writes, in the order the CLI performs them and
//! the HTTP test in `tests/ship_a_change_landed_twin.rs` drives against
//! the real router: the evidence onto the outcome step through the
//! merge door (it is still pending, so the door takes it), the hold off
//! the review step (it is still open), the marker onto the job (the
//! metadata write re-evaluates, so the outcome goes ready), then the
//! outcome's status. The hold comes off BEFORE the marker because the
//! marker can close the car — the dispatcher completes a ready outcome
//! on its own — and a closed car's review step is frozen, so a hold
//! released afterwards would be refused and ride the record forever.

use serde_json::{Value, json};

use crate::car::{BUILD, BUILD_SLUG, REVIEW, REVIEW_SLUG, StepWrite, find_step, is_open};

/// The `landed-twin` outcome's slug and title — the terminal a carried
/// car closes through.
pub const LANDED_TWIN_SLUG: &str = "landed-twin";
pub const LANDED_TWIN: &str = "Retired — its commits landed inside another car";
/// The job-metadata marker its `ready_when` waits on.
pub const LANDED_TWIN_MARKER: &str = "landed_twin";

/// The `abandoned` outcome — the terminal a superseded car closes
/// through, and its marker.
pub const ABANDONED_SLUG: &str = "abandoned";
pub const ABANDONED: &str = "Abandoned";
pub const ABANDONED_MARKER: &str = "abandoned";

/// The carrier, on the `landed-twin` step (a required field) and on
/// the job, where the yard and a later reader find it without opening
/// the step.
pub const CARRIED_BY: &str = "carried_by";
/// The successor, on the `abandoned` step and on the job.
pub const SUPERSEDED_BY: &str = "superseded_by";
/// The proof, on the outcome step (a required field of `landed-twin`).
pub const EVIDENCE: &str = "evidence";
/// The same proof on the job, so the packet's own metadata says why it
/// closed without a reader having to find the step.
pub const RETIRE_EVIDENCE: &str = "retire_evidence";

/// How a car is being retired, and the proof — already judged by the
/// caller. `by` is what the operator named and the caller resolved: the
/// carrier's branch or merge sha, or the successor's car id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retirement {
    /// Its commits landed inside another car.
    CarriedBy { by: String, evidence: String },
    /// Another car replaced it; its own work never landed.
    SupersededBy { by: String, evidence: String },
}

impl Retirement {
    fn parts(
        &self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &str,
        &str,
    ) {
        match self {
            Self::CarriedBy { by, evidence } => (
                LANDED_TWIN_SLUG,
                LANDED_TWIN,
                LANDED_TWIN_MARKER,
                CARRIED_BY,
                by,
                evidence,
            ),
            Self::SupersededBy { by, evidence } => (
                ABANDONED_SLUG,
                ABANDONED,
                ABANDONED_MARKER,
                SUPERSEDED_BY,
                by,
                evidence,
            ),
        }
    }

    /// The outcome step this retirement closes the car through.
    pub fn outcome_slug(&self) -> &'static str {
        self.parts().0
    }
}

/// Everything a retirement writes, in the order it is written.
#[derive(Debug, Clone, PartialEq)]
pub struct RetireWrites {
    /// FIRST: the evidence onto the outcome step through the merge
    /// door, THEN its status — the status PUT is the LAST write.
    pub outcome: StepWrite,
    /// SECOND: the review step whose hold comes off, when it carries
    /// one. `None` when the car is not held.
    pub held_review: Option<String>,
    /// THIRD: the job-metadata merge — the marker the outcome's
    /// `ready_when` waits on, the carrier or successor, and the proof.
    pub marker: Value,
}

/// A marker that is present, read the way the dock reads `hold`
/// (`metadata_unmarked`): an empty string or `false` is released.
fn marked(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

fn status(step: &Value) -> &str {
    step.get("status").and_then(Value::as_str).unwrap_or("?")
}

fn short(car: &Value) -> &str {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    &id[..8.min(id.len())]
}

/// PURE: the writes that retire `car`, or the refusal.
///
/// Refuses a closed car (nothing to retire), a car aboard a train (it is
/// the train's to land or release — retiring it under a consist would
/// close a passenger mid-run), a blank carrier or proof, a car whose
/// pinned protocol has no such terminal (named, with the conversion that
/// gives it one), a terminal already taken, and — for a carried car — a
/// build not yet done, because a car still building has no finished
/// commits to prove carried and the terminal waits on `build`.
pub fn retire_writes(car: &Value, r: &Retirement) -> Result<RetireWrites, String> {
    let (slug, title, marker_key, by_key, by, evidence) = r.parts();
    let id = short(car);
    if !is_open(car) {
        return Err(format!(
            "car {id} is already {} (outcome {}) — there is nothing left to retire",
            car.get("status").and_then(Value::as_str).unwrap_or("?"),
            car.pointer("/metadata/outcome")
                .and_then(Value::as_str)
                .unwrap_or("none recorded")
        ));
    }
    if let Some(train) = car
        .pointer("/metadata/train")
        .filter(|t| marked(Some(t)))
        .map(|t| t.as_str().unwrap_or("?").to_string())
    {
        return Err(format!(
            "car {id} is aboard train {} — a car in transit is the train's to land or \
             release. Retire it once the train has left it at the dock.",
            &train[..8.min(train.len())]
        ));
    }
    if by.trim().is_empty() || evidence.trim().is_empty() {
        return Err(format!(
            "a retirement records WHO carries the work and the PROOF, and one of them is \
             blank (`{by_key}` {by:?}) — no evidence is not a pass"
        ));
    }
    let Some(outcome) = find_step(car, slug, title) else {
        return Err(format!(
            "car {id} is pinned to ship-a-change v{}, which has no `{slug}` terminal. An \
             in-flight packet keeps the version it was admitted under; `boss job convert \
             {id}` moves it to the active version, and if that version has no `{slug}` \
             either, `boss workflow publish ship-a-change` is owed first.",
            car.get("workflow_version")
                .map(Value::to_string)
                .unwrap_or_else(|| "?".into())
        ));
    };
    if !matches!(status(outcome), "pending" | "ready") {
        return Err(format!(
            "car {id}'s `{slug}` terminal is {} — it cannot be taken again",
            status(outcome)
        ));
    }
    if slug == LANDED_TWIN_SLUG {
        let build = find_step(car, BUILD_SLUG, BUILD)
            .map(status)
            .unwrap_or("absent");
        if build != "completed" {
            return Err(format!(
                "car {id}'s build is {build} — a car still building has no finished commits \
                 to prove carried. If its work went elsewhere, it was superseded: \
                 `--superseded-by <car>`."
            ));
        }
    }
    let outcome_id = outcome
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("car {id}'s `{slug}` step has no id"))?;
    let held_review = find_step(car, REVIEW_SLUG, REVIEW)
        .filter(|s| matches!(status(s), "pending" | "ready" | "active"))
        .filter(|s| marked(s.pointer("/metadata/hold")))
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .map(str::to_string);
    let by = by.trim();
    let evidence = evidence.trim();
    Ok(RetireWrites {
        outcome: StepWrite {
            step_id: outcome_id.to_string(),
            title,
            metadata: json!({ (by_key): by, (EVIDENCE): evidence }),
            status_body: json!({"status": "completed"}),
        },
        held_review,
        marker: json!({
            (marker_key): "true",
            (by_key): by,
            (RETIRE_EVIDENCE): evidence,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parked, HELD car on a protocol version that carries the
    /// `landed-twin` terminal — the shape of the four cars on
    /// 2026-09-24 once converted.
    fn held_car() -> Value {
        json!({
            "id": "64a4b9ea-0000-0000-0000-000000000000",
            "status": "open",
            "workflow_version": 2,
            "metadata": {"branch": "feat/a-region-owns-its-page-2-rerail"},
            "steps": [
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "completed"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "ready",
                 "metadata": {"hold": "Duplicate: its commits ride inside a newer green car"}},
                {"id": "s-abandoned", "spec_slug": "abandoned", "title": ABANDONED,
                 "status": "pending"},
                {"id": "s-twin", "spec_slug": LANDED_TWIN_SLUG, "title": LANDED_TWIN,
                 "status": "pending"},
            ],
        })
    }

    fn carried() -> Retirement {
        Retirement::CarriedBy {
            by: "feat/it-shop-floor-actors-and-a-plant-strip-2-rerail".into(),
            evidence: "1 commit carried: 697951cd ~ 2213c8f7".into(),
        }
    }

    /// THE CARRIED RETIREMENT: evidence onto `landed-twin` in the two
    /// fields it requires, the hold off the review step, and the marker
    /// its `ready_when` waits on — with the carrier and the proof on the
    /// job too.
    #[test]
    fn a_carried_car_closes_through_landed_twin_with_its_evidence() {
        let w = retire_writes(&held_car(), &carried()).unwrap();
        assert_eq!(w.outcome.step_id, "s-twin");
        assert_eq!(
            w.outcome.metadata,
            json!({
                "carried_by": "feat/it-shop-floor-actors-and-a-plant-strip-2-rerail",
                "evidence": "1 commit carried: 697951cd ~ 2213c8f7",
            })
        );
        assert_eq!(w.outcome.status_body, json!({"status": "completed"}));
        assert_eq!(w.held_review.as_deref(), Some("s-review"));
        assert_eq!(w.marker["landed_twin"], "true");
        assert_eq!(
            w.marker["carried_by"],
            "feat/it-shop-floor-actors-and-a-plant-strip-2-rerail"
        );
        assert_eq!(
            w.marker["retire_evidence"],
            "1 commit carried: 697951cd ~ 2213c8f7"
        );
    }

    /// A SUPERSEDED car is an abandoned one with a named successor — the
    /// terminal that has always meant "this car's work did not land".
    #[test]
    fn a_superseded_car_closes_through_abandoned_naming_its_successor() {
        let r = Retirement::SupersededBy {
            by: "d4439bcf-5e48-4010-856b-4a94ffa54ccc".into(),
            evidence: "same item c3105b2a".into(),
        };
        let w = retire_writes(&held_car(), &r).unwrap();
        assert_eq!(w.outcome.step_id, "s-abandoned");
        assert_eq!(
            w.outcome.metadata["superseded_by"],
            "d4439bcf-5e48-4010-856b-4a94ffa54ccc"
        );
        assert_eq!(w.marker["abandoned"], "true");
        assert!(w.marker.get("landed_twin").is_none(), "{}", w.marker);
    }

    /// An unheld car has no hold to release — and a hold released by
    /// writing `false` or `""` is released already.
    #[test]
    fn only_a_held_review_is_released() {
        for released in [Value::Null, json!(false), json!(""), json!("  ")] {
            let mut car = held_car();
            car["steps"][1]["metadata"]["hold"] = released.clone();
            let w = retire_writes(&car, &carried()).unwrap();
            assert_eq!(w.held_review, None, "{released}");
        }
    }

    /// A CAR PINNED TO A VERSION WITHOUT THE TERMINAL is refused, naming
    /// the conversion — every car open on 2026-09-24 was pinned to v1.
    #[test]
    fn a_car_whose_version_lacks_the_terminal_is_refused_naming_the_conversion() {
        let mut car = held_car();
        car["workflow_version"] = json!(1);
        car["steps"].as_array_mut().unwrap().pop();
        let e = retire_writes(&car, &carried()).unwrap_err();
        assert!(e.contains("boss job convert 64a4b9ea"), "{e}");
        assert!(e.contains("v1"), "{e}");
        assert!(e.contains("boss workflow publish ship-a-change"), "{e}");
    }

    /// No evidence is not a pass: a blank carrier or proof is refused.
    #[test]
    fn a_blank_carrier_or_proof_is_refused() {
        for r in [
            Retirement::CarriedBy {
                by: " ".into(),
                evidence: "x".into(),
            },
            Retirement::CarriedBy {
                by: "feat/x".into(),
                evidence: "".into(),
            },
        ] {
            let e = retire_writes(&held_car(), &r).unwrap_err();
            assert!(e.contains("no evidence is not a pass"), "{e}");
        }
    }

    /// Closed, aboard a train, already taken, or still building — each
    /// refused, each naming why.
    #[test]
    fn a_car_that_is_not_retirable_is_refused_naming_why() {
        let mut closed = held_car();
        closed["status"] = json!("closed");
        closed["metadata"]["outcome"] = json!("merged");
        let e = retire_writes(&closed, &carried()).unwrap_err();
        assert!(e.contains("already closed (outcome merged)"), "{e}");

        let mut aboard = held_car();
        aboard["metadata"]["train"] = json!("fcaf834a-76a1-4323-9b32-0363adf50206");
        let e = retire_writes(&aboard, &carried()).unwrap_err();
        assert!(e.contains("aboard train fcaf834a"), "{e}");

        let mut taken = held_car();
        taken["steps"][3]["status"] = json!("skipped");
        let e = retire_writes(&taken, &carried()).unwrap_err();
        assert!(e.contains("is skipped"), "{e}");

        let mut building = held_car();
        building["steps"][0]["status"] = json!("active");
        let e = retire_writes(&building, &carried()).unwrap_err();
        assert!(e.contains("build is active"), "{e}");
        assert!(e.contains("--superseded-by"), "{e}");
    }
}
