//! A CORRECTION NAMES WHAT IT CORRECTS — one append-only list, one
//! door, and readers are handed it without asking (design 4105b020,
//! answering backlog 56727f95).
//!
//! THE IMMUTABILITY IS CORRECT AND STAYS. A completed step is a fact:
//! the step API refuses a metadata write to one, and rewriting it
//! would be the forbidden thing. What was missing was a TIE. Measured
//! 2026-09-19: prose damaged by shell substitution sat in a completed
//! step — packet f3e091f0's triage evidence still reads "Ordering trap
//! confirmed:  is required of every rule" — and the only route was an
//! annotation in JOB metadata under a key of the author's invention.
//! A reader of the step saw the damaged sentence and no signal that a
//! correction existed. The design's scan of all 15,039 jobs found about
//! 132 such corrections under 25 different invented key names, and the
//! 409 that refuses the step write was the only guidance any author had
//! ("write to the parent job's metadata"), naming no key at all.
//!
//! So a correction is now a SHAPE, not a key name:
//!
//! - It lives in one reserved JOB-metadata key, [`CORRECTIONS_KEY`], an
//!   ordered list of `{step, field, reads, should_read, why, by, at}`.
//!   The step and the audit event it produced are never touched.
//! - One door writes it — `POST /api/jobs/{id}/steps/{step_id}/corrections`
//!   (`boss correct`) — and refuses what would make it a guess: a step
//!   that is still open (an open step is simply edited), a `field` the
//!   step does not hold, and a `reads` excerpt that is not in that
//!   field's stored text. The last is what proves the author is
//!   correcting the sentence that is actually there.
//! - The generic metadata PATCH refuses the key, and the job PUT
//!   carries the stored list forward, so the list only ever grows.
//! - A correction is withdrawn only by appending an entry carrying
//!   `withdraws: <index>` (Q4: gate-run 9a2576fb has
//!   `verdict_correction` then `verdict_correction_withdrawn`, joined
//!   by nothing but the key name).
//! - The job GET hands each step the entries that target it as
//!   `step.corrections` ([`for_step`]), each carrying its `index`, so a
//!   reader renders a list it was given rather than searching for one.
//!
//! `reproof` and `regate_receipt` stay as they are: they record new
//! evidence that supersedes by precedence, not a correction of what
//! was written, and each already has its reader.
//!
//! Everything here is pure; the handler in `http::steps` does the I/O.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The reserved job-metadata key. Written only through the corrections
/// door; the generic metadata PATCH refuses it by this name.
pub const CORRECTIONS_KEY: &str = "corrections";

/// The hint every refusal of a write to a terminal step carries — the
/// step PUT's freeze, the step metadata merge, and the dispatcher test
/// double that answers the way the real API does. ONE copy (CLAUDE.md
/// §9a): until design 4105b020 it was three copies of a sentence that
/// sent every correcting author to free-form job metadata, which is
/// where the 25 invented key names came from.
pub const TERMINAL_STEP_HINT: &str = "a completed step is a record of what happened, and it is \
     never rewritten. To CORRECT what it says, append a correction beside it: POST \
     /api/jobs/{id}/steps/{step_id}/corrections with {field, reads, should_read, why} \
     (`boss correct <packet> --step <slug> --field <name>`) — it names the field and the \
     excerpt it corrects, and every reader of the step is handed it. To annotate the \
     packet with something new, write to the parent job's metadata \
     (PATCH /api/jobs/{id}/metadata) instead.";

/// The refusal the generic job metadata PATCH answers a `corrections`
/// key with — naming the door, because the caller is by definition
/// trying to record a correction.
pub const PATCH_REFUSAL: &str = "`corrections` is a reserved, append-only list: it is written \
     only through POST /api/jobs/{id}/steps/{step_id}/corrections (`boss correct`), which \
     checks that the field and the excerpt it corrects are really there. A metadata patch \
     could rewrite or erase it";

