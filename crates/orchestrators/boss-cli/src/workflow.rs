//! `boss workflow publish <kind> <spec.json>` — publish a protocol
//! version without the footgun.
//!
//! WHY THIS IS A VERB. Publishing was a hand-assembled sequence, done
//! three times in one day for ship-a-change v20/v21/v22 and again for
//! v23: GET the active spec, mutate it, re-inject the fields a
//! publishable row needs, `_validate`, create the draft, publish, then
//! GET again to see what actually went live (f2c2ed14).
//!
//! IT CAUSED A LIVE REGRESSION, and the mechanism is worth stating
//! exactly. `POST /api/workflows/{kind}/publish` takes NO BODY — it
//! promotes whatever draft currently exists, and a `{"version": 20}`
//! body is ignored rather than honoured. On 2026-08-28 the draft-create
//! failed 422 for a missing field, so nothing was created, and the
//! follow-up publish promoted a STALE v16 draft left over from earlier
//! work. ship-a-change ran as v16 in production for about four minutes.
//! v16 has no `proven` step at all, so any car admitted in that window
//! would have shipped with no proof step. Zero were — luck, not design.
//!
//! So the load-bearing move here is not convenience. It is REFUSING TO
//! PUBLISH INTO A DIRTY REGISTRY: if a draft already exists that is not
//! the one this command just created, stop and say what is sitting
//! there, because that is precisely the state that promoted v16.
//!
//! The other half is that every step is CHECKED rather than assumed —
//! the draft is read back before it is promoted, and the active row is
//! read back after. A publish that reports success without confirming
//! what went live is how a regression stays invisible for four minutes.

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A draft that is not ours is a refusal, not a warning.
///
/// Returns the version of any draft found, so the message can name it.
/// Pure: the whole point is that this decision is inspectable without a
/// live registry, since the failure it prevents happened in production.
pub(crate) fn blocking_draft(versions: &[Value], ours: Option<i32>) -> Option<i32> {
    versions
        .iter()
        .filter(|v| v.get("status").and_then(Value::as_str) == Some("draft"))
        .filter_map(|v| v.get("version").and_then(Value::as_i64).map(|n| n as i32))
        .find(|v| Some(*v) != ours)
}

