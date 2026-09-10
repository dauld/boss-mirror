//! `boss park <branch>` — parking a car is one verb, not eight steps.
//!
//! WHY THIS EXISTS (filed de6f0c06). Parking a car by hand is: POST a
//! ship-a-change packet, GET it back to learn the step ids, then PUT
//! `scope`, `build` and `gate` with hand-assembled metadata — including
//! the receipt, retyped. It was done eight times on 2026-08-26 and six
//! more on 2026-08-27/28: fourteen across three days, which is more
//! often than the sequence `boss gate` replaced.
//!
//! THE RECEIPT IS THE POINT, NOT THE TEDIUM. The `gate` step's metadata
//! IS the receipt — verdict, head, mode, fails — and the standing rule
//! is that no car rides a train without one. Retyping it per car is
//! exactly where a wrong head gets in, and it has, twice: once a packet
//! recorded a symbolic `origin/<branch>` because an unauthenticated
//! ls-remote returned nothing, and once a car carried a fabricated
//! 40-character sha that had to be replaced with the real one. Neither
//! is possible if the receipt is COPIED from the gate-run packet by a
//! machine rather than transcribed by a person.
//!
//! So the verb's job is narrow and entirely mechanical: find the
//! gate-run packet for the branch, REFUSE unless it is green, copy its
//! receipt verbatim, and file the car. The prose — what the change is,
//! what it excludes, what was tested, what was observed — stays human
//! input, because that is judgement and not transcription.
//!
//! THE REFUSAL IS THE FEATURE. "No receipt, no ride" stops being a
//! discipline someone remembers and becomes something the verb enforces.

use anyhow::{Result, bail};
use boss_jobs::car::{
    Receipt, building_car_for, car_body, finish_writes, parked_car_for, regate_patch,
    triage_on_park,
};
// `json!` is no longer needed out here: the step bodies a park writes are
// built by `car::finish_writes` in core. The test module still builds
// fixtures with it and imports it itself.
use serde_json::Value;

/// Find the receipt for `branch` among gate-run packets, or refuse.
///
/// Refuses on every path that would otherwise put an unvouched car on a
/// train: no packet at all, a packet still running, and — the one that
/// matters — a packet whose verdict is `failed` or `lost`. A `lost` run
/// is refused as loudly as a failed one: it means the environment died
/// before saying anything, so there is no evidence either way, and
/// "we don't know" must not read as "fine".
pub(crate) fn receipt_for(packets: &[Value], branch: &str, head_now: &str) -> Result<Receipt> {
    // Every reported gate for this branch, in the order the API gave them.
    let mut reported: Vec<(&str, &str)> = Vec::new(); // (verdict, receipt)
    let mut seen_branch = false;
    for p in packets {
        let b = p
            .get("metadata")
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if b != branch {
            continue;
        }
        seen_branch = true;
        for s in p
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let sm = s.get("metadata");
            let verdict = sm.and_then(|m| m.get("verdict")).and_then(Value::as_str);
            let raw = sm.and_then(|m| m.get("receipt")).and_then(Value::as_str);
            if let (Some(v), Some(r)) = (verdict, raw) {
                reported.push((v, r));
            }
        }
    }

    if !seen_branch {
        bail!(
            "no gate-run packet for `{branch}`. A car does not ride a train without a \
             receipt — gate it first (`boss gate {branch}`), then park it."
        );
    }
    if reported.is_empty() {
        bail!(
            "the gate for `{branch}` has not reported yet. Wait for a verdict \
             (`boss gate {branch} --wait`) rather than parking a car whose gate is \
             still running."
        );
    }

    let head_of = |raw: &str| -> String {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| v.get("head").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    };

    // SELECT BY HEAD, NOT BY POSITION. A branch gated more than once has
    // several packets, and which one the API lists first is not a
    // contract — depending on it parked a car on a two-rebases-old
    // receipt on 2026-08-28. The only packet that matters is the one
    // vouching for the commit that would actually ride the train.
    // ...and among packets vouching for the SAME head, prefer a green
    // one: a dead `lost` packet (a refused launch, a killed runner) must
    // not shadow the real verdict that followed it — on 2026-08-30 that
    // shadow forced editing a closed packet's metadata to unblock a true
    // green (ed7f1355). When no green exists the first match stands, so
    // the honest lost/failed refusal below still names what happened.
    let known_head = head_now.len() >= 40;
    let chosen = if known_head {
        reported
            .iter()
            .find(|(v, raw)| *v == "green" && head_of(raw) == head_now)
            .or_else(|| reported.iter().find(|(_, raw)| head_of(raw) == head_now))
    } else {
        reported.last()
    };

    let Some((verdict, raw)) = chosen else {
        let heads: Vec<String> = reported
            .iter()
            .map(|(v, r)| format!("{}@{}", v, &head_of(r)[..12.min(head_of(r).len())]))
            .collect();
        bail!(
            "no gate for `{branch}` vouches for its current head {} — refusing to park it.\n  \
             gates on record: {}\n  \
             A receipt vouches for ONE head. This is what a rebase or a new commit after \
             gating looks like; re-gate (`boss gate {branch}`) so the car carries a receipt \
             for the commit it would actually take onto a train.",
            &head_now[..12.min(head_now.len())],
            heads.join(", ")
        );
    };

    if *verdict != "green" {
        bail!(
            "the gate for `{branch}` is `{verdict}`, not green — refusing to park it.\n  \
             receipt: {raw}\n  \
             A `lost` verdict is refused for the same reason as a failed one: the run \
             said nothing, so there is no evidence, and no evidence is not a pass."
        );
    }

    let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let head = parsed
        .get("head")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mode = parsed
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if head.len() < 40 {
        bail!(
            "the receipt for `{branch}` names no head (`{head}`) — refusing to park it.\n  \
             receipt: {raw}\n  \
             A receipt vouches for ONE head; without a sha it vouches for nothing. This \
             happens when sha resolution fell back to a symbolic ref, which means the \
             gate ran but nobody can say against what."
        );
    }

    Ok(Receipt {
        raw: raw.to_string(),
        head,
        mode,
    })
}

