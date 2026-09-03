//! `boss design` — file a design-doc packet in the shape the protocol
//! can actually process.
//!
//! WHY THIS EXISTS. David, 2026-09-02: *"Design doc failures are pretty
//! common. Makes me think we don't provide good instructions on
//! triggering a job of that protocol."* The cause runs deeper than
//! instructions: the `design-doc` workflow's `metadata_schema` is `{}`,
//! its review step declares `title`/`markdown`/`doc_path` and NOT
//! `questions` — the one field the whole protocol exists to process —
//! and a Job's metadata is never validated against a schema anywhere.
//! So a malformed draft is admitted silently and the cost lands on the
//! reviewer, who opens a design doc with nothing to answer.
//!
//! I proved it the same hour by falling in it: a packet filed with its
//! questions written as prose inside `metadata.detail` reached David's
//! queue showing zero questions. With full repo access and the
//! convention in front of me, I still got the shape wrong.
//!
//! This is the door (the `2e136a67` fix, part two). It writes the shape
//! read off a doc that renders correctly: job metadata carrying
//! `title` / `markdown` / `questions`, mirrored onto the
//! `review-design` step alongside `doc_path` and an empty
//! `resolutions`. Nobody has to know that, which is the point.
//!
//! `--no-questions` is the flag David asked for: a doc that records a
//! decision already made belongs in the system of record without
//! queuing a review. It writes `no_open_questions = "true"`, which the
//! v4 protocol's predicates route around.
//!
//! THE FLAG IS ALWAYS WRITTEN, never omitted. `boss-expr` errors on an
//! absent identifier rather than resolving it false, and the codebase's
//! own answer to that is to always write the key — `jobs.clear_waiting`
//! clears `waiting_on` to `""` rather than deleting it, "so the edge
//! guard resolves the empty string trivially". A packet that simply
//! left the flag out would stall its own review step.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::gate::api;

/// One open question, in the shape the tracker reads.
fn question(anchor: &str, title: &str, proposal: &str) -> Value {
    json!({ "anchor": anchor, "title": title, "proposal": proposal })
}

/// Parse `Qn|title|proposal` — the flag form, so a shell caller can
/// pass several without a heredoc. The pipe is deliberate: question
/// titles routinely contain commas and colons.
pub(crate) fn parse_question(raw: &str) -> Result<Value> {
    let parts: Vec<&str> = raw.splitn(3, '|').collect();
    if parts.len() != 3 {
        bail!(
            "--question wants `anchor|title|proposal`, got {raw:?}. \
             The pipe separates them because titles routinely contain \
             commas and colons."
        );
    }
    let (a, t, p) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
    if a.is_empty() || t.is_empty() || p.is_empty() {
        bail!("--question needs all three of anchor, title and proposal: {raw:?}");
    }
    Ok(question(a, t, p))
}

/// The job body, pure so the shape is pinned by tests rather than by
/// the doc comment above it.
pub(crate) fn design_job_body(
    title: &str,
    markdown: &str,
    questions: &[Value],
    no_open_questions: bool,
    opened_on: &str,
) -> Value {
    json!({
        "kind": "design-doc",
        "status": "open",
        "owner_id": "emp-david",
        "priority": "standard",
        "tags": ["design"],
        "opened_on": opened_on,
        "subject": {"subject_kind": "custom", "id": "boss-platform"},
        "metadata": {
            "title": title,
            "markdown": markdown,
            "questions": questions,
            // Always present, never omitted — see the module header.
            "no_open_questions": if no_open_questions { "true" } else { "false" },
        },
    })
}

/// The review step's own copy. The tracker reads the STEP, so a doc
/// whose questions live only on the Job renders empty — which is
/// exactly how this defect presented.
pub(crate) fn review_step_metadata(body: &Value, doc_path: &str) -> Value {
    let md = body.get("metadata").cloned().unwrap_or_else(|| json!({}));
    json!({
        "title": md.get("title").cloned().unwrap_or(Value::Null),
        "markdown": md.get("markdown").cloned().unwrap_or(Value::Null),
        "questions": md.get("questions").cloned().unwrap_or_else(|| json!([])),
        "doc_path": doc_path,
        "resolutions": [],
    })
}