/// Did the publish put live what we meant to put live?
///
/// Compares the version AND the step titles, because a version number
/// alone would not have caught the v16 regression — v16 is a perfectly
/// valid version, just the wrong protocol.
pub(crate) fn confirm(active: &Value, want_version: i32, want_titles: &[String]) -> Result<()> {
    let got_version = active
        .get("version")
        .and_then(Value::as_i64)
        .context("the active row carries no version")? as i32;
    let got_titles: Vec<String> = active
        .get("steps")
        .and_then(Value::as_array)
        .map(|ss| {
            ss.iter()
                .filter_map(|s| s.get("title").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if got_version != want_version {
        bail!(
            "PUBLISHED THE WRONG VERSION: meant v{want_version}, the active row is now \
             v{got_version}. This is the v19->v16 shape — publish promotes whatever draft \
             exists, so a stale draft can go live in place of yours."
        );
    }
    if got_titles != want_titles {
        bail!(
            "v{got_version} went live but its steps are not the ones published.\n  \
             live: {got_titles:?}\n  sent: {want_titles:?}"
        );
    }
    Ok(())
}

/// Fields a publishable row needs that a GET of the active row does not
/// hand back in usable form. Carrying them forward is four of the steps
/// this verb replaces, and forgetting one is the 422 that left the
/// stale draft armed.
fn carry_forward(spec: &mut Value, active: Option<&Value>) {
    let obj = match spec.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    obj.entry("status")
        .or_insert_with(|| Value::String("draft".into()));
    if !obj.contains_key("version") {
        obj.insert("version".into(), Value::from(1));
    }
    if let Some(a) = active {
        for k in ["created_at", "authoring_job_id"] {
            if !obj.contains_key(k)
                && let Some(v) = a.get(k)
            {
                obj.insert(k.into(), v.clone());
            }
        }
    }
}

fn titles(spec: &Value) -> Vec<String> {
    spec.get("steps")
        .and_then(Value::as_array)
        .map(|ss| {
            ss.iter()
                .filter_map(|s| s.get("title").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Discard a draft version, then confirm it is GONE by reading the
/// version list back — a 204 is a claim; the read-back is the fact.
pub async fn discard(kind: &str, version: i32) -> Result<()> {
    let http = reqwest::Client::new();
    crate::gate::api(
        &http,
        reqwest::Method::DELETE,
        &format!("/api/workflows/{kind}/versions/{version}"),
        None,
    )
    .await?;
    let versions = crate::gate::rows(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await?,
    );
    let still_there = versions
        .iter()
        .any(|v| v.get("version").and_then(serde_json::Value::as_i64) == Some(i64::from(version)));
    if still_there {
        anyhow::bail!(
            "the DELETE answered but {kind} v{version} is still in the version list — \
             refusing to call that discarded"
        );
    }
    println!("boss workflow: {kind} v{version} draft discarded — confirmed gone");
    Ok(())
}

pub async fn publish(kind: &str, path: &std::path::Path, dry: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading the spec {}", path.display()))?;
    let mut spec: Value =
        serde_json::from_str(&raw).with_context(|| format!("{} is not JSON", path.display()))?;

    // The active row, for the fields a draft needs and for the
    // before/after comparison.
    let active = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/workflows/{kind}"),
        None,
    )
    .await
    .ok()
    .flatten()
    .map(|v| v.get("data").cloned().unwrap_or(v));
    let active = active.map(|a| {
        if a.is_array() {
            a[a.as_array().map_or(0, |x| x.len() - 1)].clone()
        } else {
            a
        }
    });
    carry_forward(&mut spec, active.as_ref());
    let want_titles = titles(&spec);
    if want_titles.is_empty() {
        bail!("that spec declares no steps — refusing to publish a protocol with no work in it");
    }

    // REFUSE INTO A DIRTY REGISTRY, before writing anything.
    let versions = crate::gate::rows(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await?,
    );
    if let Some(stale) = blocking_draft(&versions, None) {
        bail!(
            "a draft of {kind} v{stale} is already sitting in the registry, and publish \
             promotes WHATEVER DRAFT EXISTS — it takes no body and cannot be told which \
             one.\n  Publishing now would put v{stale} live instead of your spec. That is \
             exactly how ship-a-change went from v19 to v16 in production for four \
             minutes.\n  Discard it first: `boss workflow discard {kind} {stale}`."
        );
    }

    // Lint before persisting — the same check the publish path enforces.
    let verdict = crate::gate::api(
        &http,
        reqwest::Method::POST,
        "/api/workflows/_validate",
        Some(spec.clone()),
    )
    .await?;
    let problems = verdict
        .as_ref()
        .and_then(|v| v.get("problems"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !problems.is_empty() {
        bail!("the spec does not lint clean, so nothing was written:\n  {problems:#?}");
    }
    println!(
        "boss workflow: {kind} lints clean ({} steps)",
        want_titles.len()
    );

    if dry {
        println!("boss workflow: DRY — would create a draft and publish it");
        return Ok(());
    }

    // Create the draft, and CHECK IT LANDED. A failed create leaves the
    // publish armed against something else, which is the whole defect.
    let created = crate::gate::api(
        &http,
        reqwest::Method::PUT,
        &format!("/api/workflows/{kind}"),
        Some(spec.clone()),
    )
    .await?
    .map(|v| v.get("data").cloned().unwrap_or(v))
    .context("the draft create returned no body — refusing to publish on that")?;
    let draft_version = created
        .get("version")
        .and_then(Value::as_i64)
        .context("the created draft carries no version — refusing to publish blind")?
        as i32;
    println!("boss workflow: draft v{draft_version} created — verified, not assumed");

    crate::gate::api(
        &http,
        reqwest::Method::POST,
        &format!("/api/workflows/{kind}/publish"),
        None,
    )
    .await?;

    // Read back what actually went live.
    let now_active = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/workflows/{kind}"),
        None,
    )
    .await?
    .map(|v| v.get("data").cloned().unwrap_or(v))
    .context("could not read the active row back")?;
    let now_active = if now_active.is_array() {
        now_active[now_active.as_array().map_or(0, |x| x.len() - 1)].clone()
    } else {
        now_active
    };
    confirm(&now_active, draft_version, &want_titles)?;
    println!(
        "boss workflow: {kind} v{draft_version} is live, with the {} steps sent — confirmed by \
         reading the active row back",
        want_titles.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// THE v16 REGRESSION, as a rule. A stale draft left by an earlier
    /// failed attempt is exactly what `publish` promotes, because it
    /// takes no body and cannot be told which version to promote.
    #[test]
    fn a_stale_draft_blocks_publishing() {
        let versions = vec![
            json!({"version": 30, "status": "active"}),
            json!({"version": 16, "status": "draft"}),
        ];
        assert_eq!(blocking_draft(&versions, None), Some(16));
    }

    /// ...and our own draft does not block us, or the verb could never
    /// publish anything.
    #[test]
    fn our_own_draft_is_not_a_blocker() {
        let versions = vec![
            json!({"version": 30, "status": "active"}),
            json!({"version": 31, "status": "draft"}),
        ];
        assert_eq!(blocking_draft(&versions, Some(31)), None);
    }

    #[test]
    fn a_clean_registry_has_no_blocker() {
        let versions = vec![json!({"version": 30, "status": "active"})];
        assert_eq!(blocking_draft(&versions, None), None);
    }

    /// CONFIRMATION COMPARES THE STEPS, NOT JUST THE NUMBER. v16 was a
    /// perfectly valid version — it was the wrong protocol, and a
    /// version check alone would have called that a success.
    #[test]
    fn a_valid_but_wrong_version_is_caught() {
        let live = json!({"version": 16, "steps": [{"title": "opened"}]});
        let want: Vec<String> = ["opened", "scope", "proven"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let e = confirm(&live, 31, &want).unwrap_err().to_string();
        assert!(e.contains("PUBLISHED THE WRONG VERSION"), "{e}");
        assert!(e.contains("v19->v16"), "{e}");
    }

    /// Right version, wrong contents — the case a version check misses
    /// entirely.
    #[test]
    fn the_right_version_with_the_wrong_steps_is_caught() {
        let live = json!({"version": 31, "steps": [{"title": "opened"}]});
        let want: Vec<String> = ["opened", "settled"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(confirm(&live, 31, &want).is_err());
    }

    #[test]
    fn a_faithful_publish_confirms() {
        let live = json!({"version": 31, "steps": [{"title": "opened"}, {"title": "settled"}]});
        let want: Vec<String> = ["opened", "settled"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(confirm(&live, 31, &want).is_ok());
    }
}
