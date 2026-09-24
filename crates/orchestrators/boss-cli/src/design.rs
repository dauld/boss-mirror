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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::gate::api;

/// One open question, in the shape the tracker reads.
fn question(anchor: &str, title: &str, proposal: &str) -> Value {
    json!({ "anchor": anchor, "title": title, "proposal": proposal })
}

/// THE PACKET A DESIGN ANSWERS, read off the design — one accessor for
/// the declared `design-doc.answers` job edge this module writes.
///
/// Three callers ask three different questions of the same key and each
/// must read it the same way: `steps::design_link_check` ("does this
/// design answer THAT packet"), `gate::item_a_design_answers` ("which
/// item does a `--park-design` car close"), and the writer above. A
/// blank value is NOT an answer — the edge guard reads `''` as "no
/// claim to check" (migration 104), so a design carrying one answers
/// nothing and must read as absent rather than as an id (CLAUDE.md §9a:
/// collapse, do not pin).
pub(crate) fn answers_edge(design: &Value) -> Option<&str> {
    design
        .get("metadata")
        .and_then(|m| m.get("answers"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|a| !a.is_empty())
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

/// Is this value a file's NAME rather than a document's text? A single
/// line (nothing after trimming but one line) that either is one token
/// ending in `.md`/`.txt` or names a file `exists` answers for. Prose
/// that merely mentions a file ("fold it into decisions.md") is several
/// words and no file, so it passes; an empty body is not a path.
/// `exists` is handed in so the shape is pinned without a filesystem.
pub(crate) fn path_shaped(text: &str, exists: impl Fn(&str) -> bool) -> bool {
    let line = text.trim();
    if line.is_empty() || line.contains('\n') {
        return false;
    }
    let one_token = !line.contains(char::is_whitespace);
    (one_token && (line.ends_with(".md") || line.ends_with(".txt"))) || exists(line)
}

/// `--markdown` takes the doc's TEXT, and the natural misreading of an
/// option whose value is a whole document is to hand it a file name.
/// The verb accepted that: two designs (11e60367, 55417146) reached
/// David's review queue with a one-line /tmp path for a body, rendered
/// on /it/design as "carried by this packet · not yet a file" followed
/// by the path, and neither could be reviewed until the packet and the
/// step's copy were rewritten by hand (backlog 1763d5af, 2026-09-18).
/// A body is prose and a path is not prose, so the path-shaped value
/// is refused at the flag, naming the door that reads a file.
pub(crate) fn body_is_prose(flag: &str, text: &str, exists: impl Fn(&str) -> bool) -> Result<()> {
    if path_shaped(text, exists) {
        bail!(
            "{flag} takes the doc body as text, and {:?} is a path, not prose — pass \
             `--markdown-file <PATH>` to read the body from that file. A design filed \
             with a path for a body reaches the reviewer with nothing to read.",
            text.trim()
        );
    }
    Ok(())
}

/// The same refusal one flag over: a question's title and proposal are
/// prose too, and a path in either reaches the reviewer as a question
/// nobody can answer.
pub(crate) fn question_is_prose(q: &Value, exists: impl Fn(&str) -> bool) -> Result<()> {
    for key in ["title", "proposal"] {
        let text = q.get(key).and_then(Value::as_str).unwrap_or("");
        if path_shaped(text, &exists) {
            bail!(
                "--question takes its {key} as text, and {text:?} is a path, not prose — write \
                 the question inline (`anchor|title|proposal`); only the doc body can come \
                 from a file, through `--markdown-file <PATH>`."
            );
        }
    }
    Ok(())
}

/// The job body, pure so the shape is pinned by tests rather than by
/// the doc comment above it.
pub(crate) fn design_job_body(
    title: &str,
    markdown: &str,
    questions: &[Value],
    no_open_questions: bool,
    answers: Option<&str>,
    owner: &str,
) -> Value {
    let mut body = json!({
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
        // The platform owner as the registry answers it (backlog
        // 3c23662d), or nobody for the jobs API to resolve from the
        // kind's owner_role — never a literal person.
        "owner_id": owner,
        "priority": "standard",
        "tags": ["design"],
        // No `opened_on`: the create handler injects it off its clock
        // and stamps the filing instant as `metadata.opened_at` only
        // when it does — this body's `today` silenced the stamp on
        // every design doc (dd3624a0, 2026-09-15; envelope: a7a07ffb).
        "subject": {"subject_kind": "custom", "id": "boss-platform"},
        "metadata": {
            "title": title,
            "markdown": markdown,
            "questions": questions,
            // Always present, never omitted — see the module header.
            "no_open_questions": if no_open_questions { "true" } else { "false" },
        },
    });
    // The feedback (or backlog item) this design answers, as the
    // DECLARED `design-doc.answers` job edge — ref-checked and
    // prefix-normalised at the write like ship-a-change's
    // `backlog_item`, and the link the dispatcher follows when the
    // design's review completes to complete that packet's
    // design-review (complete-feedback-design-review-on-design-
    // review-decided; the -on-design-doc-published rule is the
    // backstop for a design decided before it was live, 8f83cade).
    // Absent, not null, when there is none: the edge guard resolves a
    // present key.
    if let Some(feedback) = answers {
        body["metadata"]["answers"] = json!(feedback);
    }
    body
}

/// The question the feedback's `design-review` step is given at the
/// moment a design is filed for it. Until backlog 5f0b2661 that step
/// carried `verdict: ""` and nothing else — the design IS the question,
/// and it lived on the other packet — so it rendered as a statement,
/// and if the operator decided it first the design went unread (David,
/// bug 4f6019d7: "There is no question, just a statement"). Prose a
/// person reads, so the short id.
pub(crate) fn feedback_question(design_title: &str, design_id: &str) -> String {
    let short = &design_id[..8.min(design_id.len())];
    format!(
        "Decide design '{design_title}' ({short}) at /it/design — deciding it completes this \
         step: approved when its questions are all decided, and the build goes ahead as the \
         design proposes."
    )
}

/// One step write this verb owes: method, path, body.
pub(crate) type StepWrite = (reqwest::Method, String, Value);

/// THE STEP MERGE DOOR, `PATCH /api/jobs/{job}/steps/{step}/metadata`:
/// the keys named are merged into what the step holds, in one
/// transaction against the row as it stands, and nothing unnamed is
/// touched.
///
/// Every step write here used to be a PUT carrying `metadata`, and the
/// step PUT REPLACES metadata wholesale (backlog e39a9d2a, the car after
/// `boss prove`, 2026-09-24). Two of the three read the step first and
/// laid their key over it — correct only if nothing wrote between the
/// read and the PUT, and the feedback's review was read BEFORE the
/// design was filed. The third, the review-step mirror, sent a fresh
/// body and deleted every key the registry materializes at admission
/// (`authority_role`, `station`, `audience`, `claimable`,
/// `metadata_defaults`) on every design filed. The item's last car makes
/// the PUT refuse a metadata body; the merge door is the form it routes
/// to, so this verb is on it first.
fn step_merge(job_id: &str, step_id: &str, md: Value) -> StepWrite {
    (
        reqwest::Method::PATCH,
        format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
        md,
    )
}

/// The status-only flip that follows a merge: a PUT carrying no
/// `metadata`, so it can drop no stored key.
fn step_flip(job_id: &str, step_id: &str) -> StepWrite {
    (
        reqwest::Method::PUT,
        format!("/api/jobs/{job_id}/steps/{step_id}"),
        json!({ "status": "completed" }),
    )
}

/// The feedback's design-review gets its question through the merge
/// door, carrying `question` alone — so `authority_role`, which keeps
/// the step gated, and every other key it holds stay as they are.
pub(crate) fn feedback_question_write(
    feedback: &str,
    review_step: &str,
    design_title: &str,
    design_id: &str,
) -> StepWrite {
    step_merge(
        feedback,
        review_step,
        json!({ "question": feedback_question(design_title, design_id) }),
    )
}

/// Where a packet stands on its design route, as `--answers` needs it:
/// the `design-review` step the question goes onto, and the
/// `draft-design` step this verb completes when the route has one.
///
/// `review` is `None` when the packet HAS a design route but none of
/// it is open — decided already, or triaged somewhere else. The edge
/// is still written; nothing is completed; `records_only` carries the
/// sentence the verb prints saying so (design c0d2787a q2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DesignRoute {
    pub review: Option<Value>,
    pub draft: Option<Value>,
    /// Why this route completes nothing, when it completes nothing.
    pub records_only: Option<String>,
}

fn step_by_slug<'a>(packet: &'a Value, slug: &str) -> Option<&'a Value> {
    packet
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
}