/// Resolve a job id that may have been given as a short prefix.
///
/// THE MISMATCH THAT MANUFACTURES TYPOS. Every surface in this system
/// SHOWS eight characters — the conductor's journal (`car 3d0498d3`),
/// the board, `id8()` in train.rs, every report anyone writes — and
/// then the API demands all thirty-six. So the id you can see is never
/// the id you can use, and the gap gets closed by retyping from memory.
/// It was closed wrongly four times on 2026-08-27/28, once while
/// testing this very verb: `--backlog-item 20dfcb03-a5f7-…` was
/// fabricated, and only the API's ref-check caught it.
///
/// Refusing a prefix that matches more than one Job is the point. A
/// silent pick between two candidates would be the same defect wearing
/// a helpful face — and eight hex characters over a few hundred Jobs
/// makes a collision unlikely enough to be worth naming loudly when it
/// happens, rather than guarding against by demanding thirty-six.
pub(crate) fn resolve_job_id(candidates: &[Value], given: &str) -> Result<String> {
    if given.len() >= 36 {
        return Ok(given.to_string());
    }
    let hits: Vec<&str> = candidates
        .iter()
        .filter_map(|j| j.get("id").and_then(Value::as_str))
        .filter(|id| id.starts_with(given))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => bail!(
            "no Job whose id starts with `{given}`. The id has to name a Job on this \
             instance — read it from the board or the API rather than from memory."
        ),
        many => bail!(
            "`{given}` matches {} Jobs: {}. Give more characters — picking one for you \
             is how the wrong packet gets linked.",
            many.len(),
            many.join(", ")
        ),
    }
}

// car_body, summary_title and the SCOPE/BUILD/GATE step titles now live
// in boss_jobs::car — shared with the dispatcher's auto-park handler so
// the two ways of filing a car cannot drift.

// The by-title step lookup this verb used to carry is gone: step ids
// now come off the packet inside `boss_jobs::car::finish_writes`, which
// looks a step up by its registry SLUG with the title as a fallback —
// the same `find_step` the conductor uses — and refuses rather than
// guessing when a step is absent. One lookup, one refusal (CLAUDE.md
// §9a).

