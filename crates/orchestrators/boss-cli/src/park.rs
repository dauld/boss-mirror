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
use boss_jobs::car::{Receipt, car_body, step_fields};
use serde_json::{Value, json};

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
    let known_head = head_now.len() >= 40;
    let chosen = if known_head {
        reported.iter().find(|(_, raw)| head_of(raw) == head_now)
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

fn step_id(job: &Value, title: &str) -> Result<String> {
    job.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the car has no step titled {title:?}. The ship-a-change workflow was \
                 renamed or re-versioned; this verb fills steps by title and will not \
                 guess which one was meant."
            )
        })
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

    if dry {
        println!("boss park: DRY would file a car for {branch} carrying that receipt");
        return Ok(());
    }

    let created = crate::gate::api(
        &http,
        reqwest::Method::POST,
        "/api/jobs",
        Some(car_body(branch, summary, backlog_item.as_deref())),
    )
    .await?;
    let car = created
        .as_ref()
        .and_then(|c| c.get("data").unwrap_or(c).get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("jobs api did not return an id for the new car"))?
        .to_string();

    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{car}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("could not read back car {car}"))?;

    for (title, metadata) in step_fields(summary, excludes, test, verified, &receipt, now) {
        let sid = step_id(&job, title)?;
        crate::gate::api(
            &http,
            reqwest::Method::PUT,
            &format!("/api/jobs/{car}/steps/{sid}"),
            Some(json!({"status": "completed", "metadata": metadata})),
        )
        .await?;
    }

    println!(
        "boss park: car {} parked at review — receipt copied, not retyped",
        &car[..8.min(car.len())]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