fn is_open(step: &Value) -> bool {
    matches!(
        step.get("status").and_then(Value::as_str),
        Some("ready" | "active")
    )
}

/// Can a design be filed against this packet, and what does the verb
/// then write? Pure, so the shapes it must accept are pinned.
///
/// RECORDING IS NOT COMPLETING (design c0d2787a, answered
/// 2026-09-21). This used to refuse whenever the target had no OPEN
/// design route — "a design filed against it would complete nothing
/// when it closes" — which declined to record a TRUE FACT because a
/// side effect would not fire. The loss is larger than the no-op it
/// prevented: the author then writes the relationship into freeform
/// metadata, where nothing resolves it, nothing ref-checks it and
/// nothing can query it. Measured the day this changed: four
/// different ad-hoc spellings in one session, beside one declared
/// edge that was refused.
///
/// So the edge is written whenever the packet is the KIND of thing a
/// design decides, and the verb says plainly what it will and will
/// not cause. The refusal that remains is a different claim — that
/// `answers` does not apply at all — and it now names the relation
/// that does.
///
/// Two protocol versions are live at once (in-flight packets keep
/// theirs). Before f90ca046, routing to `design` opened
/// `design-review` directly, so an OPEN review is the whole test.
/// Since it, the route opens the executor's `draft-design` and the
/// review is `ready_when = steps.draft-design.done` — so an open
/// DRAFT is the other way in, and the verb completes it with the
/// design's id after filing. Neither open is a refusal that names the
/// fix: triage the packet to `design`, or it was already decided.
pub(crate) fn answerable(packet: &Value) -> std::result::Result<DesignRoute, String> {
    let short = packet
        .get("id")
        .and_then(Value::as_str)
        .map(|id| &id[..8.min(id.len())])
        .unwrap_or("?");
    let kind = packet.get("kind").and_then(Value::as_str).unwrap_or("?");
    let review = step_by_slug(packet, "design-review").ok_or_else(|| {
        format!(
            "packet {short} ({kind}) has no design-review step, so `answers` does not apply \
             to it — only a user-feedback or backlog-item can be DECIDED by a design. If \
             that packet merely brought this design about, that is the `occasioned_by` \
             relation: set it in the design's metadata, where it is declared, resolved and \
             queryable"
        )
    })?;
    if is_open(review) {
        return Ok(DesignRoute {
            review: Some(review.clone()),
            draft: None,
            records_only: None,
        });
    }
    let draft = step_by_slug(packet, "draft-design");
    if let Some(draft) = draft.filter(|d| is_open(d)) {
        return Ok(DesignRoute {
            review: Some(review.clone()),
            draft: Some(draft.clone()),
            records_only: None,
        });
    }
    let status = review.get("status").and_then(Value::as_str).unwrap_or("?");
    let draft_status = draft
        .and_then(|d| d.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("absent");
    Ok(DesignRoute {
        review: None,
        draft: None,
        records_only: Some(format!(
            "packet {short}'s design-review is {status} and its draft-design is \
             {draft_status}, so nothing on it is waiting for this design: the edge is \
             recorded and NO step will be completed when the design is decided. If you \
             meant to put the decision in front of someone, triage {short} to `design` \
             first"
        )),
    })
}

/// The draft-design completion, in order: `design_id` through the merge
/// door, then the status-only flip. Merge FIRST — a step's
/// required-at-done fields are judged when it flips to completed.
pub(crate) fn draft_done_writes(
    feedback: &str,
    draft_step: &str,
    design_id: &str,
) -> [StepWrite; 2] {
    [
        step_merge(feedback, draft_step, json!({ "design_id": design_id })),
        step_flip(feedback, draft_step),
    ]
}

/// The questions mirrored onto the filed design's own `review-design`
/// step, through the merge door (see [`step_merge`] for why).
pub(crate) fn review_mirror_write(
    design: &str,
    review_step: &str,
    body: &Value,
    doc_path: &str,
) -> StepWrite {
    step_merge(design, review_step, review_step_metadata(body, doc_path))
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

/// What a design may still ASK David, in the words the drafting
/// procedures use (backlog 4f71e608; the Workflow rows' copies are
/// pinned by boss-jobs' the_author_decides_from_the_company_frame.rs).
/// Everything else the author decides from the company frame.
const ESCALATE_ONLY: &str = "strategy or priority trade-offs, trust and security boundaries, \
                             credentials, money, and brand or voice";

pub async fn run(
    title: String,
    markdown: String,
    markdown_file: Option<PathBuf>,
    questions: Vec<String>,
    no_questions: bool,
    doc_path: Option<String>,
    answers: Option<String>,
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
             failure this verb exists to prevent.\n\n\
             A question is for David, and only for {ESCALATE_ONLY}. Every other \
             choice, decide from the company frame (one person plus agents; a hosting \
             business on the open-source release; our instance runs only the modules \
             it uses; the demo tenant's leftovers cleared out, then cleaned up), write it into \
             the markdown with its reason, and file with --no-questions when none are \
             left (backlog 4f71e608)."
        );
    }
    // The body: read from the file named, or the text given — and a
    // path handed to the text flag is refused here, before anything is
    // filed (backlog 1763d5af). clap keeps the two flags exclusive.
    let is_file = |p: &str| Path::new(p).is_file();
    let markdown = match markdown_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("--markdown-file: reading {}", path.display()))?,
        None => {
            body_is_prose("--markdown", &markdown, is_file)?;
            markdown
        }
    };
    let parsed = questions
        .iter()
        .map(|q| parse_question(q).and_then(|q| question_is_prose(&q, is_file).map(|()| q)))
        .collect::<Result<Vec<_>>>()?;
    let http = reqwest::Client::new();

    // `--answers`: the feedback (or backlog item) this design decides.
    // Read BEFORE filing — the edge needs the full id, and the packet
    // must be on its design route (`answerable`); a design filed for a
    // packet nobody routed to design would carry an edge the close
    // rule can act on nothing with. Refusing here keeps the filer on
    // the line, where the fix is one triage away.
    let answered = match answers.as_deref() {
        Some(given) => {
            let id = crate::job::fetch_and_resolve(&http, given).await?;
            let packet = api(
                &http,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await?
            .with_context(|| format!("--answers: reading packet {id}"))?;
            let route = answerable(&packet).map_err(|e| anyhow::anyhow!("--answers: {e}"))?;
            Some((id, route))
        }
        None => None,
    };

    let owner = crate::owner::for_filing_at(&crate::gate::resolve_jobs_base(None)?).await;
    let body = design_job_body(
        &title,
        &markdown,
        &parsed,
        no_questions,
        answered.as_ref().map(|(id, _)| id.as_str()),
        &owner,
    );
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

    // The other half of the link: the answered packet's design-review
    // step gets a real question, naming this design. The edge on the
    // design is what the close rule follows; this is what the person
    // assigned that step reads. Through the step merge door, so the
    // step's own keys are untouched (e39a9d2a).
    if let Some((feedback, route)) = &answered {
        // RECORDED, BUT COMPLETING NOTHING. The edge went onto the
        // design at filing, which is the fact; there is no open step
        // to put a question on, so say what that means and stop —
        // loud and recorded beats silent and absent (c0d2787a q2).
        if let Some(why) = &route.records_only {
            println!(
                "boss design: {short} records `answers` {} — {why}",
                &feedback[..8]
            );
        } else if let Some(review) = &route.review {
            let sid = review
                .get("id")
                .and_then(Value::as_str)
                .context("the answered packet's design-review step has no id")?;
            let (method, path, md) = feedback_question_write(feedback, sid, &title, &id);
            api(&http, method, &path, Some(md)).await.with_context(|| {
                format!(
                    "writing the question onto {}'s design-review (the design {short} is filed \
                 and carries the edge; only the question is missing)",
                    &feedback[..8]
                )
            })?;
            // The draft step is done BY THIS VERB, carrying the id it just
            // got back (f90ca046): the record is copied from the filing,
            // never retyped, and completing it is what opens the review —
            // which by now already asks its question, so it is never ready
            // and empty.
            if let Some(draft) = &route.draft {
                let did = draft
                    .get("id")
                    .and_then(Value::as_str)
                    .context("the answered packet's draft-design step has no id")?;
                for (method, path, body) in draft_done_writes(feedback, did, &id) {
                    api(&http, method, &path, Some(body))
                        .await
                        .with_context(|| {
                            format!(
                                "completing {}'s draft-design with design_id {short} (the design \
                                 is filed, the edge and the question are written; only the \
                                 draft's record is missing)",
                                &feedback[..8]
                            )
                        })?;
                }
            }
            println!(
                "boss design: {short} answers {} — its design-review now asks for this design, \
             and deciding the design completes it",
                &feedback[..8]
            );
        }
    }

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
    let (method, path, step_md) =
        review_mirror_write(&id, &sid, &body, doc_path.as_deref().unwrap_or(""));
    api(&http, method, &path, Some(step_md))
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
        let with = design_job_body("t", "m", &[], true, None, "emp-owner");
        let without = design_job_body("t", "m", &[], false, None, "emp-owner");
        assert_eq!(with["metadata"]["no_open_questions"], json!("true"));
        assert_eq!(without["metadata"]["no_open_questions"], json!("false"));
    }

    /// The tracker reads the STEP, so the questions must be mirrored
    /// there — a doc carrying them only on the Job renders empty, which
    /// is exactly how this defect presented on 2026-09-02.
    #[test]
    fn the_review_step_carries_the_questions_too() {
        let q = vec![question("Q1", "which brick first?", "the cheap one")];
        let body = design_job_body("t", "# doc", &q, false, None, "emp-owner");
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
    ///
    /// `opened_on` is injected by the handler before it deserializes
    /// (the body leaves it to the clock, dd3624a0), so injecting it
    /// here reproduces what the type actually sees — as gate.rs does.
    #[test]
    fn the_body_deserializes_into_the_job_type_the_api_parses_it_as() {
        let body = as_the_handler_sees_it(design_job_body(
            "the doc",
            "# body",
            &[],
            true,
            None,
            "emp-owner",
        ));
        let job: boss_core::job::Job = serde_json::from_value(body).expect(
            "design body must deserialize into Job — this is verbatim what the API does before \
             it admits the packet",
        );
        assert_eq!(job.kind, "design-doc");
        assert_eq!(job.title, "the doc");
        assert_eq!(job.owner_id, "emp-owner", "the owner is the one handed in");
    }

    /// The create handler injects `opened_on` off its clock before it
    /// deserializes the body (boss-jobs http/jobs.rs); a test that asks
    /// `Job` what it admits has to do the same.
    fn as_the_handler_sees_it(mut body: Value) -> Value {
        body.as_object_mut()
            .expect("body is an object")
            .insert("opened_on".into(), json!("2026-09-15"));
        body
    }

    /// The create handler stamps `metadata.opened_at` — the precise
    /// filing instant behind the one-day `opened_on` — ONLY when its
    /// clock owns the date, i.e. when the body carries no `opened_on`
    /// (boss-jobs http/jobs.rs). This body sent `now.date_naive()`, so
    /// no design doc had a filing instant (measured 2026-09-15: the
    /// three newest design-docs all lacked `opened_at` while every
    /// pr-train beside them carried one; backlog dd3624a0). `boss
    /// design` files today's doc, never a backdated one.
    #[test]
    fn the_design_body_leaves_the_open_date_to_the_api_clock() {
        let body = design_job_body("t", "m", &[], true, None, "emp-owner");
        assert!(
            body.get("opened_on").is_none(),
            "`opened_on` must be left to the create handler's clock, \
             or the packet gets no `opened_at`: {body}"
        );
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
            None,
            "emp-owner",
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

    /// `--answers <feedback>` (backlog 5f0b2661). The design names the
    /// feedback it answers as `metadata.answers` — a DECLARED job edge
    /// on `design-doc`, ref-checked at the write like ship-a-change's
    /// `backlog_item` — never as prose. Until now the only link was
    /// `metadata.design_packet`, a string an operator typed onto the
    /// feedback, which nothing read: the design was decided at
    /// /it/design and the feedback's own design-review step stayed
    /// open, an empty second decision for the same person (David's bug
    /// 4f6019d7, 2026-09-15). ABSENT when not given, not null: the edge
    /// guard treats a present key as a reference to resolve.
    #[test]
    fn answers_rides_as_the_declared_edge_and_is_absent_otherwise() {
        const FEEDBACK: &str = "61366e5a-d15f-472c-a667-f4cc007ef8f8";
        let with = design_job_body("t", "m", &[], false, Some(FEEDBACK), "emp-owner");
        assert_eq!(with["metadata"]["answers"], json!(FEEDBACK));
        let without = design_job_body("t", "m", &[], false, None, "emp-owner");
        assert!(
            without["metadata"].get("answers").is_none(),
            "a design that answers nothing carries no edge key at all"
        );
        // Still the body the API admits.
        let job: boss_core::job::Job =
            serde_json::from_value(as_the_handler_sees_it(with)).expect("deserializes");
        assert_eq!(job.metadata["answers"], json!(FEEDBACK));
    }

    /// The feedback's design-review step is an `answer-question` with
    /// no question — the design IS the question, and it lives on the
    /// other packet. So the verb writes a real one, naming the design
    /// and saying what deciding it does. The step's own keys survive
    /// because the question goes through the step MERGE door carrying
    /// `question` alone (backlog e39a9d2a): nothing else is named, so
    /// nothing else — `authority_role`, `verdict`, the keys the registry
    /// materialized — can be dropped.
    #[test]
    fn the_feedbacks_design_review_gets_a_real_question_through_the_merge_door() {
        let (method, path, md) = feedback_question_write(
            "61366e5a-d15f-472c-a667-f4cc007ef8f8",
            "s-review",
            "A car lands where its change goes live",
            "c6bd173e-3dc9-426f-8fff-866a3b2a6117",
        );
        assert_eq!(method, reqwest::Method::PATCH);
        assert_eq!(
            path,
            "/api/jobs/61366e5a-d15f-472c-a667-f4cc007ef8f8/steps/s-review/metadata"
        );
        assert_eq!(
            md.as_object()
                .map(|m| m.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["question".to_string()]),
            "the merge names only the key it adds: {md}"
        );
        let q = md["question"].as_str().expect("a question is written");
        assert!(
            q.contains("A car lands where its change goes live") && q.contains("c6bd173e"),
            "the question names the design by title and short id: {q}"
        );
        assert!(
            q.contains("completes this step"),
            "and says that deciding the design is what completes it: {q}"
        );
        assert!(
            !q.contains("c6bd173e-3dc9"),
            "the short id, not the full one — this is prose a person reads"
        );
    }

    fn packet(steps: Vec<Value>) -> Value {
        json!({
            "id": "54f0ab33-335e-46a7-a49f-d9afd3107d54",
            "kind": "user-feedback",
            "steps": steps,
        })
    }

    fn step(slug: &str, status: &str) -> Value {
        json!({ "id": format!("s-{slug}"), "spec_slug": slug, "status": status,
                "metadata": { "authority_role": "platform-admin" } })
    }

    /// Backlog f90ca046: the design route opens the executor's draft
    /// first, and the review waits on it. The verb must take BOTH
    /// shapes — a packet in flight under the version before (review
    /// open, no draft step at all) and one under it (draft open,
    /// review pending) — and in the second complete the draft. What
    /// it refuses is a packet on neither: not routed to design, or
    /// already decided.
    #[test]
    fn a_packet_is_answerable_at_its_open_review_or_at_its_open_draft() {
        // The shape before f90ca046, still live for in-flight packets.
        let v1 = packet(vec![
            step("triage", "completed"),
            step("design-review", "ready"),
        ]);
        let route = answerable(&v1).expect("an open review is answerable");
        assert_eq!(
            route.review.as_ref().expect("a review to write onto")["spec_slug"],
            json!("design-review")
        );
        assert!(route.records_only.is_none(), "this route completes a step");
        assert!(route.draft.is_none(), "no draft to complete");

        // The shape since: the draft is open and the review pending.
        let v2 = packet(vec![
            step("triage", "completed"),
            step("draft-design", "ready"),
            step("design-review", "pending"),
        ]);
        let route = answerable(&v2).expect("an open draft is answerable");
        assert_eq!(
            route.review.as_ref().expect("a review to write onto")["status"],
            json!("pending")
        );
        assert_eq!(
            route.draft.as_ref().map(|d| d["spec_slug"].clone()),
            Some(json!("draft-design")),
            "the verb completes the draft it stands at"
        );

        // The draft done by hand and the review open: the review is
        // the target, and the draft is not touched again.
        let drafted = packet(vec![
            step("draft-design", "completed"),
            step("design-review", "active"),
        ]);
        assert!(answerable(&drafted).expect("open review").draft.is_none());

        // Not routed to design (both pending), or already decided:
        // the fact is RECORDED and the verb says what it will not
        // cause. Refusing here used to push the author into freeform
        // metadata, which is worse in every way (c0d2787a q2).
        for (draft, review) in [("pending", "pending"), ("completed", "completed")] {
            let route = answerable(&packet(vec![
                step("draft-design", draft),
                step("design-review", review),
            ]))
            .expect("a packet with a design route is recordable even when nothing is open");
            assert!(
                route.review.is_none() && route.draft.is_none(),
                "nothing open means nothing to complete"
            );
            let why = route
                .records_only
                .expect("a sentence saying what will not happen");
            assert!(
                why.contains("54f0ab33") && why.contains(review) && why.contains(draft),
                "it names the packet and both statuses: {why}"
            );
            assert!(
                why.contains("NO step will be completed"),
                "it says plainly that nothing completes: {why}"
            );
        }

        // A kind a design cannot DECIDE is still a refusal — `answers`
        // does not apply — and it names the relation that does.
        let err = answerable(&json!({ "id": "abc", "kind": "ship-a-change", "steps": [] }))
            .expect_err("no design-review step");
        assert!(err.contains("ship-a-change"), "{err}");
        assert!(
            err.contains("occasioned_by"),
            "a refusal points at the door that fits: {err}"
        );
    }

    /// `--markdown` takes the doc's TEXT, and the natural misreading of
    /// an option whose value is a whole document is to hand it a file
    /// name. The verb accepted that: two designs (11e60367, 55417146)
    /// reached David's review queue with a one-line /tmp path for a
    /// body, and neither could be reviewed until the packet and the
    /// step's copy were rewritten by hand (backlog 1763d5af,
    /// 2026-09-18). A body is prose; a path is not prose. A single
    /// token ending in .md/.txt, or a single line naming a file that
    /// exists, is refused and told about `--markdown-file`.
    #[test]
    fn a_path_is_not_a_body() {
        let none = |_: &str| false;
        let tmp_file = |p: &str| p == "/home/david/design-body.md" || p == "notes";
        for path in [
            "/home/david/design-body.md",
            "docs/design/x.md",
            "body.txt",
            " README.md\n",
        ] {
            assert!(path_shaped(path, none), "a path-shaped body: {path:?}");
            let err = body_is_prose("--markdown", path, none).expect_err("refused");
            assert!(
                err.to_string().contains("--markdown-file")
                    && err.to_string().contains(path.trim()),
                "the refusal names the path and the door: {err}"
            );
        }
        // A bare name is a path when it names a file that exists.
        assert!(path_shaped("notes", tmp_file));
        assert!(
            !path_shaped("notes", none),
            "and just a word when it does not"
        );
        // Prose is prose: several lines, or one that only mentions a file.
        for prose in [
            "# the doc\n\nBody, on lines.\n",
            "fold it into architecture-decisions.md",
            "ship the cheap one",
            "",
        ] {
            assert!(
                !path_shaped(prose, tmp_file),
                "prose, not a path: {prose:?}"
            );
            body_is_prose("--markdown", prose, tmp_file).expect("accepted");
        }
        // A one-line body that is prose still passes: the shape refused
        // is a path, not brevity.
        body_is_prose("--markdown", "# only a heading", none).expect("accepted");
    }

    /// The same refusal on a question: its title and proposal are prose
    /// too, and a path in either is the same misreading one flag over.
    #[test]
    fn a_question_is_prose_too() {
        let none = |_: &str| false;
        let q = parse_question("Q1|first brick?|ship the cheap one").unwrap();
        question_is_prose(&q, none).expect("prose");
        let bad = parse_question("Q1|first brick?|/home/david/proposal.md").unwrap();
        let err = question_is_prose(&bad, none).expect_err("a path proposal is refused");
        assert!(
            err.to_string().contains("/home/david/proposal.md")
                && err.to_string().contains("--question"),
            "{err}"
        );
        let bad = parse_question("Q1|title.txt|a real proposal").unwrap();
        assert!(
            question_is_prose(&bad, none).is_err(),
            "a path title is refused"
        );
    }

    /// The refusal of a doc with no questions is the one place this verb
    /// instructs an author on questions, and it used to say only "a
    /// design doc needs open questions" — so the author filled the flag
    /// with choices the company frame answered, and David got three of
    /// them on design e1dba350 (backlog 4f71e608). It now names what a
    /// question is FOR, in the words the drafting procedures use, and
    /// offers `--no-questions` for a doc whose choices were decided.
    #[tokio::test]
    async fn the_no_questions_refusal_says_what_a_question_is_for() {
        let err = run(
            "a title".into(),
            "a body".into(),
            None,
            vec![],
            false,
            None,
            None,
        )
        .await
        .expect_err("a doc with neither questions nor the flag is refused");
        let text = err.to_string();
        for phrase in [
            "strategy or priority trade-offs, trust and security boundaries, credentials, money, and brand or voice",
            "decide",
            "--no-questions",
        ] {
            assert!(text.contains(phrase), "the refusal says `{phrase}`: {text}");
        }
    }

    /// The draft's completion carries the id the filing returned —
    /// copied, never retyped — as TWO writes (backlog e39a9d2a): the id
    /// through the step merge door, then a status-only flip. Merge
    /// first, because `design_id` is what the flip is judged on; the flip
    /// carries no `metadata`, so it can drop no stored key.
    #[test]
    fn the_draft_merges_the_filed_id_then_flips_the_status_alone() {
        let writes = draft_done_writes("fb-1", "s-draft", "5fc71f03-db4f-4be2-9839-484ccf29781a");
        assert_eq!(writes.len(), 2, "one merge, one flip: {writes:?}");
        let (method, path, body) = &writes[0];
        assert_eq!(*method, reqwest::Method::PATCH);
        assert_eq!(path, "/api/jobs/fb-1/steps/s-draft/metadata");
        assert_eq!(
            body,
            &json!({ "design_id": "5fc71f03-db4f-4be2-9839-484ccf29781a" })
        );
        let (method, path, body) = &writes[1];
        assert_eq!(*method, reqwest::Method::PUT);
        assert_eq!(path, "/api/jobs/fb-1/steps/s-draft");
        assert_eq!(body, &json!({ "status": "completed" }));
    }

    /// The questions are mirrored onto the design's own review step
    /// through the merge door too. That step was just admitted, so every
    /// key it holds is one the registry materialized (`authority_role`,
    /// `station`, `audience`, `claimable`, `metadata_defaults`); a PUT of
    /// the fresh mirror replaced all of them, on every design filed
    /// (correction_2026_09_23 on backlog e39a9d2a, design.rs ~549-571).
    #[test]
    fn the_review_mirror_merges_onto_the_review_step() {
        let q = vec![question("Q1", "which brick first?", "the cheap one")];
        let body = design_job_body("t", "# doc", &q, false, None, "emp-owner");
        let (method, path, md) = review_mirror_write("d-1", "s-review", &body, "docs/design/x.md");
        assert_eq!(method, reqwest::Method::PATCH);
        assert_eq!(path, "/api/jobs/d-1/steps/s-review/metadata");
        assert_eq!(md, review_step_metadata(&body, "docs/design/x.md"));
        // The merge door DELETES a key sent as null, where the PUT stored
        // it; the mirror sends none, so nothing it means to write is lost.
        assert!(
            md.as_object()
                .is_some_and(|m| m.values().all(|v| !v.is_null())),
            "no mirrored key is null: {md}"
        );
    }

    /// The text half of the pins above: no step write in this verb
    /// carries a `metadata` body through the PUT any longer, so a later
    /// edit that builds one by hand again fails here by name.
    #[test]
    fn no_design_step_write_puts_metadata() {
        let src = include_str!("design.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first piece");
        for banned in [
            "json!({ \"metadata\"",
            "\"status\": \"completed\", \"metadata\"",
        ] {
            assert!(
                !production.contains(banned),
                "a design step write PUTs a metadata body ({banned}) — that replaces the \
                 step's stored keys wholesale; write through step_merge (e39a9d2a)"
            );
        }
    }
}
