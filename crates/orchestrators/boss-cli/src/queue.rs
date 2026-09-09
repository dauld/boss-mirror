//! `boss queue` — read the feedback triage board from a terminal.
//!
//! Ported from the python heredoc that lived in
//! `infra/feedback-queue.sh` (directive 26d61c97: no python runs the
//! BOSS system). The shell file survives as an exec shim for operator
//! muscle memory; its history lives on here.
//!
//! The board at /system/feedback is the operator's view; this is the
//! same thing for whoever is playing the agent. Read-only on purpose:
//! taking an item, annotating it, or closing it goes through the board
//! or the step API, so every state change carries an actor. A command
//! that could also write would make "who triaged this" ambiguous
//! exactly where the audit trail matters most.
//!
//! Columns are derived, not stored — same as the board. But this
//! derives them from facts the board does not own: a Job is done when
//! the JOB is closed, and it is with an agent when ANY step carries an
//! agent request.
//!
//! That is deliberate. The first version of this reader found the
//! triage step the way the board did at the time — by matching a step
//! kind — and drifted the same day, when the board switched to finding
//! it by its authority gate. The reader then reported a freshly-filed
//! item as already triaged, which is the worst way for a queue reader
//! to be wrong. Two copies of "how to find the triage step" is the
//! fact-that-lives-twice failure in CLAUDE.md §9a; the fix is not a
//! comment telling the next person to sync them, it is not needing the
//! rule here at all.
//!
//! Reads jobs-api directly rather than through the gateway. The
//! gateway is the BROWSER edge: it authenticates a session cookie and
//! strips every inbound `x-boss-*` header, so an operator tool has no
//! way to present itself there. Terminal tooling goes to the service
//! port with an actor header — the same path verify-smoke.sh and
//! verify-replay.sh take.
//!
//! The shell ancestor once curled the gateway anonymously. That worked
//! only because demo mode minted an `audit-readonly` session for
//! anyone who asked; when that was removed the reader started
//! returning 401 and a stack trace. Anonymous read was never the
//! contract, it was a side effect.
//!
//! (One failure mode the port retires outright: the python used to be
//! embedded in a double-quoted shell string, where a literal double
//! quote silently truncated the program — which is exactly how its
//! fork_step function broke on first write.)

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::train::truthy;

// Reads are policy-gated; an unheadered call lands as `guest`, which
// holds Workflow read and nothing else. Reading is all this does —
// the module doc above is the reason writes are not added here. The
// header comes from `identity` like every other verb's: the id names
// whoever ran the command (it used to be the fixed slug
// `it-triage-queue`), and the shape has one definition rather than a
// copy per module (CLAUDE.md §9a).

/// The agent hand-off record, from whichever step carries it.
fn agent_request(job: &Value) -> Option<&Value> {
    for s in job
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(md) = s.get("metadata")
            && truthy(md.get("agent_requested_at"))
        {
            return Some(md);
        }
    }
    None
}

/// The step that asks for a disposition. Found by the enum field it
/// declares, which is the same data the board reads — not a second
/// copy of the rule for which step is the triage step. A pipe-shaped
/// field_type IS the fork marker; the Workflow lint reads it the same
/// way to prove every value has a successor.
fn fork_step(job: &Value) -> Option<(&Value, String)> {
    let steps = job.get("steps").and_then(Value::as_array)?;
    for s in steps {
        for f in s
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let piped = f
                .get("field_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains('|');
            if truthy(f.get("required"))
                && piped
                && let Some(name) = f.get("name").and_then(Value::as_str)
            {
                return Some((s, name.to_string()));
            }
        }
    }
    // Jobs opened before the fork existed keep their old steps forever
    // — a gated step with no disposition field. Same fallback the board
    // uses, for the same reason: without it a routed legacy item reads
    // as still waiting, which is a queue reader lying about the queue.
    for s in steps {
        if truthy(s.get("metadata").and_then(|m| m.get("authority_role"))) {
            return Some((s, "disposition".to_string()));
        }
    }
    None
}