pub async fn run(
    title: String,
    markdown: String,
    questions: Vec<String>,
    no_questions: bool,
    doc_path: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    // Refuse before filing, not after: a doc with neither questions nor
    // the flag is the exact packet this verb exists to stop reaching a
    // reviewer.
    if questions.is_empty() && !no_questions {
        bail!(
            "a design doc needs open questions, or --no-questions if it records a \
             decision already made.\n\n\
             Pass questions as `--question 'Q1|title|proposal'` (repeatable). A doc \
             with neither reaches the reviewer with nothing to answer, which is the \
             failure this verb exists to prevent."
        );
    }
    let parsed = questions
        .iter()
        .map(|q| parse_question(q))
        .collect::<Result<Vec<_>>>()?;

    let body = design_job_body(
        &title,
        &markdown,
        &parsed,
        no_questions,
        &now.date_naive().to_string(),
    );
    let http = reqwest::Client::new();
    let created = api(
        &http,
        reqwest::Method::POST,
        "/api/jobs",
        Some(body.clone()),
    )
    .await
    .context("filing the design-doc packet")?;
    let id = created
        .as_ref()
        .and_then(|c| c.get("data").unwrap_or(c).get("id"))
        .and_then(Value::as_str)
        .context("jobs api returned no id for the new design doc")?
        .to_string();
    let short = &id[..8.min(id.len())];

    if no_questions {
        println!("boss design: {short} filed — no open questions, no review queued");
        return Ok(());
    }

    // Mirror onto the review step, which is what the tracker reads.
    let job = api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("re-reading the filed design doc")?;
    let sid = job
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("kind").and_then(Value::as_str) == Some("review-design"))
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .context("the filed doc has no review-design step")?
        .to_string();
    let step_md = review_step_metadata(&body, doc_path.as_deref().unwrap_or(""));
    api(
        &http,
        reqwest::Method::PUT,
        &format!("/api/jobs/{id}/steps/{sid}"),
        Some(json!({ "metadata": step_md })),
    )
    .await
    .context("writing the questions onto the review step")?;

    println!(
        "boss design: {short} filed with {} open question(s) — review queued",
        parsed.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag is written on EVERY doc, both ways. boss-expr errors on
    /// an absent identifier, so a packet that omitted it would stall its
    /// own review step — the trap the module header records.
    #[test]
    fn the_flag_is_always_present() {
        let with = design_job_body("t", "m", &[], true, "2026-09-02");
        let without = design_job_body("t", "m", &[], false, "2026-09-02");
        assert_eq!(with["metadata"]["no_open_questions"], json!("true"));
        assert_eq!(without["metadata"]["no_open_questions"], json!("false"));
    }

    /// The tracker reads the STEP, so the questions must be mirrored
    /// there — a doc carrying them only on the Job renders empty, which
    /// is exactly how this defect presented on 2026-09-02.
    #[test]
    fn the_review_step_carries_the_questions_too() {
        let q = vec![question("Q1", "which brick first?", "the cheap one")];
        let body = design_job_body("t", "# doc", &q, false, "2026-09-02");
        let step = review_step_metadata(&body, "docs/design/x.md");
        assert_eq!(step["questions"].as_array().map(Vec::len), Some(1));
        assert_eq!(step["questions"][0]["anchor"], json!("Q1"));
        assert_eq!(step["markdown"], json!("# doc"));
        assert_eq!(step["doc_path"], json!("docs/design/x.md"));
        assert!(step["resolutions"].as_array().is_some_and(Vec::is_empty));
    }

    /// `anchor|title|proposal`, and a partial one is refused rather than
    /// filed half-formed.
    #[test]
    fn a_question_needs_all_three_parts() {
        let ok = parse_question("Q1 | first brick? | ship the cheap one").unwrap();
        assert_eq!(ok["anchor"], json!("Q1"));
        assert_eq!(ok["title"], json!("first brick?"));
        assert_eq!(ok["proposal"], json!("ship the cheap one"));
        // A proposal may contain pipes; only the first two split.
        let piped = parse_question("Q2|title|a || b").unwrap();
        assert_eq!(piped["proposal"], json!("a || b"));
        for bad in ["Q1|only-two", "|title|proposal", "Q1||proposal"] {
            assert!(parse_question(bad).is_err(), "should refuse {bad:?}");
        }
    }
}