/// BEST-EFFORT: state the linked item's ROUTE, because parking this car
/// is what decided it.
///
/// `--backlog-item <id>` says this car is that item's build, and the
/// item's `build` step only OPENS once its triage records
/// `disposition = build` — so a car parked against an un-triaged item
/// linked to a step nothing could advance, and the change shipped while
/// the item still read as undecided work (backlog ca76d8f9). The
/// DECISION is `boss_jobs::car::triage_on_park`, shared with the
/// dispatcher's auto-park handler so the two park paths cannot disagree,
/// and idempotent — an item a person already routed is left alone.
///
/// NOTHING HERE FAILS THE PARK. The car is filed by the time this runs;
/// a write that cannot land costs a printed line saying what to do by
/// hand, which is also the warning the builder wanted at park time.
pub(crate) async fn route_linked_item(
    http: &reqwest::Client,
    item_id: &str,
    car_id: &str,
    branch: &str,
    // The verb saying it, so the line reads as the verb the operator
    // actually ran. `boss car open` routes the item too — the item's
    // `build` step opens when the build STARTS, not when it succeeds —
    // and shares this write rather than copying it (CLAUDE.md §9a).
    verb: &str,
) {
    let id8 = &item_id[..8.min(item_id.len())];
    let item = match crate::gate::api(
        http,
        reqwest::Method::GET,
        &format!("/api/jobs/{item_id}"),
        None,
    )
    .await
    {
        Ok(Some(item)) => item,
        Ok(None) => return,
        Err(e) => {
            println!(
                "boss {verb}: could not read backlog-item {id8} to route it ({e}) — \
                 the car is filed; triage the item to `build` by hand or its build step \
                 never opens"
            );
            return;
        }
    };
    let Some(write) = triage_on_park(&item, car_id, branch) else {
        return;
    };
    match crate::gate::api(
        http,
        reqwest::Method::PUT,
        &format!("/api/jobs/{item_id}/steps/{}", write.step_id),
        Some(write.body),
    )
    .await
    {
        Ok(_) => println!(
            "boss {verb}: backlog-item {id8} routed to `build` — this car IS its build, \
             so the arrival rule has a step to complete"
        ),
        Err(e) => println!(
            "boss {verb}: could not route backlog-item {id8} to `build` ({e}) — \
             the car is filed; triage it by hand or its build step never opens"
        ),
    }
}

