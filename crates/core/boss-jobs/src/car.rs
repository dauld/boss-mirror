//! Shared builders for a ship-a-change "car": the packet body and the
//! per-step evidence, plus the `Receipt` type they carry.
//!
//! WHY THIS IS IN CORE. Filing a car — POST the ship-a-change packet,
//! then fill its scope/build/gate steps with the receipt copied
//! verbatim — is done two ways now: by `boss park` (a human parks after
//! a green gate) and by the dispatcher's auto-park handler (a rule parks
//! on the gate-run's green step). If each built the packet its own way
//! they would drift, and the one field that must never be rebuilt is the
//! receipt (that is the bug `boss park` was created to kill). So the
//! pure builders live here, shared by both callers; the HTTP
//! orchestration (the POST and the PUTs) stays with each caller, because
//! that part legitimately differs. CLAUDE.md 9a: collapse the fact that
//! would otherwise live twice.

use serde_json::{Value, json};

/// What a green gate-run packet says about a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The receipt string, copied verbatim — never rebuilt from parts.
    pub raw: String,
    /// The head it vouches for, read back out for refusal messages and
    /// for the caller to compare against the branch.
    pub head: String,
    /// `full`, `--auto`, or whatever the runner was given.
    pub mode: String,
}

/// The instant stamp a car's gate step and the conductor's `review`
/// step share. RFC3339 to the SECOND, `Z`: dock queue time is
/// `review.completed_at` minus `gate.completed_at`, so sub-second
/// precision or an offset would break that subtraction by writer. ONE
/// definition — `boss gate`'s `stamp` delegates here — so the two ends
/// of the measurement cannot drift in format.
pub fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The ship-a-change packet body for a car.
pub fn car_body(
    branch: &str,
    summary: &str,
    backlog_item: Option<&str>,
    delivery_channel: Option<&str>,
) -> Value {
    let mut metadata = json!({ "branch": branch, "summary": summary });
    if let Some(item) = backlog_item {
        // A declared job edge — ref-checked by the API at the write,
        // which is what makes it safe to write here rather than by hand.
        // A mistyped id is refused instead of silently pointing at
        // nothing.
        metadata["backlog_item"] = json!(item);
    }
    // How this change ships (data/config/software/infra), derived at the
    // gate and carried here so the yard and channel-gated delivery read
    // it off the car. Absent when the gate could not classify the diff.
    if let Some(dc) = delivery_channel {
        metadata["delivery_channel"] = json!(dc);
    }
    json!({
        "kind": "ship-a-change",
        "title": summary_title(summary),
        "subject": {"subject_kind": "custom", "id": branch},
        "owner_id": "emp-david",
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// A car's title: the first sentence of its summary, trimmed.
///
/// Titles are what David reads on a board, so they get the summary's
/// opening claim rather than the branch name — `fix/a-dropped-lookup`
/// says less than "A dropped lookup does not red a train".
fn summary_title(summary: &str) -> String {
    let first = summary
        .split_terminator(['.', '\n'])
        .next()
        .unwrap_or(summary)
        .trim();
    let t = if first.is_empty() {
        summary.trim()
    } else {
        first
    };
    t.chars().take(120).collect()
}

/// The step titles a parked car fills, in the order the workflow runs
/// them. `ship-a-change` names them in its registry row, so a rename
/// there is a rename here — a caller refuses rather than guesses when a
/// step is missing.
pub const SCOPE: &str = "Declare the boundary";
pub const BUILD: &str = "Build it";
pub const GATE: &str = "Green, and observed working";

/// The evidence each of the three steps carries, and when it was filled.
///
/// All three stamps are the same instant on purpose: filing a car is one
/// act, so scope, build and gate genuinely complete together. What the
/// stamp separates is the *dock* — gate's stamp is when the car became
/// ready, and the conductor's stamp on `review` is when it boarded, so
/// the difference is queue time and nothing else.
pub fn step_fields(
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    receipt: &Receipt,
    now: chrono::DateTime<chrono::Utc>,
) -> [(&'static str, Value); 3] {
    let at = stamp(now);
    [
        (
            SCOPE,
            json!({"summary": summary, "excludes": excludes, "completed_at": at}),
        ),
        (BUILD, json!({"test": test, "completed_at": at})),
        (
            GATE,
            json!({
                "gates": if receipt.mode.is_empty() { "full" } else { &receipt.mode },
                // VERBATIM. The whole point of the shared builder.
                "receipt": receipt.raw,
                "verified": verified,
                "completed_at": at,
            }),
        ),
    ]
}

/// The review step a parked car waits at. The conductor reads it as
/// the dock: a car whose review is `ready`/`active` is parked, and
/// boarding stamps `metadata.train` rather than completing it (so a
/// cancelled train can release the car by clearing the stamp).
pub const REVIEW: &str = "Open for review";

/// A step by its registry slug, falling back to its title. The same
/// lookup the conductor uses; one definition (CLAUDE.md 9a).
pub fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    let steps = job.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
        })
}

/// Is this car still PARKED — gated, filed, and waiting at the dock —
/// rather than boarded (a train stamped it) or past review?
///
/// ONE PREDICATE, THREE READERS. The conductor counts the dock with it
/// (`parked_ready` adds boarding's own refinements: a `hold`, a
/// `train/` branch). `boss park` and the dispatcher's auto-park handler
/// ask it the question this file exists for: when a branch is gated
/// AGAIN, is there a car to refresh, or is a new one due? Measured
/// 2026-09-05 (backlog d052afad): every re-gate filed a twin, the dock
/// held 10 cars for 6 branches, and the first car of each pair sat
/// there with a receipt for a head that no longer existed — left behind
/// by every train as "gated, then changed" until closed by hand.
///
/// Status is deliberately not read here: callers list `status=open`,
/// and a fixture without a top-level status must still answer.
pub fn is_parked(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() || is_set(md.and_then(|m| m.get("train"))) {
        return false;
    }
    matches!(
        find_step(car, "review", REVIEW)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str),
        Some("ready" | "active")
    )
}

