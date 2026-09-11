//! The one place that knows how the jobs API spells a packet.
//!
//! WHY THIS IS A MODULE. Counted on 2026-09-11, one pod session
//! (backlog d44f6152): roughly fifteen throwaway parsers written
//! against `boss-api GET … > file`, and THREE of them mis-keyed with a
//! confident wrong conclusion reported before the error was caught. The
//! envelopes genuinely differ per endpoint:
//!
//! - `/api/jobs/{id}` keys the packet `id` and carries `steps` (plural,
//!   an array) and `title`.
//! - `/api/stations/{name}/queue` rows are packet envelopes keyed `id`
//!   with NO `steps` at all — the station's own fields (`station`,
//!   `total`, `wip_limit`, `lens`) wrap them.
//! - `/api/jobs/assignments` rows key the packet `job_id`, its name
//!   `job_title`, and carry ONE `step` (singular, an object). There is
//!   no `id` on the row, and the job's `status` is not on it either.
//! - A gate receipt rides as a JSON **string** inside a step's
//!   `metadata.receipt`, and keys `fails` at the TOP level while a
//!   `checks[]` entry carries only `name` / `result` / `seconds`.
//! - A packet's `metadata` sometimes nests a second `metadata` object
//!   whose keys SHADOW top-level ones with different values (live
//!   example: packet e67cdd56, where nested `context_md` and `proposed`
//!   both differ from the top-level pair).
//!
//! THE SHAPE OF THE DAMAGE is why this is worth a module rather than
//! care: a mis-keyed read returns `None` / `false` / empty, which is
//! indistinguishable from a true negative. The same failure family as
//! an unidentified SoR read (61085a9e) and a truncated page
//! (`a-limit-is-not-a-filter`), and the same family as CLAUDE.md
//! §Doors' "a wrong target answers instead of erroring". So every
//! function here answers from whichever spelling the row actually uses,
//! and the ones that can be absent return `Option` so an absence has to
//! be handled rather than read as a fact.
//!
//! PURE ON PURPOSE. Nothing here does I/O: these are functions of a
//! `Value` the caller already fetched, so the variance is pinned by
//! tests against real trimmed envelopes rather than by a live read.
//!
//! RELATED, NOT COLLAPSED. `gate::red_verdict_detail` also reads a gate
//! receipt, finding it by `spec_slug == "record-verdict"`. [`receipt`]
//! deliberately does NOT copy that locator — it finds the receipt by
//! the key that HOLDS one (the same "find it by the field it declares"
//! move `queue::fork_step` documents), so there is no second copy of
//! which step is the verdict step. What the two do share is the
//! `fails`-then-non-pass-`checks` reading of a red; collapsing that
//! needs an edit inside `gate.rs`, which is the §9a follow-up this
//! module cannot make from here.

use serde_json::Value;

/// The packet's id, however the row spells it.
///
/// `id` on a job or a station-queue row; `job_id` on an assignment or
/// queue-age row, which has no `id` of its own. Reading the wrong one
/// is measured error #2 of d44f6152: a freshly filed design-doc was
/// reported NOT VISIBLE on its own station because the reader asked a
/// station row for `job_id`.
pub(crate) fn job_id(row: &Value) -> Option<&str> {
    row.get("id")
        .and_then(Value::as_str)
        .or_else(|| row.get("job_id").and_then(Value::as_str))
}

/// The packet's title, however the row spells it.
pub(crate) fn job_title(row: &Value) -> Option<&str> {
    row.get("title")
        .and_then(Value::as_str)
        .or_else(|| row.get("job_title").and_then(Value::as_str))
}

/// The steps on a row, however the row carries them.
///
/// A job has `steps`, an array. An assignment row has `step`, a single
/// object — the one workable step, not the packet's whole list. Empty
/// for a station-queue row, which carries no steps at all; an empty
/// answer here means "this row does not hold them", never "the packet
/// has none".
pub(crate) fn steps(row: &Value) -> Vec<&Value> {
    if let Some(arr) = row.get("steps").and_then(Value::as_array) {
        return arr.iter().collect();
    }
    row.get("step")
        .filter(|s| s.is_object())
        .into_iter()
        .collect()
}