/// What a caller sends to the door. Every key is optional in the wire
/// shape so a missing one is refused by NAME rather than by serde's
/// generic 422.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorrectionRequest {
    pub field: Option<String>,
    pub reads: Option<String>,
    pub should_read: Option<String>,
    pub why: Option<String>,
    pub withdraws: Option<usize>,
}

/// Why the door refused. Each variant is one sentence a caller can act
/// on; [`Refusal::is_conflict`] says which answer the HTTP layer gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The step is not completed or skipped — it is still editable, so
    /// there is nothing to correct beside.
    StepOpen { status: String },
    /// A required body key is absent or blank.
    Missing(&'static str),
    /// `field` is not a key of the step's metadata.
    UnknownField { field: String, holds: Vec<String> },
    /// `reads` does not appear in the field's stored text.
    ExcerptAbsent { field: String },
    /// `withdraws` names no entry, an entry on another step, a
    /// withdrawal, or an entry already withdrawn.
    BadWithdraw { index: usize, why: &'static str },
}

impl Refusal {
    /// A refusal about the step's STATE (409) rather than the body (422).
    pub fn is_conflict(&self) -> bool {
        matches!(self, Refusal::StepOpen { .. })
    }

    pub fn message(&self) -> String {
        match self {
            Refusal::StepOpen { status } => format!(
                "the step is {status}, not completed or skipped — an open step is still \
                 editable, so change it through PUT /api/jobs/{{id}}/steps/{{step_id}}; a \
                 correction sits beside a step that can no longer change"
            ),
            Refusal::Missing(key) => format!(
                "`{key}` is required and must not be blank — a correction names the field, \
                 quotes what it reads now, and says what it should read"
            ),
            Refusal::UnknownField { field, holds } => format!(
                "the step holds no `{field}` — a correction targets a field that is there. \
                 It holds: {}",
                if holds.is_empty() {
                    "(nothing)".to_string()
                } else {
                    holds.join(", ")
                }
            ),
            Refusal::ExcerptAbsent { field } => format!(
                "`reads` does not appear in the step's stored `{field}` — quote the damaged \
                 text exactly as it is stored; that is what proves the correction is about \
                 the sentence that is really there"
            ),
            Refusal::BadWithdraw { index, why } => {
                format!("`withdraws: {index}` cannot be appended — {why}")
            }
        }
    }
}

/// The corrections list a job's metadata holds, in order. A job with
/// none (or a non-list under the key) holds none.
pub fn list(job_metadata: &Value) -> &[Value] {
    job_metadata
        .get(CORRECTIONS_KEY)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The text a correction's `reads` is checked against: a string field
/// as stored, anything else as its JSON — so an excerpt of a list or an
/// object is still a quote of what the record holds.
pub fn stored_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The step a correction targets, as the door read it.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub step_id: &'a str,
    /// Completed or skipped.
    pub terminal: bool,
    /// The status word, for the refusal.
    pub status: &'a str,
    /// The step's stored metadata.
    pub metadata: &'a Value,
}

/// Judge a request against the step it targets and the list as it
/// stands, and build the entry to append — or say why not. `existing`
/// is the job's list now; `by` is the signing actor, `at` the write's
/// timestamp (RFC 3339).
pub fn entry_for(
    target: Target<'_>,
    existing: &[Value],
    req: &CorrectionRequest,
    by: &str,
    at: &str,
) -> Result<Value, Refusal> {
    let step_id = target.step_id;
    let step_metadata = target.metadata;
    if !target.terminal {
        return Err(Refusal::StepOpen {
            status: target.status.to_string(),
        });
    }
    let why = req.why.as_deref().map(str::trim).unwrap_or("");

    if let Some(index) = req.withdraws {
        let target = existing.get(index).ok_or(Refusal::BadWithdraw {
            index,
            why: "the list holds no entry at that index",
        })?;
        if target.get("step").and_then(Value::as_str) != Some(step_id) {
            return Err(Refusal::BadWithdraw {
                index,
                why: "that entry corrects a different step — withdraw it through that step",
            });
        }
        if target.get("withdraws").is_some() {
            return Err(Refusal::BadWithdraw {
                index,
                why: "that entry is itself a withdrawal; append a new correction instead",
            });
        }
        if existing
            .iter()
            .any(|e| e.get("withdraws").and_then(Value::as_u64) == Some(index as u64))
        {
            return Err(Refusal::BadWithdraw {
                index,
                why: "that entry is already withdrawn",
            });
        }
        if why.is_empty() {
            return Err(Refusal::Missing("why"));
        }
        return Ok(json!({
            "step": step_id,
            "field": target.get("field").cloned().unwrap_or(Value::Null),
            "withdraws": index,
            "why": why,
            "by": by,
            "at": at,
        }));
    }

    let field = nonblank(req.field.as_deref()).ok_or(Refusal::Missing("field"))?;
    let reads = nonblank(req.reads.as_deref()).ok_or(Refusal::Missing("reads"))?;
    // `should_read` may be empty — "this phrase should not be there" is
    // a correction — but it must be SAID, not left out.
    let should_read = req
        .should_read
        .as_deref()
        .ok_or(Refusal::Missing("should_read"))?;

    let holds = step_metadata.as_object().cloned().unwrap_or_else(Map::new);
    let Some(stored) = holds.get(field) else {
        return Err(Refusal::UnknownField {
            field: field.to_string(),
            holds: holds.keys().cloned().collect(),
        });
    };
    if !stored_text(stored).contains(reads) {
        return Err(Refusal::ExcerptAbsent {
            field: field.to_string(),
        });
    }
    Ok(json!({
        "step": step_id,
        "field": field,
        "reads": reads,
        "should_read": should_read,
        "why": why,
        "by": by,
        "at": at,
    }))
}

fn nonblank(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.trim().is_empty())
}

