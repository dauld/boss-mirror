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
use serde_json::{Value, json};

/// What a green gate-run packet says about a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    /// The receipt string, copied verbatim — never rebuilt from parts.
    pub raw: String,
    /// The head it vouches for, read back out for the refusal messages
    /// and for the caller to compare against the branch.
    pub head: String,
    /// `full`, `--auto`, or whatever the runner was given.
    pub mode: String,
}

/// Find the receipt for `branch` among gate-run packets, or refuse.
///
/// Refuses on every path that would otherwise put an unvouched car on a
/// train: no packet at all, a packet still running, and — the one that
/// matters — a packet whose verdict is `failed` or `lost`. A `lost` run
/// is refused as loudly as a failed one: it means the environment died
/// before saying anything, so there is no evidence either way, and
/// "we don't know" must not read as "fine".
pub(crate) fn receipt_for(packets: &[Value], branch: &str) -> Result<Receipt> {
    let mut seen_branch = false;
    let mut newest: Option<(&str, &str)> = None; // (verdict, receipt)

    for p in packets {
        let md = p.get("metadata").and_then(Value::as_object);
        let b = md
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
            let sm = s.get("metadata").and_then(Value::as_object);
            let verdict = sm.and_then(|m| m.get("verdict")).and_then(Value::as_str);
            let raw = sm.and_then(|m| m.get("receipt")).and_then(Value::as_str);
            if let (Some(v), Some(r)) = (verdict, raw) {
                newest = Some((v, r));
            }
        }
    }

    if !seen_branch {
        bail!(
            "no gate-run packet for `{branch}`. A car does not ride a train without a \
             receipt — gate it first (`boss gate {branch}`), then park it."
        );
    }
    let Some((verdict, raw)) = newest else {
        bail!(
            "the gate for `{branch}` has not reported yet. Wait for a verdict \
             (`boss gate {branch} --wait`) rather than parking a car whose gate is \
             still running."
        );
    };
    if verdict != "green" {
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

/// The ship-a-change packet body for a car.
pub(crate) fn car_body(branch: &str, summary: &str, backlog_item: Option<&str>) -> Value {
    let mut metadata = json!({ "branch": branch, "summary": summary });
    if let Some(item) = backlog_item {
        // A declared job edge — ref-checked by the API at the write, which
        // is what makes it safe to write here rather than by hand. A
        // mistyped id is refused instead of silently pointing at nothing.
        metadata["backlog_item"] = json!(item);
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

/// The step titles this verb fills, in the order the workflow runs them.
///
/// Matched against the materialised step title. `ship-a-change` names
/// them in its registry row, so a rename there is a rename here — the
/// same coupling the gate-runner has, and it is pinned the same way:
/// the verb refuses rather than guesses when a step is missing.
const SCOPE: &str = "Declare the boundary";
const BUILD: &str = "Build it";
const GATE: &str = "Green, and observed working";

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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    branch: &str,
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    backlog_item: Option<String>,
    dry: bool,
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
    let receipt = receipt_for(&open, branch)?;
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
            let all = crate::gate::rows(
                crate::gate::api(
                    &http,
                    reqwest::Method::GET,
                    "/api/jobs?kind=backlog-item&limit=200",
                    None,
                )
                .await?,
            );
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

    let filled = [
        (SCOPE, json!({"summary": summary, "excludes": excludes})),
        (BUILD, json!({"test": test})),
        (
            GATE,
            json!({
                "gates": if receipt.mode.is_empty() { "full" } else { &receipt.mode },
                // VERBATIM. The whole point of the verb.
                "receipt": receipt.raw,
                "verified": verified,
            }),
        ),
    ];
    for (title, metadata) in filled {
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

    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    #[test]
    fn a_green_receipt_is_copied_verbatim_not_rebuilt() {
        let ps = vec![packet("feat/x", Some("green"), Some(GREEN))];
        let r = receipt_for(&ps, "feat/x").unwrap();
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
        let e = receipt_for(&ps, "feat/x").unwrap_err().to_string();
        assert!(e.contains("no gate-run packet"), "{e}");
        assert!(
            e.contains("boss gate feat/x"),
            "the refusal must say what to do: {e}"
        );
    }

    #[test]
    fn a_gate_still_running_is_refused() {
        let ps = vec![packet("feat/x", None, None)];
        let e = receipt_for(&ps, "feat/x").unwrap_err().to_string();
        assert!(e.contains("has not reported yet"), "{e}");
    }

    /// THE RULE THE VERB EXISTS TO ENFORCE: no receipt, no ride.
    #[test]
    fn a_red_or_lost_gate_is_refused_and_the_receipt_is_shown() {
        for verdict in ["failed", "lost"] {
            let raw = format!(r#"{{"verdict": "{verdict}", "head": "abc", "fails": ["clippy"]}}"#);
            let ps = vec![packet("feat/x", Some(verdict), Some(&raw))];
            let e = receipt_for(&ps, "feat/x").unwrap_err().to_string();
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
        let e = receipt_for(&ps, "feat/x").unwrap_err().to_string();
        assert!(e.contains("names no head"), "{e}");
    }

    /// A re-gate files a second packet for the same branch; the latest
    /// verdict is the one that counts, so an old green must not rescue a
    /// branch whose newest gate went red.
    #[test]
    fn the_newest_verdict_wins_over_an_older_one() {
        let red = r#"{"verdict": "failed", "head": "aaa", "fails": ["fmt"]}"#;
        let ps = vec![
            packet("feat/x", Some("green"), Some(GREEN)),
            packet("feat/x", Some("failed"), Some(red)),
        ];
        let e = receipt_for(&ps, "feat/x").unwrap_err().to_string();
        assert!(
            e.contains("is `failed`"),
            "an older green must not mask a newer red: {e}"
        );
    }

    #[test]
    fn the_car_body_carries_the_fields_the_api_demands() {
        let b = car_body("feat/x", "A thing does the thing. And more.", None);
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
        );
        assert_eq!(
            b["metadata"]["backlog_item"],
            "de6f0c06-a341-4445-9f47-399dc27a60fb"
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
}
