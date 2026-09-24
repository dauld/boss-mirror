//! The writes that close a LANDED car whose claim was MEASURED FALSE —
//! the pure half of `boss prove <car> --disproved` (backlog 08664157).
//!
//! WHY. Car 6b23d135 (fix/dev-pod-not-first-evicted) claimed boss-dev is
//! not the first eviction under disk pressure. After it converged,
//! boss-dev (priority 1000) was evicted for ephemeral-storage at
//! 2026-09-24T01:27Z — a request and a priority are inert once usage
//! exceeds the request (measured by 3f2a08ab). `boss prove` refuses to
//! record a failing probe, and rightly: "A probe that fails is evidence
//! AGAINST the claim … nothing is recorded either way." So the car could
//! only stand at `proven`, counted "ours" in the shed, for ever — the
//! red that lingers and trains its readers to ignore it. The fact was
//! written on the car by hand (`disproved_2026_09_24`), where nothing
//! reads it.
//!
//! ONE TERMINAL, THROUGH THE DESIGNED PATH. `disproved` (ship-a-change)
//! waits on `merged`, as `proven` does, and on its own marker; its three
//! required fields — `verified` (what the failure means), `disproof`
//! (the failing run, recorded verbatim as the `proof` record is) and
//! `superseded_by` (the landed car or backlog item that carries the real
//! remedy) — are written onto the step BEFORE the marker readies it, so
//! the terminal cannot be reached without the evidence. What counts as a
//! failing run is the CLI's to judge (it runs the probe); this module
//! only refuses to write a blank one, and judges the remedy it is
//! handed, because that is a question about records.
//!
//! The car is CLOSED, never deleted: the record keeps that the change
//! landed (`merged` stays "true") and that it did not do what it said.
//! The shed counts open cars at `proven`, so it stops counting this one
//! by construction. The arrival rule fires on `merged` only, so an item
//! the car answered stays open for its remedy to close.
//!
//! The writes reuse [`RetireWrites`] — the same three writes, in the
//! same order, as a retirement — and are pinned against the real router
//! in `tests/ship_a_change_disproved.rs`.

use serde_json::{Value, json};

use crate::car::{StepWrite, find_step, is_landed, is_open};
use crate::car_retire::RetireWrites;
pub use crate::car_retire::SUPERSEDED_BY;

/// The `disproved` outcome's slug and title — the terminal a car whose
/// claim was measured false closes through.
pub const DISPROVED_SLUG: &str = "disproved";
pub const DISPROVED: &str = "Disproved — it landed, and its claim was measured false";
/// The job-metadata marker its `ready_when` waits on.
pub const DISPROVED_MARKER: &str = "disproved";
/// What the failure means, in prose — required on the step.
pub const VERIFIED: &str = "verified";
/// The failing run, verbatim — required on the step.
pub const DISPROOF: &str = "disproof";
/// The remedy described for a reader, on the step and the job beside
/// `superseded_by`'s bare id.
pub const REMEDY: &str = "remedy";

/// The `proven` step, by slug and title, as `boss prove` addresses it.
const PROVEN_SLUG: &str = "proven";
const PROVEN: &str = "Proven in prod";

/// A disproof, already judged by the caller: the probe ran and said
/// false, and the remedy resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disproof {
    /// What the failing probe means, in prose.
    pub verified: String,
    /// The failing run: the serialised record `boss prove` writes as
    /// `proof`, with the probe, exit, streams, host and tree.
    pub disproof: String,
    /// The FULL id of the landed car or backlog item carrying the fix.
    pub superseded_by: String,
    /// That remedy described — what [`judge_remedy`] returned.
    pub remedy: String,
    /// RFC3339, the instant the probe ran under.
    pub completed_at: String,
}