/// File a car for `branch` and fill it up to `review`.
pub(crate) async fn run(
    branch: &str,
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    backlog_item: Option<String>,
    dry: bool,
    // The operator's now, taken once at the CLI entry point.
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();

    // The receipt first: refuse before anything is created, so a red
    // gate costs a line of output rather than a half-filled packet.
    let open = crate::gate::rows(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&limit=100",
            None,
        )
        .await?,
    );
    let head_now = crate::gate::resolve_sha(branch);
    let receipt = receipt_for(&open, branch, &head_now)?;
    println!(
        "boss park: {branch} is green at {} ({})",
        &receipt.head[..12.min(receipt.head.len())],
        if receipt.mode.is_empty() {
            "full"
        } else {
            &receipt.mode
        }
    );

    // Resolve a short backlog id BEFORE filing, so a bad reference costs
    // a line of output rather than a rejected POST half way through.
    let backlog_item = match backlog_item {
        None => None,
        Some(given) => {
            // BOTH KINDS A CAR CAN ANSWER, not just one. This searched
            // `kind=backlog-item` alone, so linking a car to a piece of
            // David's own feedback was refused with a message that reads
            // like the id is wrong — `boss park --backlog-item 898761cb`
            // said "no Job whose id starts with 898761cb" while 898761cb
            // was a perfectly real user-feedback packet (filed 381a4872).
            // The refusal was right about what it searched and wrong
            // about what exists.
            //
            // The two lists are concatenated rather than queried
            // separately so `resolve_job_id` still sees ONE candidate
            // set: a prefix matching a packet of each kind is genuinely
            // ambiguous and must be reported as such, not resolved by
            // whichever query happened to run first.
            let mut all = Vec::new();
            for kind in ["backlog-item", "user-feedback"] {
                all.extend(crate::gate::rows(
                    crate::gate::api(
                        &http,
                        reqwest::Method::GET,
                        &format!("/api/jobs?kind={kind}&limit=200"),
                        None,
                    )
                    .await?,
                ));
            }
            let full = resolve_job_id(&all, &given)?;
            if full != given {
                println!("boss park: backlog-item {given} -> {full}");
            }
            Some(full)
        }
    };

    // A RE-GATE REFRESHES THE PARKED CAR; IT DOES NOT FILE A TWIN.
    // Measured 2026-09-05 (backlog d052afad): the dock held 10 cars for
    // 6 branches because every re-gate parked again, and each first car
    // then sat with a receipt for a head that no longer existed — left
    // behind by every train as "gated, then changed" until closed by
    // hand. Skipping the duplicate would be worse (the stale car could
    // never board); the fix is the rerail write: the fresh receipt rides
    // the job as `regate_receipt`, verbatim, and the skip is cleared. A
    // car that has BOARDED or closed is spent history — a new car then.
    //
    // READ EVERY OPEN CAR, NOT THE ONES FILED UNDER THIS BRANCH. This
    // read was narrowed by `subject_id={branch}`, and a car's SUBJECT is
    // only the branch it was FILED under: `boss rerail` repoints
    // `metadata.branch` and leaves the subject where it was. So on a
    // re-railed branch the query answered nothing, "no car" was read as
    // "no car exists", and this verb filed a TWIN — three of them on
    // 2026-09-08, one while a sibling car was already aboard a train
    // (backlog 6790175e). The auto-park handler had the identical defect
    // and was fixed the same way (car 235157a5): page all the open cars
    // and let the shared `boss_jobs::car` predicates decide on
    // `metadata.branch`. `gate::all_open_cars` is that read — ONE
    // definition, the one `rerail::find_car`, `boss receipt` and
    // `boss channels` already use — and it pages on `total`, so a car
    // past page one is found too (a-limit-is-not-a-filter: 832
    // ship-a-change packets exist as of 2026-09-09).
    let parked = crate::gate::all_open_cars(&http).await?;
    if let Some(car) = parked_car_for(&parked, branch) {
        let id = car
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("parked car for {branch} has no id"))?
            .to_string();
        let id8 = &id[..8.min(id.len())];
        if dry {
            println!(
                "boss park: DRY would refresh the receipt on parked car {id8} \
                 (regate_receipt), not file a second"
            );
            return Ok(());
        }
        let note = format!(
            "re-gated in place: green at {} — receipt machine-copied to regate_receipt \
             by boss park; the frozen gate step stays as the original head's record",
            &receipt.head[..12.min(receipt.head.len())]
        );
        // Re-classify the channel from the current diff so the refreshed
        // car carries it like a fresh park (None if the diff won't resolve).
        let dc = crate::channels::delivery_channel_for(branch);
        crate::gate::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(regate_patch(&receipt, &note, dc.as_deref())),
        )
        .await?;
        println!(
            "boss park: car {id8} was already parked for {branch} — receipt refreshed \
             (regate_receipt), no second car"
        );
        // A re-gate restates the route too: the item may still be
        // un-triaged from the first park, and a second pass over one
        // already routed writes nothing.
        if let Some(item) = backlog_item.as_deref() {
            route_linked_item(&http, item, &id, branch, "park").await;
        }
        return Ok(());
    }

    // A GREEN FINISHES THE CAR THE BUILDER OPENED; IT DOES NOT FILE A
    // TWIN (backlog be025b44). Now that `boss car open` files the packet
    // at build START, the commonest live state a park meets is BUILDING,
    // not parked — the car's `gate` step has not reported, so
    // `parked_car_for` above correctly answered `None`. Reading that as
    // "no car" is the exact mistake that filed three twins (d052afad,
    // 02165b1d, 6790175e) and it would file a fourth the day builders
    // start opening cars. `building_car_for` is the shared predicate,
    // keyed on `metadata.branch` like the rest (CLAUDE.md §9a).
    let building = building_car_for(&parked, branch).cloned();

    if dry {
        match &building {
            Some(c) => {
                let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
                println!(
                    "boss park: DRY would FINISH car {} — open for {branch} since {} — \
                     not file a second",
                    &id[..8.min(id.len())],
                    c.pointer("/metadata/build_started_at")
                        .and_then(Value::as_str)
                        .unwrap_or("an unrecorded time")
                );
            }
            None => println!("boss park: DRY would file a car for {branch} carrying that receipt"),
        }
        return Ok(());
    }

    // Either the car already exists (opened at build start) or this park
    // files it. From there the two paths are identical: read the packet
    // back, then complete the steps it has not already completed.
    let car = match &building {
        Some(c) => c
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("the building car for {branch} has no id"))?
            .to_string(),
        None => {
            let created = crate::gate::api(
                &http,
                reqwest::Method::POST,
                "/api/jobs",
                Some(car_body(branch, summary, backlog_item.as_deref(), None)),
            )
            .await?;
            created
                .as_ref()
                .and_then(|c| c.get("data").unwrap_or(c).get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("jobs api did not return an id for the new car"))?
                .to_string()
        }
    };

    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{car}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("could not read back car {car}"))?;

    // The writes are decided in core, shared with the auto-park handler,
    // and SKIP whatever the open already completed — the step API refuses
    // a metadata write to a completed step, so re-sending `scope` would
    // 409 on every car a builder opened.
    for w in finish_writes(&job, summary, excludes, test, verified, &receipt, now)
        .map_err(anyhow::Error::msg)?
    {
        crate::gate::api(
            &http,
            reqwest::Method::PUT,
            &format!("/api/jobs/{car}/steps/{}", w.step_id),
            Some(w.body),
        )
        .await?;
    }

    println!(
        "boss park: car {} {} at review — receipt copied, not retyped",
        &car[..8.min(car.len())],
        if building.is_some() {
            "FINISHED and parked"
        } else {
            "parked"
        }
    );

    // THE CAR IS THE ITEM'S BUILD — say so on the item, last, so a
    // failure here costs the routing and not the car.
    if let Some(item) = backlog_item.as_deref() {
        route_linked_item(&http, item, &car, branch, "park").await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn packet(branch: &str, verdict: Option<&str>, receipt: Option<&str>) -> Value {
        let mut step = json!({"title": "Record the receipt", "metadata": {}});
        if let Some(v) = verdict {
            step["metadata"]["verdict"] = json!(v);
        }
        if let Some(r) = receipt {
            step["metadata"]["receipt"] = json!(r);
        }
        json!({
            "kind": "gate-run",
            "metadata": {"branch": branch},
            "steps": [{"title": "Gate launched", "metadata": {}}, step],
        })
    }

    /// A PARKED car packet: open, no train stamp, review step ready.
    /// `subject_branch` is the branch it was FILED under — its subject —
    /// and `branch` the one it CARRIES, which `boss rerail` repoints and
    /// the shared predicates read. For an ordinary car the two are equal.
    fn car(id: &str, subject_branch: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "subject": {"subject_kind": "custom", "id": subject_branch},
            "metadata": {"branch": branch},
            "steps": [{"spec_slug": "review", "title": boss_jobs::car::REVIEW,
                       "status": "ready"}],
        })
    }

    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    #[test]
    fn a_green_receipt_is_copied_verbatim_not_rebuilt() {
        let ps = vec![packet("feat/x", Some("green"), Some(GREEN))];
        let r = receipt_for(&ps, "feat/x", HEAD).unwrap();
        assert_eq!(
            r.raw, GREEN,
            "the receipt must survive byte-for-byte — rebuilding it from parts is the \
             transcription this verb exists to remove"
        );
        assert_eq!(r.head, "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8");
        assert_eq!(r.mode, "full");
    }

    /// THE WIDE RECEIPT, alongside the four-field one above.
    ///
    /// The gate runner reduced `infra/gate.sh`'s account of a run to
    /// `{verdict, head, mode, fails}` before reporting it until
    /// 2026-09-09; it now reports the whole thing. `boss park` copies the
    /// receipt VERBATIM onto the car's gate step and reads only `head`
    /// and `mode` out of it, so the widening must change nothing here —
    /// which is exactly the sort of claim worth a test rather than a
    /// reading.
    #[test]
    fn a_wide_receipt_is_copied_verbatim_too() {
        const WIDE: &str = r#"{"verdict":"green","mode":"auto","scope":"","head":"e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8","dirty":false,"host":"gate-runner-abc","ci":true,"free_gb":91,"unverifiable":[],"report":{"attempts":4,"waited_s":63,"sor_unreachable":true},"checks":[{"name":"fmt","result":"pass","seconds":3}]}"#;
        let ps = vec![packet("feat/x", Some("green"), Some(WIDE))];
        let r = receipt_for(&ps, "feat/x", HEAD).unwrap();
        assert_eq!(r.raw, WIDE, "the whole receipt rides the car, unrebuilt");
        assert_eq!(r.head, "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8");
        assert_eq!(r.mode, "auto");
    }

    #[test]
    fn a_branch_with_no_gate_is_refused() {
        let ps = vec![packet("other", Some("green"), Some(GREEN))];
        let e = receipt_for(&ps, "feat/x", HEAD).unwrap_err().to_string();
        assert!(e.contains("no gate-run packet"), "{e}");
        assert!(
            e.contains("boss gate feat/x"),
            "the refusal must say what to do: {e}"
        );
    }

    #[test]
    fn a_gate_still_running_is_refused() {
        let ps = vec![packet("feat/x", None, None)];
        let e = receipt_for(&ps, "feat/x", HEAD).unwrap_err().to_string();
        assert!(e.contains("has not reported yet"), "{e}");
    }

    /// THE RULE THE VERB EXISTS TO ENFORCE: no receipt, no ride.
    #[test]
    fn a_red_or_lost_gate_is_refused_and_the_receipt_is_shown() {
        for verdict in ["failed", "lost"] {
            let raw =
                format!(r#"{{"verdict": "{verdict}", "head": "{HEAD}", "fails": ["clippy"]}}"#);
            let ps = vec![packet("feat/x", Some(verdict), Some(&raw))];
            let e = receipt_for(&ps, "feat/x", HEAD).unwrap_err().to_string();
            assert!(e.contains(&format!("is `{verdict}`")), "{e}");
            assert!(
                e.contains("clippy"),
                "the refusal must show the receipt, or the reader has to go find it: {e}"
            );
        }
    }

    /// A receipt with no sha vouches for nothing — the symbolic-ref
    /// fallback that produced exactly this on 2026-08-27.
    #[test]
    fn a_receipt_naming_no_head_is_refused() {
        let raw = r#"{"verdict": "green", "head": "origin/feat/x", "mode": "full"}"#;
        let ps = vec![packet("feat/x", Some("green"), Some(raw))];
        // head_now empty = the branch head could not be resolved.
        let e = receipt_for(&ps, "feat/x", "").unwrap_err().to_string();
        assert!(e.contains("names no head"), "{e}");
    }

    /// A re-gate files a second packet for the same branch. What decides
    /// is which packet vouches for the CURRENT head — not which is newer,
    /// and emphatically not which the API happened to list last.
    #[test]
    fn a_red_gate_on_the_current_head_is_not_rescued_by_an_older_green() {
        let red = format!(r#"{{"verdict": "failed", "head": "{HEAD}", "fails": ["fmt"]}}"#);
        let old_green = r#"{"verdict": "green", "head": "1111111111111111111111111111111111111111", "mode": "full"}"#;
        let ps = vec![
            packet("feat/x", Some("green"), Some(old_green)),
            packet("feat/x", Some("failed"), Some(&red)),
        ];
        let e = receipt_for(&ps, "feat/x", HEAD).unwrap_err().to_string();
        assert!(
            e.contains("is `failed`"),
            "a green for a DIFFERENT commit must not vouch for this one: {e}"
        );
    }

    #[test]
    fn a_short_id_resolves_to_the_one_job_it_names() {
        let js = vec![
            json!({"id": "20dfcb03-1616-4b8d-8a7a-d1e34ff96486"}),
            json!({"id": "de6f0c06-a341-4445-9f47-399dc27a60fb"}),
        ];
        assert_eq!(
            resolve_job_id(&js, "20dfcb03").unwrap(),
            "20dfcb03-1616-4b8d-8a7a-d1e34ff96486"
        );
    }

    #[test]
    fn a_full_id_passes_through_without_a_lookup() {
        let full = "de6f0c06-a341-4445-9f47-399dc27a60fb";
        assert_eq!(resolve_job_id(&[], full).unwrap(), full);
    }

    /// THE PACKET THIS COULD NOT NAME (381a4872).
    ///
    /// The candidate list used to be `kind=backlog-item` alone, so a car
    /// answering a piece of David's own feedback was refused — and the
    /// refusal said the id names no Job, which is the opposite of what
    /// was wrong. Now both kinds are concatenated before resolving, so a
    /// user-feedback id resolves like any other.
    #[test]
    fn a_user_feedback_id_resolves_like_a_backlog_id() {
        let candidates = vec![
            json!({"id": "20dfcb03-1616-4b8d-8a7a-d1e34ff96486", "kind": "backlog-item"}),
            json!({"id": "898761cb-3e49-4494-a812-d3ab5fa4bd69", "kind": "user-feedback"}),
        ];
        assert_eq!(
            resolve_job_id(&candidates, "898761cb").unwrap(),
            "898761cb-3e49-4494-a812-d3ab5fa4bd69"
        );
    }

    /// Concatenating the two lists means a prefix can now collide ACROSS
    /// kinds. That has to stay an error: picking one is how the wrong
    /// packet gets linked, and the whole point of the edge is that it is
    /// ref-checked.
    #[test]
    fn a_prefix_matching_both_kinds_is_ambiguous_not_guessed() {
        let candidates = vec![
            json!({"id": "abc12345-1616-4b8d-8a7a-d1e34ff96486", "kind": "backlog-item"}),
            json!({"id": "abc12345-3e49-4494-a812-d3ab5fa4bd69", "kind": "user-feedback"}),
        ];
        let err = resolve_job_id(&candidates, "abc12345")
            .expect_err("a prefix matching two packets must not resolve");
        assert!(
            err.to_string().contains("matches 2 Jobs"),
            "the error must say how many it matched: {err}"
        );
    }

    /// The failure this exists to prevent: an id typed from memory that
    /// looks right and names nothing.
    #[test]
    fn an_id_that_names_nothing_is_refused() {
        let js = vec![json!({"id": "20dfcb03-1616-4b8d-8a7a-d1e34ff96486"})];
        let e = resolve_job_id(&js, "a5f7beef").unwrap_err().to_string();
        assert!(e.contains("no Job whose id starts with"), "{e}");
        assert!(
            e.contains("from memory"),
            "the refusal should name the habit: {e}"
        );
    }

    /// Two candidates must be an error, never a pick.
    #[test]
    fn an_ambiguous_prefix_is_refused_and_lists_the_candidates() {
        let js = vec![
            json!({"id": "20dfcb03-1616-4b8d-8a7a-d1e34ff96486"}),
            json!({"id": "20dfcb03-9999-4b8d-8a7a-d1e34ff96486"}),
        ];
        let e = resolve_job_id(&js, "20dfcb03").unwrap_err().to_string();
        assert!(e.contains("matches 2 Jobs"), "{e}");
        assert!(e.contains("9999"), "the candidates must be listed: {e}");
    }

    /// THE BUG THIS VERB SHIPPED WITH, AND THE FIX FOR IT.
    ///
    /// On 2026-08-28 `boss park` parked a car on a receipt for a commit
    /// two rebases old. The cause was ordering: the code took the LAST
    /// matching packet, the API returns newest-FIRST, and the unit test
    /// happened to craft a list where those agreed. Comparing the
    /// receipt to the branch head does not depend on ordering at all.
    #[test]
    fn a_receipt_for_a_commit_that_is_no_longer_the_tip_is_refused() {
        let ps = vec![packet("feat/x", Some("green"), Some(GREEN))];
        let moved = "7ed20bbf9d4d75c12b6e89569494e22d994b3f4b";
        let e = receipt_for(&ps, "feat/x", moved).unwrap_err().to_string();
        assert!(e.contains("vouches for its current head"), "{e}");
        assert!(
            e.contains("re-gate"),
            "the refusal must say how to fix it: {e}"
        );
    }

    /// Ordering must not decide the outcome: whichever way round the
    /// packets arrive, the one matching the branch head is the one used.
    #[test]
    fn the_packet_matching_the_head_wins_regardless_of_list_order() {
        let stale = r#"{"verdict": "green", "head": "1111111111111111111111111111111111111111", "mode": "full"}"#;
        for order in 0..2 {
            let mut ps = vec![
                packet("feat/x", Some("green"), Some(GREEN)),
                packet("feat/x", Some("green"), Some(stale)),
            ];
            if order == 1 {
                ps.reverse();
            }
            let r = receipt_for(&ps, "feat/x", HEAD)
                .unwrap_or_else(|e| panic!("order {order} failed: {e}"));
            assert_eq!(r.head, HEAD, "order {order} picked the wrong packet");
        }
    }

    /// A dead `lost` packet naming the branch head must not shadow the
    /// real green that followed it (ed7f1355: a refused launch left an
    /// orphan whose hand-closed receipt matched first in API order, and
    /// unblocking the true verdict meant editing a closed packet).
    #[test]
    fn a_lost_packet_on_the_same_head_does_not_shadow_the_green() {
        let lost = format!(
            r#"{{"verdict": "lost", "head": "{HEAD}", "mode": "", "fails": ["launch refused"]}}"#
        );
        for order in 0..2 {
            let mut ps = vec![
                packet("feat/x", Some("lost"), Some(&lost)),
                packet("feat/x", Some("green"), Some(GREEN)),
            ];
            if order == 1 {
                ps.reverse();
            }
            let r = receipt_for(&ps, "feat/x", HEAD)
                .unwrap_or_else(|e| panic!("order {order} failed: {e}"));
            assert_eq!(r.head, HEAD, "order {order}");
        }
        // ...and with NO green on record, the lost verdict still refuses
        // with its honest no-evidence message.
        let ps = vec![packet("feat/x", Some("lost"), Some(&lost))];
        let e = receipt_for(&ps, "feat/x", HEAD).unwrap_err().to_string();
        assert!(e.contains("`lost`"), "{e}");
    }
    /// A RE-RAILED CAR IS FOUND WHEN PARKING THE BRANCH IT NOW CARRIES.
    ///
    /// `boss rerail` repoints `metadata.branch` at the new branch and
    /// leaves the car's SUBJECT as the branch it was FILED under. The
    /// shared predicate has always decided on `metadata.branch`, so the
    /// only thing that hid the car was the read: narrowed by
    /// `subject_id={branch}`, the API returned nothing for the re-railed
    /// branch, "no car" was read as "no car exists", and `boss park`
    /// filed a TWIN (backlog 6790175e; three twins on 2026-09-08). The
    /// fixture is the shape car 538775dd actually has.
    #[test]
    fn a_rerailed_car_is_found_when_parking_the_branch_it_carries() {
        let rerailed = car(
            "car-rerail",
            "feat/arrival-runs-the-probe",
            "feat/arrival-runs-the-probe-rerail",
        );
        let others = [
            car("car-a", "fix/other", "fix/other"),
            car("car-b", "fix/another", "fix/another"),
        ];
        let all: Vec<Value> = others.iter().cloned().chain([rerailed.clone()]).collect();

        let found = parked_car_for(&all, "feat/arrival-runs-the-probe-rerail")
            .expect("the re-railed car must be found by the branch it CARRIES, so park refreshes");
        assert_eq!(found.get("id").and_then(Value::as_str), Some("car-rerail"));

        // THE DEFECT, named: the old read asked the API for cars whose
        // SUBJECT is the branch. Simulate that server-side filter and the
        // re-railed car is not in the answer at all — which is how a twin
        // got filed for a branch that already had a car.
        let subject_narrowed: Vec<Value> = all
            .iter()
            .filter(|c| {
                c.pointer("/subject/id").and_then(Value::as_str)
                    == Some("feat/arrival-runs-the-probe-rerail")
            })
            .cloned()
            .collect();
        assert!(
            parked_car_for(&subject_narrowed, "feat/arrival-runs-the-probe-rerail").is_none(),
            "the subject-narrowed read is what hid the car and filed the twin"
        );
    }

    /// And the read reaches past page one: `gate::all_open_cars` pages on
    /// `total`, so a car opened days ago but re-gated today — sorted to
    /// the tail of `opened_on DESC` — is found rather than twinned. 832
    /// ship-a-change packets exist as of 2026-09-09, so this is not
    /// hypothetical.
    #[tokio::test]
    async fn a_car_past_page_one_is_found_rather_than_twinned() {
        use crate::train::{PAGE_LIMIT, list_all_pages};
        let mut open: Vec<Value> = (0..149)
            .map(|i| {
                car(
                    &format!("car-{i}"),
                    &format!("fix/f{i}"),
                    &format!("fix/f{i}"),
                )
            })
            .collect();
        open.push(car("car-tail", "fix/filed-as", "fix/tail-car"));
        let open_ref = &open;
        let gathered = list_all_pages(|offset| async move {
            let page: Vec<Value> = open_ref
                .iter()
                .skip(offset)
                .take(PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({
                "data": page, "total": open_ref.len(), "offset": offset, "limit": PAGE_LIMIT,
            })))
        })
        .await
        .unwrap();
        let found = parked_car_for(&gathered, "fix/tail-car")
            .expect("the car on page two must be found, not twinned");
        assert_eq!(found.get("id").and_then(Value::as_str), Some("car-tail"));
    }
    /// THE PIN ON THE READ ITSELF. The two tests above show that the
    /// shared predicate finds a re-railed car once the car is IN the
    /// list; the defect was never the predicate, it was that the read
    /// asked the API for cars whose SUBJECT is the branch, so the car
    /// never reached the predicate. That is a property of the request,
    /// which has no return value to assert on — so it is pinned at the
    /// source: this module must gather its cars through
    /// `gate::all_open_cars` and must never narrow a car read by
    /// `subject_id`. Red on the pre-fix source, which read
    /// a car list narrowed to the subject the car was filed under.
    #[test]
    fn park_never_narrows_its_car_read_by_subject() {
        let src = include_str!("park.rs");
        // The needle is assembled rather than written out, so this test
        // cannot match its own source and fail on itself.
        let narrowed = format!("&{}=", "subject_id");
        assert!(
            !src.contains(&narrowed),
            "boss park must not narrow a car read by subject_id: a car's subject is only \
             the branch it was FILED under, and `boss rerail` repoints metadata.branch \
             without moving it, so the narrowed read misses a re-railed car and files a \
             twin (backlog 6790175e)"
        );
        assert!(
            src.contains("all_open_cars"),
            "the open cars come from gate::all_open_cars — the one definition rerail, \
             receipt and channels already read, which pages on total"
        );
    }
}
