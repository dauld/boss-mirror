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
//! AND THE DOOR WAS SHUT UNTIL 2026-09-04. The body it built set the
//! doc title in `metadata.title` only, and the filer requires the
//! ENVELOPE's `title` at admission — so every invocation of this verb
//! returned `422 invalid job body: missing title`, and design docs kept
//! going in as hand-built JSON POSTed straight at `/api/jobs`, which is
//! the exact failure mode the verb exists to retire. Nothing in the
//! tree exercised the call, so the omission shipped and survived; the
//! test that now closes it deserializes the body into the same
//! `boss_core::job::Job` the handler does, which asks the authoritative
//! type what it demands instead of listing the fields someone
//! remembered. `gate.rs` learned this the same way, for `tags`.
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
        // THE ENVELOPE'S OWN TITLE, not merely metadata's. The filer
        // requires it at admission ("one line; every lens leads with
        // it") and this verb shipped without it, so `boss design`
        // answered every invocation with `422 invalid job body: missing
        // title` and design docs went in as hand-built JSON POSTed
        // straight at /api/jobs instead — repeatedly, through
        // 2026-09-04. Same string as metadata's copy, from the one
        // argument, so the card and the doc cannot disagree.
        "title": title,
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

    /// THE TEST THAT WOULD HAVE CAUGHT THE BUG THAT MADE THE VERB
    /// UNUSABLE.
    ///
    /// `boss design` 422'd on every invocation — `invalid job body:
    /// missing 'title' (one line; every lens leads with it)` — because
    /// the body carried the doc title in `metadata.title` and nowhere
    /// else, while the filer requires the ENVELOPE's `title` at
    /// admission. So the sanctioned door for filing a design doc could
    /// not file one, and docs went in as hand-built JSON POSTed
    /// straight at `/api/jobs` instead — repeatedly, through
    /// 2026-09-04. A door that 422s is not a door.
    ///
    /// The two tests above pin the fields I already knew to look for,
    /// which is exactly the blind spot that shipped: `title` was not
    /// one of them. `POST /api/jobs` deserializes the body into
    /// `boss_core::job::Job` and rejects what will not, so running that
    /// same deserialization here asks the authoritative type what it
    /// demands rather than asking my memory. `gate.rs` carries the same
    /// pin for the same reason (its verb shipped unable to file too,
    /// for want of `tags`); this crate now has it on both bodies.
    #[test]
    fn the_body_deserializes_into_the_job_type_the_api_parses_it_as() {
        let body = design_job_body("the doc", "# body", &[], true, "2026-09-04");
        let job: boss_core::job::Job = serde_json::from_value(body).expect(
            "design body must deserialize into Job — this is verbatim what the API does before \
             it admits the packet",
        );
        assert_eq!(job.kind, "design-doc");
        assert_eq!(job.title, "the doc");
        assert_eq!(job.owner_id, "emp-david");
    }

    /// The title is written TWICE by design — once on the envelope
    /// (what every lens leads with) and once in metadata, which is
    /// where the tracker and the review step read it. Two copies of one
    /// fact, so they are pinned equal here and written from the single
    /// argument: a doc whose card says one thing and whose body says
    /// another is the drift this costs nothing to prevent.
    #[test]
    fn the_envelope_title_and_the_tracker_title_are_the_same_string() {
        let body = design_job_body(
            "stations hold, they do not drop",
            "# doc",
            &[],
            true,
            "2026-09-04",
        );
        assert_eq!(body["title"], json!("stations hold, they do not drop"));
        assert_eq!(body["title"], body["metadata"]["title"]);
        assert_eq!(
            review_step_metadata(&body, "docs/design/x.md")["title"],
            body["title"]
        );
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