fn md<'a>(c: &'a Value, k: &str) -> Option<&'a str> {
    c.pointer(&format!("/metadata/{k}"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn short(car: &Value) -> &str {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    &id[..8.min(id.len())]
}

fn status(step: &Value) -> &str {
    step.get("status").and_then(Value::as_str).unwrap_or("?")
}

/// PURE: can `car` be disproved at all? Checked BEFORE the probe runs,
/// so a car that cannot take the terminal costs a line, not a probe
/// against production. Open, landed (the conductor's `merged` marker),
/// and its `proven` step still open — a proven car's claim held once,
/// and a proof that stopped holding is `--recheck`'s NO LONGER HOLDS
/// and `--replace`'s to answer, not this terminal's.
pub fn disprovable(car: &Value) -> Result<(), String> {
    let id = short(car);
    if !is_open(car) {
        return Err(format!(
            "car {id} is already {} (outcome {}) — there is nothing left to disprove",
            car.get("status").and_then(Value::as_str).unwrap_or("?"),
            md(car, "outcome").unwrap_or("none recorded")
        ));
    }
    if md(car, "merged") != Some("true") {
        return Err(format!(
            "car {id} has not landed (no `merged` marker) — a change that never shipped \
             cannot be disproved in production. If it will not ship, it is abandoned: \
             `boss car retire {id} --superseded-by <car>`."
        ));
    }
    match find_step(car, PROVEN_SLUG, PROVEN).map(status) {
        Some("ready" | "active") => Ok(()),
        Some(other) => Err(format!(
            "car {id}'s `proven` step is {other} — only a car still owed its proof can be \
             disproved. A proof that stopped holding is `boss prove {id} --recheck`'s to \
             report and `--replace`'s to supersede."
        )),
        None => Err(format!(
            "car {id} has no `proven` step, so it is not a ship-a-change car this verb knows"
        )),
    }
}

/// PURE: the writes that close `car` through `disproved`, or the
/// refusal. [`disprovable`] first; then the evidence must be non-blank,
/// and the car's pinned protocol must carry the terminal (named, with
/// the conversion that gives it one) still untaken.
pub fn disprove_writes(car: &Value, d: &Disproof) -> Result<RetireWrites, String> {
    disprovable(car)?;
    let id = short(car);
    for (key, v) in [
        (VERIFIED, &d.verified),
        (DISPROOF, &d.disproof),
        (SUPERSEDED_BY, &d.superseded_by),
    ] {
        if v.trim().is_empty() {
            return Err(format!(
                "a disproof records the failing run, what it means and the remedy, and \
                 `{key}` is blank — no evidence is not a pass"
            ));
        }
    }
    let Some(outcome) = find_step(car, DISPROVED_SLUG, DISPROVED) else {
        return Err(format!(
            "car {id} is pinned to ship-a-change v{}, which has no `{DISPROVED_SLUG}` \
             terminal. An in-flight packet keeps the version it was admitted under; `boss \
             job convert {id}` moves it to the active version, and if that version has no \
             `{DISPROVED_SLUG}` either, `boss workflow publish ship-a-change` is owed first.",
            car.get("workflow_version")
                .map(Value::to_string)
                .unwrap_or_else(|| "?".into())
        ));
    };
    if !matches!(status(outcome), "pending" | "ready") {
        return Err(format!(
            "car {id}'s `{DISPROVED_SLUG}` terminal is {} — it cannot be taken again",
            status(outcome)
        ));
    }
    let outcome_id = outcome
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("car {id}'s `{DISPROVED_SLUG}` step has no id"))?;
    let by = d.superseded_by.trim();
    let remedy = d.remedy.trim();
    Ok(RetireWrites {
        outcome: StepWrite {
            step_id: outcome_id.to_string(),
            title: DISPROVED,
            metadata: json!({
                (VERIFIED): d.verified.trim(),
                (DISPROOF): d.disproof,
                (SUPERSEDED_BY): by,
                (REMEDY): remedy,
                "completed_at": d.completed_at,
            }),
            status_body: json!({"status": "completed"}),
        },
        // A landed car's review step is done; there is no hold to lift.
        held_review: None,
        marker: json!({
            (DISPROVED_MARKER): "true",
            (SUPERSEDED_BY): by,
            (REMEDY): remedy,
        }),
    })
}

/// PURE: the remedy `pointer` names for `car`, described — or the
/// refusal. A remedy is where the real fix lives: a ship-a-change car
/// that LANDED (and was not itself disproved), or a backlog item that
/// carries the work (not cancelled). Anything else points nowhere a
/// reader can follow.
pub fn judge_remedy(car: &Value, pointer: &Value) -> Result<String, String> {
    if car.get("id") == pointer.get("id") {
        return Err("a car cannot be its own remedy".into());
    }
    let pid = short(pointer);
    let pstatus = pointer.get("status").and_then(Value::as_str).unwrap_or("?");
    match pointer.get("kind").and_then(Value::as_str) {
        Some("ship-a-change") => {
            if md(pointer, "outcome") == Some(DISPROVED_SLUG) {
                return Err(format!(
                    "car {pid} was itself disproved — a remedy is a change that holds"
                ));
            }
            if !is_landed(pointer) {
                return Err(format!(
                    "car {pid} ({}) has not landed ({pstatus}) — a remedy car must already be \
                     on main. Name the backlog item it answers instead; the item outlives \
                     the car.",
                    md(pointer, "branch").unwrap_or("?")
                ));
            }
            Ok(format!(
                "landed car {pid} ({}, merged as {})",
                md(pointer, "branch").unwrap_or("?"),
                md(pointer, "merge_ref").unwrap_or("?")
            ))
        }
        Some("backlog-item") => {
            if pstatus == "cancelled" {
                return Err(format!(
                    "backlog item {pid} is cancelled — it carries no remedy"
                ));
            }
            Ok(format!(
                "backlog item {pid} ({pstatus}): {}",
                pointer.get("title").and_then(Value::as_str).unwrap_or("?")
            ))
        }
        other => Err(format!(
            "{pid} is a {} packet — the remedy is the landed car that fixed it, or the \
             backlog item that carries the fix",
            other.unwrap_or("kindless")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Car 6b23d135's shape once converted: landed, `proven` ready, the
    /// `disproved` terminal pending.
    fn landed_car() -> Value {
        json!({
            "id": "6b23d135-1bde-47cc-9bd5-5612c741b9f2",
            "kind": "ship-a-change",
            "status": "open",
            "workflow_version": 3,
            "metadata": {"branch": "fix/dev-pod-not-first-evicted", "merged": "true",
                         "merge_ref": "87c2e3667cdf",
                         // A stale `train` from the landing: the car is off
                         // the train, and it is no refusal here.
                         "train": "6648d6a4-80ed-4835-9224-5765ebd710f4"},
            "steps": [
                {"id": "s-review", "spec_slug": "review", "title": "Open for review",
                 "status": "completed"},
                {"id": "s-proven", "spec_slug": "proven", "title": PROVEN, "status": "ready"},
                {"id": "s-merged", "spec_slug": "merged", "title": "Merged",
                 "status": "pending"},
                {"id": "s-disproved", "spec_slug": DISPROVED_SLUG, "title": DISPROVED,
                 "status": "pending"},
            ],
        })
    }

    fn disproof() -> Disproof {
        Disproof {
            verified: "boss-dev was evicted for ephemeral-storage after this converged".into(),
            disproof: "{\"exit\":1}".into(),
            superseded_by: "11111111-2222-4333-8444-555555555555".into(),
            remedy: "landed car 11111111 (fix/the-scratch-floor-follows-the-build)".into(),
            completed_at: "2026-09-24T20:30:00Z".into(),
        }
    }

    /// THE WRITES: the three required fields and the remedy onto
    /// `disproved`, then the marker its `ready_when` waits on with the
    /// remedy on the job too — and no hold, because a landed car has
    /// none.
    #[test]
    fn a_landed_car_closes_through_disproved_with_the_failing_run() {
        let w = disprove_writes(&landed_car(), &disproof()).unwrap();
        assert_eq!(w.outcome.step_id, "s-disproved");
        assert_eq!(w.outcome.metadata["disproof"], "{\"exit\":1}");
        assert_eq!(
            w.outcome.metadata["superseded_by"],
            "11111111-2222-4333-8444-555555555555"
        );
        assert!(
            w.outcome.metadata["verified"]
                .as_str()
                .unwrap()
                .contains("evicted")
        );
        assert_eq!(w.outcome.metadata["completed_at"], "2026-09-24T20:30:00Z");
        assert_eq!(w.outcome.status_body, json!({"status": "completed"}));
        assert_eq!(w.held_review, None);
        assert_eq!(w.marker["disproved"], "true");
        assert_eq!(
            w.marker["superseded_by"],
            "11111111-2222-4333-8444-555555555555"
        );
        assert!(
            w.marker["remedy"]
                .as_str()
                .unwrap()
                .contains("scratch-floor")
        );
    }

    /// Not landed, already closed, already proven — each refused BEFORE
    /// a probe runs, each naming why.
    #[test]
    fn a_car_that_is_not_disprovable_is_refused_naming_why() {
        let mut unlanded = landed_car();
        unlanded["metadata"]["merged"] = json!("false");
        let e = disprovable(&unlanded).unwrap_err();
        assert!(e.contains("has not landed"), "{e}");
        assert!(e.contains("--superseded-by"), "{e}");

        let mut closed = landed_car();
        closed["status"] = json!("closed");
        closed["metadata"]["outcome"] = json!("merged");
        let e = disprovable(&closed).unwrap_err();
        assert!(e.contains("already closed (outcome merged)"), "{e}");

        let mut proven = landed_car();
        proven["steps"][1]["status"] = json!("completed");
        let e = disprovable(&proven).unwrap_err();
        assert!(e.contains("--recheck"), "{e}");

        assert!(disprovable(&landed_car()).is_ok());
    }

    /// No evidence is not a pass: a blank run, meaning or remedy.
    #[test]
    fn a_blank_field_is_refused() {
        for blank in ["verified", "disproof", "superseded_by"] {
            let mut d = disproof();
            match blank {
                "verified" => d.verified = " ".into(),
                "disproof" => d.disproof = String::new(),
                _ => d.superseded_by = "\n".into(),
            }
            let e = disprove_writes(&landed_car(), &d).unwrap_err();
            assert!(e.contains(&format!("`{blank}` is blank")), "{e}");
        }
    }

    /// A CAR PINNED TO A VERSION WITHOUT THE TERMINAL is refused, naming
    /// the conversion and the publish — 6b23d135 was pinned to v1.
    #[test]
    fn a_car_whose_version_lacks_the_terminal_is_refused_naming_the_conversion() {
        let mut car = landed_car();
        car["workflow_version"] = json!(1);
        car["steps"].as_array_mut().unwrap().pop();
        let e = disprove_writes(&car, &disproof()).unwrap_err();
        assert!(e.contains("boss job convert 6b23d135"), "{e}");
        assert!(e.contains("boss workflow publish ship-a-change"), "{e}");

        let mut taken = landed_car();
        taken["steps"][3]["status"] = json!("skipped");
        let e = disprove_writes(&taken, &disproof()).unwrap_err();
        assert!(e.contains("is skipped"), "{e}");
    }

    /// THE REMEDY: a landed car, or a live backlog item — and not the car
    /// itself, a car still in flight, another disproved car, a cancelled
    /// item or a packet of any other kind.
    #[test]
    fn a_remedy_is_a_landed_car_or_a_backlog_item() {
        let car = landed_car();
        let fix = json!({"id": "3f2a08ab-aaaa", "kind": "ship-a-change", "status": "closed",
                         "metadata": {"branch": "fix/the-scratch-floor-follows-the-build",
                                      "outcome": "merged", "merged": "true",
                                      "merge_ref": "a1b2c3d4e5f6"}});
        let said = judge_remedy(&car, &fix).unwrap();
        assert_eq!(
            said,
            "landed car 3f2a08ab (fix/the-scratch-floor-follows-the-build, merged as a1b2c3d4e5f6)"
        );

        let item = json!({"id": "08664157-bbbb", "kind": "backlog-item", "status": "open",
                          "title": "The eviction order needs a real fix"});
        assert_eq!(
            judge_remedy(&car, &item).unwrap(),
            "backlog item 08664157 (open): The eviction order needs a real fix"
        );

        assert!(
            judge_remedy(&car, &car)
                .unwrap_err()
                .contains("its own remedy")
        );
        let mut parked = fix.clone();
        parked["status"] = json!("open");
        parked["metadata"] = json!({"branch": "fix/x"});
        assert!(
            judge_remedy(&car, &parked)
                .unwrap_err()
                .contains("has not landed")
        );
        let mut disproved = fix.clone();
        disproved["metadata"]["outcome"] = json!("disproved");
        assert!(
            judge_remedy(&car, &disproved)
                .unwrap_err()
                .contains("itself disproved")
        );
        let mut cancelled = item.clone();
        cancelled["status"] = json!("cancelled");
        assert!(judge_remedy(&car, &cancelled).is_err());
        let gate = json!({"id": "99999999-cccc", "kind": "gate-run", "status": "closed"});
        assert!(
            judge_remedy(&car, &gate)
                .unwrap_err()
                .contains("gate-run packet")
        );
    }
}