/// One step, reduced to what a reader asks about it: where it sits in
/// the protocol, what gates it, and who holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepLine {
    /// `spec_slug` — the step's name in the Workflow row. The stable
    /// handle; `title` is prose and gets reworded between versions.
    pub slug: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    pub assignee: Option<String>,
    /// `metadata.authority_role` — who is allowed to complete it.
    pub authority_role: Option<String>,
    /// Ready or active: the step the packet is AT.
    pub now: bool,
}

pub(crate) fn step_line(step: &Value) -> StepLine {
    let s = |k: &str| step.get(k).and_then(Value::as_str);
    let status = s("status").unwrap_or("?").to_string();
    StepLine {
        slug: s("spec_slug").unwrap_or("-").to_string(),
        kind: s("kind").unwrap_or("?").to_string(),
        now: status == "ready" || status == "active",
        status,
        title: s("title").unwrap_or("?").to_string(),
        assignee: s("assignee_id").map(str::to_string),
        authority_role: step
            .get("metadata")
            .and_then(|m| m.get("authority_role"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// A gate receipt, in the shape the receipt actually has.
///
/// `fails` is a TOP-LEVEL list of strings; a `checks[]` entry carries
/// only `name` / `result` / `seconds` and has no `fails` of its own.
/// Asking a check for `fails` is measured error #1 of d44f6152 — it
/// answered `None` and was reported as "the receipt left `fails` null",
/// with four lines of file and line:col naming where it was not. This
/// struct makes that question unaskable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub verdict: String,
    pub mode: String,
    pub head: String,
    pub checks: usize,
    /// What failed, named. `fails` when the receipt wrote it; otherwise
    /// every check whose `result` is not `pass`, so a red always names
    /// something (CLAUDE.md §Diagnosis: a verdict must name what
    /// failed).
    pub fails: Vec<String>,
}

/// The gate receipt on a packet, and the title of the step carrying it.
///
/// Found by the key that HOLDS a receipt — a step whose
/// `metadata.receipt` parses as a JSON object with a `verdict` — not by
/// the verdict step's slug. Two readers of "which step is the verdict
/// step" is the fact-that-lives-twice failure (CLAUDE.md §9a), and
/// `gate.rs` already owns that one.
pub(crate) fn receipt(job: &Value) -> Option<(String, Receipt)> {
    steps(job).into_iter().find_map(|s| {
        let raw = s.pointer("/metadata/receipt")?.as_str()?;
        let r: Value = serde_json::from_str(raw).ok()?;
        let verdict = r.get("verdict")?.as_str()?.to_string();
        let checks: Vec<&Value> = r
            .get("checks")
            .and_then(Value::as_array)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        let named: Vec<String> = r
            .get("fails")
            .and_then(Value::as_array)
            .map(|f| {
                f.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let fails = if named.is_empty() {
            checks
                .iter()
                .filter(|c| c.get("result").and_then(Value::as_str) != Some("pass"))
                .filter_map(|c| c.get("name").and_then(Value::as_str))
                .map(|n| format!("{n}: not pass (this receipt names no test)"))
                .collect()
        } else {
            named
        };
        // A real green receipt writes `scope: ""`, so "present but
        // empty" must render as absent rather than as a blank column.
        let g = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("-")
                .to_string()
        };
        Some((
            s.get("title")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            Receipt {
                verdict,
                mode: g("mode"),
                head: g("head"),
                checks: checks.len(),
                fails,
            },
        ))
    })
}

/// What a packet's metadata holds, one line per key.
///
/// KEYS, NOT A DUMP — the question asked of a packet is "what does it
/// link to", and a key list with a fitted preview answers it without
/// the reader scrolling a 4 KB claim. No key is dropped, so an absence
/// here is a real absence; `--json` keeps the full copy one flag away
/// (CLAUDE.md §Diagnosis: never reduce the only copy).
///
/// A NESTED `metadata` IS NAMED AS NESTED, and the keys it shadows are
/// called out. That shadow is live: packet e67cdd56 holds `context_md`
/// and `proposed` at BOTH levels with different text, so a reader who
/// finds one and stops has read the wrong one without being told.
pub(crate) fn metadata_lines(md: &Value, width: usize) -> Vec<String> {
    /// Keys are aligned to the widest, but only up to here. One
    /// 45-character key (`why_it_is_a_protocol_gap_and_not_carelessness`,
    /// live on d44f6152) otherwise indents every OTHER key past half the
    /// terminal and leaves nothing for the value — the alignment eating
    /// the content it exists to make readable.
    const KEY_COLUMN: usize = 24;

    let Some(obj) = md.as_object() else {
        return Vec::new();
    };
    let keyw = obj
        .keys()
        .map(|k| k.chars().count())
        .filter(|n| *n <= KEY_COLUMN)
        .max()
        .unwrap_or(0);
    obj.iter()
        .map(|(k, v)| {
            let body = match v.as_object() {
                // The trap, named where it is read.
                Some(inner) if k == "metadata" => {
                    let shadowed: Vec<&str> = inner
                        .keys()
                        .filter(|ik| obj.contains_key(*ik))
                        .map(String::as_str)
                        .collect();
                    let keys: Vec<&str> = inner.keys().map(String::as_str).collect();
                    let mut s = format!(
                        "! a NESTED metadata object, {} key(s): {}",
                        keys.len(),
                        keys.join(", ")
                    );
                    if !shadowed.is_empty() {
                        s.push_str(&format!(
                            " — {} ALSO at the top level, and may differ",
                            shadowed.join(", ")
                        ));
                    }
                    s
                }
                _ => one_line(v),
            };
            // Fit the WHOLE line, not just the value: a key column plus
            // a value each inside the terminal still wraps together,
            // and a wrapped table is a table nobody reads.
            crate::job::fit(&format!("{k:keyw$}  {body}"), width)
        })
        .collect()
}

/// A value as one line: a string unquoted and newline-folded, anything
/// else JSON-encoded. Folding rather than printing the first line
/// matters — a `claim` whose second line holds the point would
/// otherwise read as a one-line claim.
fn one_line(v: &Value) -> String {
    match v {
        Value::String(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `GET /api/jobs/d44f6152-…`, trimmed to two steps and four
    /// metadata keys. Captured with `boss-api` on 2026-09-11.
    const PACKET: &str = r#"{
      "id": "d44f6152-47e8-467b-b2ee-f5c99b741b98",
      "kind": "backlog-item",
      "workflow_version": 5,
      "subject": { "subject_kind": "custom", "id": "bosspipeline" },
      "title": "There is no read verb for a packet or a queue",
      "owner_id": "emp-david",
      "status": "open",
      "priority": "standard",
      "opened_on": "2026-09-11",
      "metadata": { "channel": "monitoring", "claim": "a claim\nwith a second line" },
      "tags": [],
      "simulated": false,
      "steps": [
        { "id": "a697955f-eef0-48a1-80e2-e35e9249b707",
          "job_id": "d44f6152-47e8-467b-b2ee-f5c99b741b98",
          "kind": "trigger", "title": "Filed to the backlog",
          "spec_slug": "filed", "assignee_id": null, "status": "completed",
          "sort_order": 0, "metadata": { "trigger_kind": "operator" } },
        { "id": "d2638b31-c7ff-4c62-a8a6-a122e6d9f06d",
          "job_id": "d44f6152-47e8-467b-b2ee-f5c99b741b98",
          "kind": "task", "title": "Measure the claim, choose a route",
          "spec_slug": "triage", "assignee_id": "claude@algedonic.dev",
          "status": "ready", "sort_order": 1,
          "metadata": { "authority_role": "platform-admin" } }
      ]
    }"#;

    /// `GET /api/stations/q.platform-admin.task/queue`, trimmed to one
    /// row. The row IS a packet envelope keyed `id` — and it carries no
    /// `steps`. Captured with `boss-api` on 2026-09-11.
    const STATION_QUEUE: &str = r#"{
      "station": "q.platform-admin.task",
      "kind": "constraint",
      "discipline": ["priority", "age"],
      "wip_limit": null,
      "over_limit": false,
      "terminal_window_days": null,
      "upstream": null,
      "lens": null,
      "total": 61,
      "data": [
        { "id": "ac356440-2aab-4d0d-af09-93a6f6488ba6",
          "kind": "rotate-a-credential", "workflow_version": 2,
          "subject": { "subject_kind": "custom", "id": "boss-dev-forge-token" },
          "title": "Maiden rotation: the broker's first self-managed token (v2)",
          "owner_id": "emp-david", "status": "open", "priority": "urgent",
          "opened_on": "2026-09-03", "due_on": null, "closed_on": null,
          "metadata": { "filed_by": "claude@algedonic.dev" },
          "tags": [], "simulated": false }
      ]
    }"#;

    /// One `GET /api/jobs/assignments?assignee_id=…` row, trimmed. No
    /// `id`, no `status`, no `steps`: the packet is `job_id`, its name
    /// is `job_title`, and the one workable step is `step` — SINGULAR,
    /// an object. Captured with `boss-api` on 2026-09-11.
    const ASSIGNMENT_ROW: &str = r#"{
      "due_on": null,
      "job_id": "508cc38c-360c-4e02-8847-7438b5d4ea04",
      "job_title": "Experiments Tier 3: the shadow partition",
      "priority": "standard",
      "simulated": false,
      "subject_id": "bosspipeline",
      "subject_kind": "custom",
      "tags": ["experiments", "network"],
      "workflow": "user-feedback",
      "workflow_version": 13,
      "step": {
        "id": "6cc308e3-2079-44aa-a1fe-3c80b4c1cf23",
        "job_id": "508cc38c-360c-4e02-8847-7438b5d4ea04",
        "assignee_id": "claude@algedonic.dev",
        "kind": "task", "spec_slug": "build", "status": "ready",
        "title": "Build the change", "sort_order": 4,
        "metadata": { "authority_role": "platform-admin" }
      }
    }"#;

    /// A gate-run's verdict step, as `GET /api/jobs/326f1532-…` serves
    /// it: the receipt is a JSON STRING, `fails` is top level, and a
    /// `checks[]` entry has only name/result/seconds. Captured with
    /// `boss-api` on 2026-09-11 and cut from 34 checks to three.
    fn gate_run(receipt_json: &str) -> Value {
        json!({
          "id": "326f1532-d981-41dc-a080-0fcf6033e0b0",
          "kind": "gate-run",
          "status": "closed",
          "metadata": { "branch": "fix/every-config-fixture-owns-its-path" },
          "steps": [
            { "spec_slug": "launch", "kind": "task", "title": "Launch the runner",
              "status": "completed", "metadata": {} },
            { "spec_slug": "record-verdict", "kind": "gate-verdict",
              "title": "Record the receipt", "status": "completed",
              "metadata": { "receipt": receipt_json, "verdict": "green" } }
          ]
        })
    }

    const GREEN_RECEIPT: &str = r#"{"verdict":"green","mode":"full","scope":"",
      "head":"5393ff651424b2e1a9c8259be05d73ce79b1a803","dirty":false,
      "host":"gate-fix-every-config-fix-4zzbd-5xthw","ci":false,"free_gb":173,
      "unverifiable":[],
      "checks":[{"name":"fixture","result":"pass","seconds":12},
                {"name":"clippy","result":"pass","seconds":20},
                {"name":"test","result":"pass","seconds":600}],
      "fails":[]}"#;

    // ---- the three measured mis-keys ----------------------------------

    #[test]
    fn the_packet_id_reads_the_same_from_every_envelope_that_carries_one() {
        // Measured error #2: a station row asked for `job_id` answered
        // None, and a filed packet was reported NOT VISIBLE on its own
        // station. Both spellings must answer.
        let packet: Value = serde_json::from_str(PACKET).unwrap();
        let station: Value = serde_json::from_str(STATION_QUEUE).unwrap();
        let assignment: Value = serde_json::from_str(ASSIGNMENT_ROW).unwrap();

        assert_eq!(
            job_id(&packet),
            Some("d44f6152-47e8-467b-b2ee-f5c99b741b98")
        );
        assert_eq!(
            job_id(&station["data"][0]),
            Some("ac356440-2aab-4d0d-af09-93a6f6488ba6"),
            "a station-queue row keys the packet `id`"
        );
        assert_eq!(
            job_id(&assignment),
            Some("508cc38c-360c-4e02-8847-7438b5d4ea04"),
            "an assignment row keys the packet `job_id` and has NO `id`"
        );
        assert!(
            assignment.get("id").is_none(),
            "fixture must keep the trap: the assignment row has no `id` at all"
        );

        // And the title, for the same reason: `title` vs `job_title`.
        assert_eq!(
            job_title(&station["data"][0]),
            Some("Maiden rotation: the broker's first self-managed token (v2)")
        );
        assert_eq!(
            job_title(&assignment),
            Some("Experiments Tier 3: the shadow partition")
        );

        // A row that carries neither says so rather than inventing one.
        assert_eq!(job_id(&json!({"kind": "backlog-item"})), None);
    }

    #[test]
    fn steps_read_from_plural_and_singular_and_an_absence_is_not_a_zero() {
        let packet: Value = serde_json::from_str(PACKET).unwrap();
        let station: Value = serde_json::from_str(STATION_QUEUE).unwrap();
        let assignment: Value = serde_json::from_str(ASSIGNMENT_ROW).unwrap();

        assert_eq!(steps(&packet).len(), 2, "`steps`, an array");
        assert_eq!(
            steps(&assignment).len(),
            1,
            "`step`, one object — measured error #3 crashed on exactly this"
        );
        assert_eq!(
            step_line(steps(&assignment)[0]).slug,
            "build",
            "the singular step must come through as a step, not as a wrapper"
        );
        assert!(
            steps(&station["data"][0]).is_empty(),
            "a station row carries no steps — the caller must not read that as `no steps exist`"
        );
    }

    #[test]
    fn a_step_line_carries_the_slug_the_kind_the_holder_and_the_authority() {
        let packet: Value = serde_json::from_str(PACKET).unwrap();
        let ss = steps(&packet);

        let filed = step_line(ss[0]);
        assert_eq!(filed.slug, "filed");
        assert_eq!(filed.kind, "trigger");
        assert_eq!(filed.status, "completed");
        assert_eq!(filed.assignee, None);
        assert_eq!(filed.authority_role, None);
        assert!(!filed.now);

        let triage = step_line(ss[1]);
        assert_eq!(triage.slug, "triage");
        assert_eq!(triage.status, "ready");
        assert_eq!(triage.assignee.as_deref(), Some("claude@algedonic.dev"));
        assert_eq!(triage.authority_role.as_deref(), Some("platform-admin"));
        assert!(triage.now, "a ready step is where the packet IS");

        // active counts too; pending and skipped do not.
        assert!(step_line(&json!({"status": "active"})).now);
        assert!(!step_line(&json!({"status": "skipped"})).now);
        // A row missing everything renders, rather than panicking.
        assert_eq!(step_line(&json!({})).kind, "?");
    }

    #[test]
    fn the_receipt_keys_fails_at_the_top_and_a_check_has_no_fails_to_ask_for() {
        // Measured error #1: `fails` was read INSIDE each check object,
        // came back None, and "the receipt left `fails` null" was
        // reported to the operator with file and line:col.
        let run = gate_run(GREEN_RECEIPT);
        let (step_title, r) = receipt(&run).expect("the receipt is on the verdict step");
        assert_eq!(step_title, "Record the receipt");
        assert_eq!(r.verdict, "green");
        assert_eq!(r.mode, "full");
        assert_eq!(r.head, "5393ff651424b2e1a9c8259be05d73ce79b1a803");
        assert_eq!(r.checks, 3);
        assert!(r.fails.is_empty(), "a green names no fail: {:?}", r.fails);

        // A red names what failed, from the top-level list.
        let red = gate_run(
            r#"{"verdict":"red","mode":"full","head":"abc1234",
                "checks":[{"name":"test","result":"fail","seconds":90}],
                "fails":["test: boss_cli::envelope::tests::x panicked"]}"#,
        );
        let (_, r) = receipt(&red).unwrap();
        assert_eq!(r.fails, vec!["test: boss_cli::envelope::tests::x panicked"]);

        // A red whose `fails` is empty still names something: the
        // non-pass checks. A verdict nobody can read is the defect.
        let thin = gate_run(
            r#"{"verdict":"red","mode":"full","head":"abc1234",
                "checks":[{"name":"clippy","result":"fail","seconds":20},
                          {"name":"fmt","result":"pass","seconds":2}],
                "fails":[]}"#,
        );
        let (_, r) = receipt(&thin).unwrap();
        assert_eq!(r.fails.len(), 1);
        assert!(r.fails[0].starts_with("clippy: not pass"), "{:?}", r.fails);

        // A packet with no receipt says so; a receipt that is not JSON
        // is not a receipt either. Neither may panic.
        let packet: Value = serde_json::from_str(PACKET).unwrap();
        assert!(receipt(&packet).is_none());
        assert!(receipt(&gate_run("not json at all")).is_none());
        // `scope: ""` in a real green must not render as an empty field.
        let (_, r) = receipt(&gate_run(r#"{"verdict":"green","mode":"","head":""}"#)).unwrap();
        assert_eq!((r.mode.as_str(), r.head.as_str()), ("-", "-"));
    }

    #[test]
    fn a_nested_metadata_object_is_named_as_nested_and_its_shadows_called_out() {
        // Live shape, packet e67cdd56: `context_md` and `proposed` exist
        // at BOTH levels with different text. A reader who finds one and
        // stops has read the wrong one and was never told.
        let md = json!({
            "area": "ux",
            "context_md": "the top-level one",
            "proposed": "the top-level one",
            "reporter": "emp-david",
            "metadata": {
                "artifact": "https://claude.ai/code/artifact/745f5757",
                "context_md": "the NESTED one, different text",
                "proposed": "the NESTED one"
            }
        });
        let lines = metadata_lines(&md, 200);
        assert_eq!(lines.len(), 5, "one line per key, none dropped: {lines:?}");
        let nested = lines
            .iter()
            .find(|l| l.starts_with("metadata"))
            .expect("the nested key must have its own line");
        assert!(nested.contains("NESTED"), "{nested}");
        assert!(nested.contains("3 key(s)"), "{nested}");
        assert!(nested.contains("artifact"), "{nested}");
        assert!(
            nested.contains("context_md, proposed ALSO at the top level"),
            "the shadow is the trap and must be named: {nested}"
        );
        // The nested object's line must NOT be a JSON dump of it.
        assert!(!nested.contains("https://"), "{nested}");
    }

    #[test]
    fn metadata_lines_keep_every_key_and_fold_a_multiline_value() {
        let packet: Value = serde_json::from_str(PACKET).unwrap();
        let lines = metadata_lines(&packet["metadata"], 200);
        assert_eq!(lines.len(), 2);
        let claim = lines.iter().find(|l| l.starts_with("claim")).unwrap();
        assert!(
            claim.contains("a claim with a second line"),
            "a second line must be folded in, not dropped: {claim}"
        );
        assert!(!claim.contains('\n'), "one line per key: {claim}");
        // Keys are aligned to the widest, so the column is readable.
        assert!(lines.iter().all(|l| l.contains("  ")), "{lines:?}");

        // EVERY LINE FITS THE WIDTH GIVEN. A key column plus a value
        // that each fit can still wrap together, and a wrapped table is
        // one nobody reads — so the whole line is fitted, not the value.
        let lines = metadata_lines(&packet["metadata"], 60);
        assert!(lines.iter().all(|l| l.chars().count() <= 60), "{lines:?}");

        // AND ONE ENORMOUS KEY DOES NOT INDENT THE REST PAST THE VALUE.
        // d44f6152 really carries a 45-character key; aligning to it
        // pushed every other value off a 100-column terminal.
        let wide = json!({
            "why_it_is_a_protocol_gap_and_not_carelessness": "the long one",
            "channel": "monitoring"
        });
        let lines = metadata_lines(&wide, 100);
        let channel = lines.iter().find(|l| l.starts_with("channel")).unwrap();
        assert!(
            channel.contains("monitoring") && channel.chars().count() < 40,
            "the short key must not be padded out to the long one: {channel:?}"
        );

        // Not an object: no lines, no panic.
        assert!(metadata_lines(&json!("a string"), 80).is_empty());
        assert!(metadata_lines(&Value::Null, 80).is_empty());
    }
}