/// The `jobs.step.corrected` payload, built once for both adapters so
/// the in-memory and Pg records of one append cannot disagree.
pub fn corrected_payload(job_id: &str, entry: &Value, index: usize) -> Value {
    json!({
        "job_id": job_id,
        "step_id": entry.get("step").cloned().unwrap_or(Value::Null),
        "index": index,
        "correction": entry,
    })
}

/// `metadata` with `entry` appended to its list — the in-memory
/// adapter's append, and the rule the Pg adapter's one UPDATE states in
/// SQL: a non-object metadata or a non-list under the key folds to
/// empty first. Returns the new metadata and the entry's index.
pub fn appended(metadata: &Value, entry: &Value) -> (Value, usize) {
    let mut md = metadata.as_object().cloned().unwrap_or_default();
    let mut items = md
        .get(CORRECTIONS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items.push(entry.clone());
    let index = items.len() - 1;
    md.insert(CORRECTIONS_KEY.into(), Value::Array(items));
    (Value::Object(md), index)
}

/// The entries that target `step_id`, each with its `index` in the
/// job's list — what the job GET attaches as `step.corrections`.
pub fn for_step(job_metadata: &Value, step_id: &str) -> Vec<Value> {
    list(job_metadata)
        .iter()
        .enumerate()
        .filter(|(_, e)| e.get("step").and_then(Value::as_str) == Some(step_id))
        .map(|(i, e)| {
            let mut entry = e.as_object().cloned().unwrap_or_default();
            entry.insert("index".into(), json!(i));
            Value::Object(entry)
        })
        .collect()
}

/// The job PUT replaces metadata wholesale, so a body built without the
/// list (or with an edited one) would rewrite it. The stored list wins,
/// exactly as the stored `partition` does on that route: carried
/// forward when the job holds one, removed from the body when it does
/// not.
pub fn carry_forward(body_metadata: &mut Value, stored_metadata: &Value) {
    carry_forward_key(body_metadata, stored_metadata, CORRECTIONS_KEY);
}

/// [`carry_forward`] for any reserved, append-only job-metadata key —
/// the re-pin record `repins` (design 7cf202a9 Q3) is the second.
pub fn carry_forward_key(body_metadata: &mut Value, stored_metadata: &Value, key: &str) {
    let stored = stored_metadata.get(key).cloned();
    match (body_metadata.as_object_mut(), stored) {
        (Some(md), Some(list)) => {
            md.insert(key.into(), list);
        }
        (Some(md), None) => {
            md.remove(key);
        }
        (None, Some(list)) => {
            let mut md = Map::new();
            md.insert(key.into(), list);
            *body_metadata = Value::Object(md);
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: &str = "11111111-1111-1111-1111-111111111111";

    fn evidence() -> Value {
        json!({ "evidence": "Ordering trap confirmed:  is required of every rule",
                "disposition": "build" })
    }

    fn req(field: &str, reads: &str, should_read: &str) -> CorrectionRequest {
        CorrectionRequest {
            field: Some(field.into()),
            reads: Some(reads.into()),
            should_read: Some(should_read.into()),
            why: Some("shell substitution ate the word".into()),
            withdraws: None,
        }
    }

    fn judge(terminal: bool, existing: &[Value], r: &CorrectionRequest) -> Result<Value, Refusal> {
        entry_for(
            Target {
                step_id: STEP,
                terminal,
                status: if terminal { "completed" } else { "active" },
                metadata: &evidence(),
            },
            existing,
            r,
            "agent-claude",
            "2026-09-24T00:00:00Z",
        )
    }

    #[test]
    fn a_correction_quoting_the_stored_text_becomes_an_entry_naming_everything() {
        let e = judge(
            true,
            &[],
            &req("evidence", "confirmed:  is", "confirmed: `why` is"),
        )
        .unwrap();
        assert_eq!(
            e,
            json!({
                "step": STEP, "field": "evidence",
                "reads": "confirmed:  is", "should_read": "confirmed: `why` is",
                "why": "shell substitution ate the word",
                "by": "agent-claude", "at": "2026-09-24T00:00:00Z",
            })
        );
    }

    #[test]
    fn an_open_step_is_refused_as_a_conflict_naming_the_door_that_edits_it() {
        let r = judge(false, &[], &req("evidence", "confirmed", "x")).unwrap_err();
        assert!(r.is_conflict());
        assert!(
            r.message().contains("PUT /api/jobs/{id}/steps/{step_id}"),
            "{}",
            r.message()
        );
    }

    #[test]
    fn a_field_the_step_does_not_hold_is_refused_naming_what_it_holds() {
        let r = judge(true, &[], &req("summary", "x", "y")).unwrap_err();
        assert!(!r.is_conflict());
        let m = r.message();
        assert!(
            m.contains("`summary`") && m.contains("disposition, evidence"),
            "{m}"
        );
    }

    #[test]
    fn an_excerpt_that_is_not_in_the_stored_text_is_refused() {
        let r = judge(true, &[], &req("evidence", "confirmed: why is", "y")).unwrap_err();
        assert_eq!(
            r,
            Refusal::ExcerptAbsent {
                field: "evidence".into()
            }
        );
    }

    #[test]
    fn a_blank_excerpt_is_refused_because_it_would_match_anything() {
        let r = judge(true, &[], &req("evidence", "  ", "y")).unwrap_err();
        assert_eq!(r, Refusal::Missing("reads"));
        let mut no_should = req("evidence", "confirmed", "");
        no_should.should_read = None;
        assert_eq!(
            judge(true, &[], &no_should).unwrap_err(),
            Refusal::Missing("should_read")
        );
        // An EMPTY should_read is a said deletion, and is accepted.
        assert!(judge(true, &[], &req("evidence", "confirmed", "")).is_ok());
    }

    #[test]
    fn a_non_string_field_is_quoted_as_its_json() {
        let md = json!({ "items": ["one", "twoo"] });
        let e = entry_for(
            Target {
                step_id: STEP,
                terminal: true,
                status: "completed",
                metadata: &md,
            },
            &[],
            &req("items", "\"twoo\"", "\"two\""),
            "a",
            "t",
        );
        assert!(e.is_ok(), "{e:?}");
    }

    fn existing_one() -> Vec<Value> {
        vec![judge(true, &[], &req("evidence", "confirmed", "confirmed!")).unwrap()]
    }

    #[test]
    fn a_withdrawal_is_a_new_entry_naming_the_index_and_the_field() {
        let existing = existing_one();
        let w = CorrectionRequest {
            withdraws: Some(0),
            why: Some("the original was right".into()),
            ..Default::default()
        };
        let e = judge(true, &existing, &w).unwrap();
        assert_eq!(e["withdraws"], 0);
        assert_eq!(e["field"], "evidence");
        assert_eq!(e["why"], "the original was right");
        assert!(e.get("reads").is_none());
    }

    #[test]
    fn a_withdrawal_must_name_a_live_correction_of_this_step_and_say_why() {
        let existing = existing_one();
        let w = |i: usize, why: &str| CorrectionRequest {
            withdraws: Some(i),
            why: Some(why.into()),
            ..Default::default()
        };
        assert!(matches!(
            judge(true, &existing, &w(3, "x")).unwrap_err(),
            Refusal::BadWithdraw { index: 3, .. }
        ));
        assert_eq!(
            judge(true, &existing, &w(0, " ")).unwrap_err(),
            Refusal::Missing("why")
        );
        let mut other = existing.clone();
        other[0]["step"] = json!("22222222-2222-2222-2222-222222222222");
        assert!(matches!(
            judge(true, &other, &w(0, "x")).unwrap_err(),
            Refusal::BadWithdraw { index: 0, .. }
        ));
        let mut withdrawn = existing.clone();
        withdrawn.push(judge(true, &existing, &w(0, "x")).unwrap());
        assert!(matches!(
            judge(true, &withdrawn, &w(0, "again")).unwrap_err(),
            Refusal::BadWithdraw { index: 0, .. }
        ));
        assert!(matches!(
            judge(true, &withdrawn, &w(1, "a withdrawal of a withdrawal")).unwrap_err(),
            Refusal::BadWithdraw { index: 1, .. }
        ));
    }

    #[test]
    fn for_step_hands_each_step_only_its_own_entries_with_their_index() {
        let md = json!({ "corrections": [
            { "step": "a", "field": "x" },
            { "step": "b", "field": "y" },
            { "step": "a", "field": "z" },
        ]});
        let a = for_step(&md, "a");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["index"], 0);
        assert_eq!(a[1]["index"], 2);
        assert_eq!(a[1]["field"], "z");
        assert!(for_step(&json!({}), "a").is_empty());
        assert!(for_step(&json!({ "corrections": "not a list" }), "a").is_empty());
    }

    #[test]
    fn the_put_carries_the_stored_list_forward_whatever_the_body_says() {
        let stored = json!({ "corrections": [{ "step": "a" }], "k": 1 });
        let mut dropped = json!({ "k": 2 });
        carry_forward(&mut dropped, &stored);
        assert_eq!(dropped, json!({ "k": 2, "corrections": [{ "step": "a" }] }));

        let mut edited = json!({ "corrections": [] });
        carry_forward(&mut edited, &stored);
        assert_eq!(edited["corrections"], json!([{ "step": "a" }]));

        let mut invented = json!({ "corrections": [{ "step": "forged" }] });
        carry_forward(&mut invented, &json!({}));
        assert_eq!(invented, json!({}));

        let mut null_body = Value::Null;
        carry_forward(&mut null_body, &stored);
        assert_eq!(null_body, json!({ "corrections": [{ "step": "a" }] }));
    }

    #[test]
    fn the_terminal_hint_names_the_corrections_door_and_still_the_annotation_door() {
        assert!(TERMINAL_STEP_HINT.contains("POST /api/jobs/{id}/steps/{step_id}/corrections"));
        assert!(TERMINAL_STEP_HINT.contains("boss correct"));
        assert!(TERMINAL_STEP_HINT.contains("PATCH /api/jobs/{id}/metadata"));
    }
}