fn column(job: &Value) -> String {
    // A routed item is NOT waiting. Reporting it as waiting is how the
    // shell ancestor hid two in-flight items during a triage session —
    // the opposite of what a queue reader is for.
    if job.get("status").and_then(Value::as_str) == Some("closed") {
        return "done".to_string();
    }
    if let Some((step, field)) = fork_step(job) {
        let status = step
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == "completed" || status == "skipped" {
            let chosen = step.get("metadata").and_then(|m| m.get(&field));
            return match chosen {
                Some(c) if truthy(Some(c)) => match c {
                    Value::String(s) => format!("routed:{s}"),
                    other => format!("routed:{other}"),
                },
                _ => "routed".to_string(),
            };
        }
    }
    if agent_request(job).is_some() {
        "with-agent".to_string()
    } else {
        "waiting".to_string()
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub async fn run(want: &str) -> Result<()> {
    // jobs-api base, resolved once. No default on purpose: with
    // `BOSS_JOBS_URL` unset this refuses and names the system of record
    // rather than defaulting to 127.0.0.1, which on boss-gcp is a
    // second, older stack holding different data (packet aa783636).
    // Shared with the other read verbs via gate::resolve_jobs_base.
    let base = crate::gate::resolve_jobs_base(None)?;
    let url = format!("{base}/api/jobs?kind=user-feedback&limit=200");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header(
            "x-boss-user",
            crate::identity::header(&crate::identity::reader()),
        )
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let body: Value = resp.json().await?;
    let rows = match body {
        Value::Object(mut o) if o.contains_key("data") => o.remove("data").unwrap_or(Value::Null),
        other => other,
    };
    let Value::Array(rows) = rows else {
        bail!("expected a job list from {url}");
    };

    let mut buckets: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for k in ["waiting", "with-agent", "done"] {
        buckets.insert(k.to_string(), Vec::new());
    }
    for j in &rows {
        buckets.entry(column(j)).or_default().push(j);
    }

    // Routed buckets are discovered from the data, so a new disposition
    // in the registry shows up here without editing this reader.
    let routed: Vec<String> = buckets
        .keys()
        .filter(|k| k.starts_with("routed"))
        .cloned()
        .collect();
    let order: Vec<String> = if want == "all" {
        ["waiting".to_string(), "with-agent".to_string()]
            .into_iter()
            .chain(routed)
            .chain(["done".to_string()])
            .collect()
    } else {
        if !buckets.contains_key(want) {
            bail!(
                "unknown column '{want}' — one of: {}",
                buckets.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        vec![want.to_string()]
    };

    for col in &order {
        let items = &buckets[col];
        println!("{col}  ({})", items.len());
        if items.is_empty() {
            println!("    (empty)");
        }
        for j in items {
            let md = j.get("metadata").cloned().unwrap_or(Value::Null);
            let id = j.get("id").and_then(Value::as_str).unwrap_or("?");
            println!(
                "    {}  {}",
                take_chars(id, 8),
                md.get("route").and_then(Value::as_str).unwrap_or("?")
            );
            let msg = md
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .replace('\n', " ");
            println!("       {}", take_chars(&msg, 100));
            let owner = j.get("owner_id").and_then(Value::as_str).unwrap_or("?");
            let since = agent_request(j)
                .and_then(|m| m.get("agent_requested_at"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            match since {
                Some(ts) => println!(
                    "       from {owner}  ·  with agent since {}",
                    take_chars(ts, 16)
                ),
                None => println!("       from {owner}"),
            }
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// `boss queue` used to default `BOSS_JOBS_URL` to
    /// `http://127.0.0.1:7900`, so run on boss-gcp without an instance
    /// set it read the SECOND, older stack — a confident answer about
    /// the wrong deployment (packet aa783636). It now resolves through
    /// `gate::resolve_jobs_base(None)`, which refuses when the env names
    /// no instance and points at the system of record. `queue` has no
    /// `--jobs-url` flag, so `None` is the whole input; this pins the
    /// refusal it now wires in.
    #[test]
    fn without_an_instance_the_queue_refuses_and_names_the_record() {
        let m = crate::gate::resolve_jobs_base_from(None, None)
            .expect_err("no BOSS_JOBS_URL must refuse, not default to 127.0.0.1")
            .to_string();
        assert!(m.contains("10.20.0.34:7900"), "must name the record: {m}");
        assert!(m.contains("127.0.0.1:7900"), "must warn of the trap: {m}");
    }

    #[test]
    fn an_instance_env_gives_the_queue_its_base() {
        assert_eq!(
            crate::gate::resolve_jobs_base_from(None, Some("http://sor:7900".into())).unwrap(),
            "http://sor:7900",
            "BOSS_JOBS_URL, when set, is used verbatim"
        );
    }
}
