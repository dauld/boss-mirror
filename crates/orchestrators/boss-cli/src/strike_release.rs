//! `boss release <car> --diagnosis-file <PATH>` — the look that clears a
//! car the conductor holds after red trains.
//!
//! WHY (backlog c96aac11, 2026-09-25). Car 6a647c88 was struck by the
//! 07:48 and 08:18 trains, both red only on a test of ANOTHER car
//! (dcdc6c64 — a combined-tree stub gap, diagnosed, that car held and
//! reworked). `train::car_hold_reason` holds a car at `red_trains >=
//! max_red_trains` "until someone looks", and nothing recorded the look:
//! `boss release` answered "carries no hold" (it reads only the review
//! step's marker), `boss rerail` is for conflict skips, and the one path
//! left was a hand metadata write of `red_trains` — a belief about the
//! past with nothing in the record behind it, which the drain rightly
//! will not do.
//!
//! THE LOOK IS THE ARTIFACT. A strike clears only against a written
//! diagnosis that names, by full id, at least one RED train gate-run
//! the car actually rode — verified here, not trusted: the id reads
//! back as a `gate-run` packet, it is a train's gate (`train_gate::
//! packet_marks` stamps `train`), its verdict is `failed`
//! (`train_gate::standing`, the same reader the conductor judges by),
//! and that train's `boarded_jobs` names this car. `boarded_jobs` is the
//! right list for "rode": a release clears the CAR's `train` marker but
//! leaves the train's list naming it forever (`releasable_cars`), which
//! is exactly the history this question needs.
//!
//! WHAT IT WRITES — one call to the job's merge door (`PATCH
//! /api/jobs/{id}/metadata`, where null deletes): `red_trains` deleted,
//! the strike's own `skip_reason` deleted (only when it is the hold's
//! text — any other skip note is some other rule's), and one entry
//! appended to `strike_releases`: who looked, when, the count cleared,
//! the gate-runs verified and the diagnosis verbatim. Append-only: an
//! earlier release stays on the car. Then the car is read back, because
//! a 204 is a claim.
//!
//! ONLY A TRAIN GATE COUNTS. A train can go red on forge CI alone with
//! its gate green; such a strike has no red gate-run to name and this
//! verb refuses it. That is deliberate for now — the gate's receipt is
//! the artifact the diagnosis can be checked against.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::steps::Wire;

/// The append-only list of looks on a car.
pub(crate) const KEY_RELEASES: &str = "strike_releases";
/// The strike count `release_stamps` increments and `car_hold_reason` reads.
pub(crate) const KEY_REDS: &str = "red_trains";