/// The car a re-gate of `branch` refreshes, if one is still parked.
/// `None` means file a new car — the branch has none, or its car has
/// boarded or reached a terminal and that history is spent.
pub fn parked_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_parked(c)
    })
}

/// A car that already carried its branch to main: closed with
/// `outcome=merged`, or stamped `merged` by the conductor's landing (the
/// marker v3 ship-a-change gates its `merged` step on — the dispatcher
/// closes the Job from it, so a car can be marked before it is closed).
/// An abandoned or cancelled car is spent, not landed.
pub fn is_landed(car: &Value) -> bool {
    let md = car.get("metadata");
    let closed_merged = car.get("status").and_then(Value::as_str) == Some("closed")
        && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
    // The conductor writes the marker as the STRING "true"; a bool is
    // accepted for the same meaning, and an explicit false is neither.
    let marked = match md.and_then(|m| m.get("merged")) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    };
    closed_merged || marked
}

/// The car that already landed `branch`, if one has — the question both
/// `boss gate` and the auto-park handler ask BEFORE filing. Measured
/// 2026-09-08 (backlog 610537b2): a re-gate of a landed branch reused a
/// gate-run that still carried park intent, and on green the handler
/// filed a twin car for content already on main; it boarded, its train
/// went red on an empty diff, and train and twin were cleaned up by
/// hand. One test of "landed" here so the CLI and the dispatcher cannot
/// disagree about it.
pub fn landed_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_landed(c)
    })
}

/// The metadata patch that supersedes a parked car's gate receipt.
///
/// A completed step is frozen, so a fresh receipt rides the JOB as
/// `regate_receipt` — the key boarding (`receipt_skip_reason`) and
/// `boss receipt` both read in preference to the gate step. VERBATIM,
/// like the gate step's copy: a rebuilt receipt once let a wrong head
/// through. `skip_reason` is present-and-null on purpose — the metadata
/// door deletes a null key, so the conductor's "left behind" reason
/// goes with the stale receipt. One builder for `boss park`, the
/// auto-park handler and `boss rerail`, so the write cannot drift.
pub fn regate_patch(receipt: &Receipt, note: &str, delivery_channel: Option<&str>) -> Value {
    let mut patch = json!({
        "regate_receipt": receipt.raw,
        "skip_reason": Value::Null,
        "regate_note": note,
    });
    // Re-classify the channel from the re-gated diff and carry it, the
    // same field `car_body` stamps on a fresh car. Without this a
    // re-gate — the common path: every `--wait --park` of an existing
    // car, plus `boss park`/`boss rerail` — dropped `delivery_channel`,
    // so the yard and the channel mixes under-counted every branch that
    // was ever rebased or rerailed. Omitted (not nulled) when the diff
    // could not be classified: a null key is DELETED by the metadata
    // door, which would strip a channel the car already carried.
    if let Some(dc) = delivery_channel {
        patch["delivery_channel"] = json!(dc);
    }
    patch
}

/// A metadata stamp that is present: the conductor writes `train` as
/// an id and clears it to `null` on release; an empty string reads as
/// cleared too.
fn is_set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    #[test]
    fn the_car_body_carries_the_fields_the_api_demands() {
        let b = car_body("feat/x", "A thing does the thing. And more.", None, None);
        assert!(
            b["metadata"].get("delivery_channel").is_none(),
            "no delivery_channel when None"
        );
        let d = car_body("feat/x", "A thing.", None, Some("data"));
        assert_eq!(
            d["metadata"]["delivery_channel"], "data",
            "the car carries the delivery channel the gate stamped"
        );
        for f in [
            "kind", "subject", "title", "owner_id", "status", "priority", "metadata", "tags",
        ] {
            assert!(b.get(f).is_some(), "car body is missing `{f}`");
        }
        assert_eq!(b["title"], "A thing does the thing");
        assert_eq!(b["subject"]["id"], "feat/x");
        assert!(b["metadata"].get("backlog_item").is_none());
    }

    #[test]
    fn a_backlog_edge_is_carried_when_given() {
        let b = car_body(
            "feat/x",
            "Summary",
            Some("de6f0c06-a341-4445-9f47-399dc27a60fb"),
            None,
        );
        assert_eq!(
            b["metadata"]["backlog_item"],
            "de6f0c06-a341-4445-9f47-399dc27a60fb"
        );
    }

    #[test]
    fn every_step_park_fills_says_when_it_was_filled() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let fields = step_fields("s", "e", "t", "v", &receipt, at("2026-08-29T04:15:09Z"));

        assert_eq!(fields.len(), 3);
        for (title, md) in &fields {
            assert_eq!(
                md.get("completed_at").and_then(Value::as_str),
                Some("2026-08-29T04:15:09Z"),
                "{title} was filled without saying when"
            );
        }
    }

    #[test]
    fn the_stamp_matches_the_one_the_conductor_writes() {
        // Dock queue time is `review.completed_at` (the conductor's)
        // minus `gate.completed_at` (this), so a format that differs by
        // writer is wrong only in the subtraction.
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let now = at("2026-08-29T04:15:09.847213Z");
        let fields = step_fields("s", "e", "t", "v", &receipt, now);
        let parked = fields[2].1["completed_at"].as_str().unwrap().to_string();

        assert_eq!(parked, stamp(now));
        assert!(
            !parked.contains('.') && parked.ends_with('Z'),
            "sub-second precision and offsets both break string comparison \
             against the conductor's stamps: {parked}"
        );
    }

    #[test]
    fn parking_does_not_disturb_the_evidence_it_already_carried() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: String::new(),
        };
        let f = step_fields(
            "sum",
            "exc",
            "tst",
            "ver",
            &receipt,
            at("2026-08-29T04:15:09Z"),
        );
        assert_eq!(f[0].1["summary"], json!("sum"));
        assert_eq!(f[0].1["excludes"], json!("exc"));
        assert_eq!(f[1].1["test"], json!("tst"));
        assert_eq!(f[2].1["verified"], json!("ver"));
        // An empty mode still reads as a full gate.
        assert_eq!(f[2].1["gates"], json!("full"));
        assert_eq!(f[2].1["receipt"], json!(GREEN));
    }
}

#[cfg(test)]
mod regate_tests {
    use super::*;

    const GREEN: &str = r#"{"verdict": "green", "head": "0123456789abcdef0123456789abcdef01234567", "mode": "full", "fails": []}"#;

    fn receipt() -> Receipt {
        Receipt {
            raw: GREEN.to_string(),
            head: "0123456789abcdef0123456789abcdef01234567".to_string(),
            mode: "full".to_string(),
        }
    }