/// The strikes a car carries.
pub(crate) fn red_trains(car: &Value) -> i64 {
    car.pointer(&format!("/metadata/{KEY_REDS}"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Every full uuid the diagnosis names, in order, once each. Full ids
/// only: a short prefix would have to be resolved among every gate-run
/// ever filed, and the record should carry the id that was checked, not
/// the eight characters that were typed.
pub(crate) fn named_ids(diagnosis: &str) -> Vec<String> {
    let b = diagnosis.as_bytes();
    let hexish = |c: u8| c.is_ascii_hexdigit() || c == b'-';
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 36 <= b.len() {
        let bounded = (i == 0 || !hexish(b[i - 1])) && (i + 36 == b.len() || !hexish(b[i + 36]));
        if bounded && uuid_at(&b[i..i + 36]) {
            let id = String::from_utf8_lossy(&b[i..i + 36]).to_ascii_lowercase();
            if !out.contains(&id) {
                out.push(id);
            }
            i += 36;
        } else {
            i += 1;
        }
    }
    out
}

fn uuid_at(w: &[u8]) -> bool {
    w.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// The train a gate-run gates, as `train_gate::packet_marks` stamped it.
pub(crate) fn train_of(gate_run: &Value) -> Option<&str> {
    gate_run
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
}

/// Does this gate-run count as a strike the car earned? `Ok` when it is
/// a red train gate whose train carried `car_id`; otherwise the reason
/// it does not, in words the refusal prints beside the id. `train` is
/// the train [`train_of`] names, read by the caller (`None` when it
/// could not be read).
pub(crate) fn judge(gate_run: &Value, train: Option<&Value>, car_id: &str) -> Result<(), String> {
    let kind = gate_run.get("kind").and_then(Value::as_str).unwrap_or("?");
    if kind != "gate-run" {
        return Err(format!("is a {kind}, not a gate-run"));
    }
    let Some(tid) = train_of(gate_run) else {
        return Err(
            "gates no train (a car's own gate is not a strike — only a train's red is)".into(),
        );
    };
    match crate::train_gate::standing(gate_run) {
        crate::train_gate::Standing::Failed => {}
        other => {
            return Err(format!(
                "is not red — its verdict reads {other:?}, and a strike is a train that went red"
            ));
        }
    }
    let Some(train) = train else {
        return Err(format!("names train {tid}, which could not be read"));
    };
    let rode = train
        .pointer("/metadata/boarded_jobs")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|i| i.as_str() == Some(car_id)));
    if !rode {
        return Err(format!(
            "gates train {}, which did not carry this car (its boarded_jobs does not name it)",
            crate::train::id8(tid)
        ));
    }
    Ok(())
}

/// One look, as the car keeps it.
pub(crate) fn release_entry(
    by: &str,
    at: &str,
    reds: i64,
    gate_runs: &[String],
    diagnosis: &str,
) -> Value {
    json!({
        "by": by,
        "at": at,
        "red_trains_cleared": reds,
        "gate_runs": gate_runs,
        "diagnosis": diagnosis,
    })
}

/// The merge-door body: the strikes deleted, the hold's own skip note
/// deleted (and no other), the look appended to what the car already
/// holds.
pub(crate) fn release_patch(car: &Value, entry: Value) -> Value {
    let mut releases = car
        .pointer(&format!("/metadata/{KEY_RELEASES}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    releases.push(entry);
    let mut patch = json!({ KEY_REDS: Value::Null, KEY_RELEASES: releases });
    // The hold's text is `car_hold_reason`'s, stamped by the conductor
    // with the count the car carries now; any other skip note is some
    // other rule's to clear (boarding clears them all).
    let hold_text = crate::train::car_hold_reason(car, 1);
    let skip = car.pointer("/metadata/skip_reason").and_then(Value::as_str);
    if hold_text.is_some() && skip == hold_text.as_deref() {
        patch["skip_reason"] = Value::Null;
    }
    patch
}

/// The verb. Every refusal happens before the one write.
pub(crate) async fn release_struck(wire: &Wire, given: &str, diagnosis: &str) -> Result<()> {
    let car = wire.car(given).await?;
    let car_id = crate::envelope::job_id(&car)
        .context("the car has no id")?
        .to_string();
    let label = format!(
        "{} {} \"{}\"",
        crate::train::id8(&car_id),
        car.pointer("/metadata/branch")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        crate::envelope::job_title(&car).unwrap_or("?")
    );
    let reds = red_trains(&car);
    if reds <= 0 {
        bail!(
            "boss release: {label} carries no red-train strikes (`{KEY_REDS}` is unset) — \
             nothing for a diagnosis to clear"
        );
    }
    let ids = named_ids(diagnosis);
    if ids.is_empty() {
        bail!(
            "boss release: the diagnosis names no gate-run — it must name, by FULL id, at least \
             one red train gate-run {label} rode (a train's gate-run id is its \
             `train_gate_run` metadata). Nothing was written."
        );
    }
    let mut verified = Vec::new();
    let mut refused = Vec::new();
    for id in &ids {
        let verdict = match wire.packet(id).await {
            Err(e) => Err(format!("could not be read ({e})")),
            Ok(run) => {
                let train = match train_of(&run) {
                    Some(t) if run.get("kind").and_then(Value::as_str) == Some("gate-run") => {
                        wire.packet(t).await.ok()
                    }
                    _ => None,
                };
                judge(&run, train.as_ref(), &car_id)
            }
        };
        match verdict {
            Ok(()) => verified.push(id.clone()),
            Err(why) => refused.push(format!("  {id} {why}")),
        }
    }
    if verified.is_empty() {
        bail!(
            "boss release: the diagnosis names no red train gate-run {label} rode — nothing was \
             written. What each id it names is:\n{}",
            refused.join("\n")
        );
    }
    let by = wire
        .caller_id()
        .ok_or_else(|| anyhow!("no actor named — the look records who looked"))?;
    let entry = release_entry(
        by,
        &boss_clock_client::wall_now().to_rfc3339(),
        reds,
        &verified,
        diagnosis,
    );
    let patch = release_patch(&car, entry);
    wire.patch_job_metadata(&car_id, patch.clone()).await?;

    let after = wire.packet(&car_id).await?;
    let now = after.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let (report, all_took) = crate::job::confirm_patch(&now, &patch);
    if !all_took {
        bail!("the API answered the write but the car does not hold what was sent\n{report}");
    }
    println!(
        "boss release: {label} — {reds} red-train strike(s) cleared on a diagnosis naming {}\n  \
         the look is recorded under `{KEY_RELEASES}`; it boards at the next tick",
        verified
            .iter()
            .map(|g| format!("gate-run {}", crate::train::id8(g)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !refused.is_empty() {
        println!(
            "  ids the diagnosis names that are not such a gate-run (recorded as prose only):\n{}",
            refused.join("\n")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAR: &str = "c6bd173e-3dc9-426f-8fff-866a3b2a6117";
    const TRAIN: &str = "7a1b2c3d-0000-4000-8000-000000000001";
    const RUN: &str = "9f8e7d6c-0000-4000-8000-000000000002";

    fn gate_run(verdict: &str, train: Option<&str>) -> Value {
        let mut md = json!({ "train_gate": true });
        if let Some(t) = train {
            md["train"] = json!(t);
        }
        json!({
            "id": RUN, "kind": "gate-run", "status": "closed", "metadata": md,
            "steps": [{ "id": "s-v", "spec_slug": "record-verdict", "status": "completed",
                        "metadata": { "verdict": verdict } }],
        })
    }

    fn train(boarded: &[&str]) -> Value {
        json!({ "id": TRAIN, "kind": "pr-train", "metadata": { "boarded_jobs": boarded } })
    }

    #[test]
    fn named_ids_reads_every_full_uuid_once_in_order_and_nothing_shorter() {
        let text = format!(
            "Both reds were car dcdc6c64's stub gap. Gate-runs {RUN} and ({TRAIN}); \
             again {}. Not an id: {}x, nor dcdc6c64-3886.",
            RUN.to_ascii_uppercase(),
            &RUN[..35]
        );
        assert_eq!(named_ids(&text), vec![RUN.to_string(), TRAIN.to_string()]);
        assert!(named_ids("no ids here, only dcdc6c64").is_empty());
        // Glued to more hex is not an id — it is part of something longer.
        assert!(named_ids(&format!("{RUN}0")).is_empty());
        // Multibyte prose around an id does not trip the scan.
        assert_eq!(named_ids(&format!("— {RUN} —")), vec![RUN.to_string()]);
    }

    #[test]
    fn only_a_red_train_gate_that_carried_the_car_counts() {
        assert_eq!(
            judge(&gate_run("failed", Some(TRAIN)), Some(&train(&[CAR])), CAR),
            Ok(())
        );
        let green = judge(&gate_run("green", Some(TRAIN)), Some(&train(&[CAR])), CAR);
        assert!(green.unwrap_err().contains("not red"));
        let lost = judge(&gate_run("lost", Some(TRAIN)), Some(&train(&[CAR])), CAR);
        assert!(lost.unwrap_err().contains("not red"));
        let own_gate = judge(&gate_run("failed", None), None, CAR);
        assert!(own_gate.unwrap_err().contains("gates no train"));
        let elsewhere = judge(
            &gate_run("failed", Some(TRAIN)),
            Some(&train(&["another-car"])),
            CAR,
        );
        assert!(elsewhere.unwrap_err().contains("did not carry this car"));
        let unread = judge(&gate_run("failed", Some(TRAIN)), None, CAR);
        assert!(unread.unwrap_err().contains("could not be read"));
        let a_car = json!({ "id": CAR, "kind": "ship-a-change", "metadata": {} });
        assert!(
            judge(&a_car, None, CAR)
                .unwrap_err()
                .contains("is a ship-a-change, not a gate-run")
        );
    }

    #[test]
    fn the_patch_deletes_the_strikes_and_the_holds_note_and_appends_the_look() {
        let held = json!({ "id": CAR, "metadata": {
            "red_trains": 2,
            "skip_reason": "held after 2 red trains — needs a look before it boards again",
            "strike_releases": [{ "by": "earlier" }],
        }});
        let patch = release_patch(&held, json!({ "by": "now" }));
        assert_eq!(
            patch,
            json!({
                "red_trains": null,
                "skip_reason": null,
                "strike_releases": [{ "by": "earlier" }, { "by": "now" }],
            })
        );
        // Another rule's skip note is not this verb's to clear.
        let other = json!({ "id": CAR, "metadata": {
            "red_trains": 1,
            "skip_reason": "returned to dock: train cancelled (CI red)",
        }});
        let patch = release_patch(&other, json!({ "by": "now" }));
        assert!(patch.get("skip_reason").is_none(), "{patch}");
        assert_eq!(patch["strike_releases"], json!([{ "by": "now" }]));
        assert_eq!(red_trains(&held), 2);
        assert_eq!(red_trains(&json!({ "metadata": {} })), 0);
    }

    #[test]
    fn the_entry_names_who_when_what_was_cleared_and_why() {
        let e = release_entry(
            "claude@algedonic.dev",
            "2026-09-25T09:00:00+00:00",
            2,
            &[RUN.to_string()],
            "the diagnosis",
        );
        assert_eq!(e["by"], json!("claude@algedonic.dev"));
        assert_eq!(e["at"], json!("2026-09-25T09:00:00+00:00"));
        assert_eq!(e["red_trains_cleared"], json!(2));
        assert_eq!(e["gate_runs"], json!([RUN]));
        assert_eq!(e["diagnosis"], json!("the diagnosis"));
    }
}