    /// A car as the jobs API lists it: open, branch in metadata, review
    /// step waiting — the shape both parkers and the conductor read.
    fn car(id: &str, branch: &str, review_status: &str, train: Value) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": branch, "train": train },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": review_status},
            ]
        })
    }

    #[test]
    fn a_car_waiting_at_review_with_no_train_is_parked() {
        assert!(is_parked(&car("c1", "fix/x", "ready", Value::Null)));
        assert!(is_parked(&car("c1", "fix/x", "active", Value::Null)));
    }

    #[test]
    fn a_boarded_car_is_not_parked() {
        // The conductor stamps `metadata.train` when a car boards and
        // clears it (Null) when a cancelled train releases the car.
        assert!(!is_parked(&car("c1", "fix/x", "ready", json!("train-1"))));
        assert!(is_parked(&car("c1", "fix/x", "ready", json!(""))));
    }

    #[test]
    fn a_car_past_review_is_not_parked() {
        assert!(!is_parked(&car("c1", "fix/x", "completed", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "skipped", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "pending", Value::Null)));
    }

    #[test]
    fn a_car_naming_no_branch_is_not_parked() {
        assert!(!is_parked(&car("c1", "", "ready", Value::Null)));
    }

    #[test]
    fn the_parked_car_for_a_branch_is_the_one_still_at_the_dock() {
        let cars = vec![
            car("boarded", "fix/x", "ready", json!("train-9")),
            car("other", "fix/y", "ready", Value::Null),
            car("parked", "fix/x", "ready", Value::Null),
        ];
        let got = parked_car_for(&cars, "fix/x").expect("one car is parked");
        assert_eq!(got["id"], "parked");
        assert!(parked_car_for(&cars, "fix/z").is_none());
    }

    #[test]
    fn the_regate_patch_copies_the_receipt_verbatim_and_clears_the_skip() {
        let p = regate_patch(&receipt(), "why", None);
        // VERBATIM: the receipt string, not a rebuilt object.
        assert_eq!(p["regate_receipt"], json!(GREEN));
        // Present-and-null: the metadata door DELETES a null key, which
        // is how the conductor's "left behind" reason goes away.
        assert!(p.get("skip_reason").is_some_and(Value::is_null));
        assert_eq!(p["regate_note"], json!("why"));
    }

    #[test]
    fn the_regate_patch_carries_the_delivery_channel_when_known() {
        // A re-gate re-classifies and stamps the channel, so a rebased
        // or rerailed car is counted in the yard's mixes like a fresh one.
        let p = regate_patch(&receipt(), "why", Some("config"));
        assert_eq!(p["delivery_channel"], "config");
        // Unknown diff: OMITTED, never nulled — a null key is deleted by
        // the metadata door, which would strip a channel already on the car.
        let q = regate_patch(&receipt(), "why", None);
        assert!(
            q.get("delivery_channel").is_none(),
            "no delivery_channel key when the diff could not be classified"
        );
    }
}

#[cfg(test)]
mod landed_tests {
    use super::*;

    const BRANCH: &str = "fix/boot-never-refuses-over-an-unviable-workflow";

    /// The car that carried the branch on train #259, as the SoR holds it.
    fn landed() -> Value {
        json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": {
                "branch": BRANCH, "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331",
                "boarded_head": "a56b4a9"
            }
        })
    }

    /// The twin auto-park filed for the same branch, abandoned by hand.
    fn abandoned_twin() -> Value {
        json!({
            "id": "dfb98d07-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": { "branch": BRANCH, "outcome": "abandoned", "abandoned": "true" }
        })
    }

    #[test]
    fn a_closed_merged_car_is_the_landing() {
        let cars = vec![abandoned_twin(), landed()];
        let got = landed_car_for(&cars, BRANCH).expect("the merged car answers");
        assert_eq!(got["metadata"]["merge_ref"], "b641f3adcf47");
    }

    #[test]
    fn an_abandoned_car_is_spent_not_landed() {
        assert!(landed_car_for(&[abandoned_twin()], BRANCH).is_none());
    }

    /// The conductor stamps `merged` first and the dispatcher closes
    /// the Job after — the marker alone is a landing.
    #[test]
    fn the_merged_marker_counts_before_the_close() {
        let mut marked = landed();
        marked["status"] = json!("open");
        marked["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("outcome");
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!(true);
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!("false");
        assert!(!is_landed(&marked), "an explicit false is not a landing");
    }

    #[test]
    fn a_parked_car_is_not_a_landing_and_another_branch_does_not_answer() {
        let parked = json!({
            "id": "p", "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [{"spec_slug": "review", "title": REVIEW, "status": "ready"}]
        });
        assert!(landed_car_for(&[parked], BRANCH).is_none());
        assert!(landed_car_for(&[landed()], "feat/other").is_none());
    }
}
