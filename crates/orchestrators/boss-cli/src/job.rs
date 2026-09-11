//! `boss job` — read, file, and patch packets without hand-building
//! the HTTP that talks about them.
//!
//! WHY THIS IS A VERB. Counted on 2026-08-30, one session: ~23
//! hand-typed `curl` invocations against the jobs API, each carrying
//! the `x-boss-user` header inline (514b39d8). Every one of those is a
//! chance at the two failure classes this crate keeps re-learning:
//!
//! - THE WRONG ACTOR READS AS AN EMPTY SYSTEM. A misspelled role or a
//!   missing header does not error — the API deliberately returns an
//!   empty collection, which once read as catastrophic data loss.
//!   A verb carries the one correct identity; a fresh curl carries
//!   whatever was typed.
//! - THE 422 DANCE. `POST /api/jobs` reports ONE missing envelope
//!   field per 422 (f5dd5167), so filing a packet by hand is three
//!   round-trips of guessing. `file` defaults the whole envelope.
//!
//! CONFIRMATION OVER STATUS CODES, everywhere. This same session hit
//! two silent 204 no-ops — a step PUT carrying an unknown field name,
//! and the write-once class (a07cfddd) — so no action here reports
//! success from a status code: `file` re-reads the packet it created,
//! and `patch` re-reads and FAILS unless every key it sent is now
//! actually on the packet.
//!
//! DELIBERATELY EXCLUDED: step completion (three verbs already own
//! their steps, and a generic step-writer would grade its own
//! homework), and the full-body job PUT — a read-modify-write replace
//! that has corrupted the system of record before. Not wrapped;
//! wrapping it would make it convenient.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// A full job id as the API expects it: 36 chars, dashed. Anything
/// else goes through list-and-match resolution.
pub(crate) fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Resolve a job reference against fetched rows, in `boss job`'s own
/// vocabulary. The matching semantics live in `prove::matching_jobs`
/// — shared, not copied — but the refusals speak about jobs of any
/// kind, because that is what this verb sees.
pub(crate) fn resolve<'a>(rows: &'a [Value], given: &str) -> Result<&'a Value> {
    let matches = crate::prove::matching_jobs(rows, given);
    match matches.len() {
        1 => Ok(matches[0]),
        0 => {
            if given.len() < 8 && !given.contains('/') {
                bail!(
                    "{given:?} is too short to resolve — give at least 8 characters \
                     of the id, the full uuid, or the branch exactly"
                );
            }
            bail!("no job matches {given:?} in the fetched rows")
        }
        n => {
            // LIST THEM, never pick one. And read the id and the name
            // through `envelope`, so a row that spells them `job_id` /
            // `job_title` is listed rather than rendered as `?  ?` —
            // which is a refusal the reader cannot act on.
            let mut listed = String::new();
            for m in &matches {
                listed.push_str(&format!(
                    "\n  {}  {}",
                    crate::envelope::job_id(m).unwrap_or("?"),
                    crate::envelope::job_title(m).unwrap_or("?")
                ));
            }
            bail!("{n} jobs match {given:?} — say which:{listed}")
        }
    }
}

/// The envelope `POST /api/jobs` actually requires, learned one 422 at
/// a time (f5dd5167). Explicit values win; everything else lands.
pub(crate) fn envelope(
    kind: &str,
    title: &str,
    priority: Option<&str>,
    subject_id: Option<&str>,
    owner_id: &str,
    today: &str,
    metadata: Option<Value>,
) -> Value {
    json!({
        "kind": kind,
        "title": title,
        "tags": [],
        "subject": {
            "id": subject_id.unwrap_or("bosspipeline"),
            "subject_kind": "custom",
        },
        "owner_id": owner_id,
        "opened_on": today,
        "status": "open",
        "priority": priority.unwrap_or("standard"),
        "metadata": metadata.unwrap_or_else(|| json!({})),
    })
}

/// One list line: 8-char id, kind, status, title — cut to `width` so a
/// narrow terminal shows one job per line instead of a wrapped mess.
pub(crate) fn list_line(row: &Value, width: usize) -> String {
    // Through `envelope`, so one definition of "where the id is" serves
    // every row shape this verb can be handed.
    let id = crate::envelope::job_id(row).unwrap_or("????????");
    let line = format!(
        "{}  {}  {}  {}",
        &id[..8.min(id.len())],
        row.get("kind").and_then(Value::as_str).unwrap_or("?"),
        row.get("status").and_then(Value::as_str).unwrap_or("?"),
        crate::envelope::job_title(row).unwrap_or("?")
    );
    fit(&line, width)
}

pub(crate) fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let cut: String = s.chars().take(width.saturating_sub(3)).collect();
    format!("{cut}...")
}

/// The packet, for a person, in a shape that is THIS VERB's contract
/// rather than the endpoint's envelope (backlog d44f6152).
///
/// It answers the three questions actually asked of a packet:
///
/// 1. WHAT KIND OF PACKET IS THIS — kind plus the protocol version it
///    is pinned to. The version is load-bearing: without it a step slug
///    cannot be resolved against the Workflow row this packet is
///    actually running, and in-flight packets stay pinned to the
///    version they were admitted under.
/// 2. WHICH STEP IS READY AND WHO OWNS IT — every step by `spec_slug`
///    (the stable handle; `title` is prose that gets reworded between
///    versions), its kind, its holder, and the `authority_role` that
///    says who may complete it. The ready/active one is marked.
/// 3. WHAT DOES IT LINK TO — one line per metadata key, value fitted.
///
/// THE METADATA IS KEYS-WITH-PREVIEWS, NOT A DUMP. A 4 KB `claim` used
/// to push the steps off the screen. Every key still appears, so an
/// absence here is a real absence, and the line naming `--json` says
/// where the untouched copy is — a reduction that hides where the full
/// record went is the defect CLAUDE.md §Diagnosis describes.
///
/// Reads every shape through [`crate::envelope`]: the id, the steps and
/// the receipt each have ONE definition there, so this renderer cannot
/// mis-key a row the way fifteen throwaway parsers did.
pub(crate) fn render_packet(job: &Value, width: usize) -> String {
    let g = |k: &str| job.get(k).and_then(Value::as_str).unwrap_or("-");
    let version = job
        .get("workflow_version")
        .and_then(Value::as_i64)
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    let mut out = format!(
        "{}\n{}{}   status {}   priority {}   opened {}\n{}\nowner {}   subject {}/{}\n",
        crate::envelope::job_id(job).unwrap_or("-"),
        g("kind"),
        version,
        g("status"),
        g("priority"),
        g("opened_on"),
        crate::envelope::job_title(job).unwrap_or("-"),
        g("owner_id"),
        job.pointer("/subject/subject_kind")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        job.pointer("/subject/id")
            .and_then(Value::as_str)
            .unwrap_or("-"),
    );

    let steps = crate::envelope::steps(job);
    if !steps.is_empty() {
        let lines: Vec<crate::envelope::StepLine> = steps
            .iter()
            .map(|s| crate::envelope::step_line(s))
            .collect();
        let sw = lines
            .iter()
            .map(|l| l.slug.chars().count())
            .max()
            .unwrap_or(0);
        let kw = lines
            .iter()
            .map(|l| l.kind.chars().count())
            .max()
            .unwrap_or(0);
        let stw = lines
            .iter()
            .map(|l| l.status.chars().count())
            .max()
            .unwrap_or(0);
        // The holder column is padded too, so the authority that follows
        // it lands in one column instead of sliding per row.
        let aw = lines
            .iter()
            .map(|l| l.assignee.as_deref().unwrap_or("—").chars().count())
            .max()
            .unwrap_or(0);
        out.push_str(&format!(
            "\nsteps ({})   * = where the packet is now\n",
            lines.len()
        ));
        for l in &lines {
            let authority = l
                .authority_role
                .as_deref()
                .map(|r| format!("  authority {r}"))
                .unwrap_or_default();
            let row = format!(
                "  {} {:stw$}  {:sw$}  {:kw$}  {:aw$}{}",
                if l.now { '*' } else { ' ' },
                l.status,
                l.slug,
                l.kind,
                l.assignee.as_deref().unwrap_or("—"),
                authority,
            );
            // `trim_end` before fitting: a step with no authority would
            // otherwise carry the holder column's padding as trailing
            // blanks, which diff tools and copy-paste both pick up.
            out.push_str(&fit(row.trim_end(), width));
            out.push('\n');
        }
    }

    // A gate receipt, when the packet carries one. A verdict must name
    // what failed (CLAUDE.md §Diagnosis), so a red lists its fails here
    // rather than sending the reader to re-derive them.
    if let Some((step, r)) = crate::envelope::receipt(job) {
        // The head gets its own line and is never shortened: a sha is
        // for copying, and a truncated one invites the hand-authored
        // half-sha that memory `never-write-a-sha-you-did-not-read` is
        // about. Everything else fits on one line.
        out.push_str(&format!(
            "\nreceipt (on \"{step}\")\n    {}   mode {}   {} check(s), {} fail(s)\n    head {}\n",
            r.verdict,
            r.mode,
            r.checks,
            r.fails.len(),
            r.head,
        ));
        for f in &r.fails {
            out.push_str(&fit(&format!("    {f}"), width));
            out.push('\n');
        }
    }

    let md = job.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let keys = crate::envelope::metadata_lines(&md, width.saturating_sub(4));
    out.push_str(&format!(
        "\nmetadata ({} key(s))   values fitted — --json prints the full copy\n",
        keys.len()
    ));
    for k in keys {
        out.push_str(&format!("    {k}\n"));
    }
    out
}

/// A station's queue: the station's own facts, then one line per packet.
///
/// WHY THE STATION'S FACTS COME FIRST. A queue depth alone does not say
/// whether the queue is healthy — the discipline that orders it and the
/// WIP limit that holds it are what make a number mean something, and
/// they are on the envelope already. And the page size is stated next to
/// the real `total`, because a truncated page answers a smaller question
/// without saying so (memory: `a-limit-is-not-a-filter`).
///
/// An EMPTY queue says so in words. A header with no rows under it reads
/// identically to a failed read, which is precisely the
/// indistinguishable-from-a-true-negative class this verb exists to end.
pub(crate) fn station_table(body: &Value, width: usize) -> String {
    let g = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("-");
    let total = body.get("total").and_then(Value::as_u64).unwrap_or(0);
    let rows = crate::gate::rows(Some(body.clone()));
    let discipline = body
        .get("discipline")
        .and_then(Value::as_array)
        .map(|d| {
            d.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "-".to_string());
    let wip = match body.get("wip_limit").and_then(Value::as_u64) {
        Some(n) => format!("{n}"),
        None => "none".to_string(),
    };
    let mut out = format!(
        "{}   {}   discipline {discipline}   wip {wip}{}\n{} packet(s) at the station, {} on this page\n",
        g("station"),
        g("kind"),
        if body.get("over_limit").and_then(Value::as_bool) == Some(true) {
            "   OVER LIMIT"
        } else {
            ""
        },
        total,
        rows.len(),
    );
    if rows.is_empty() {
        out.push_str("  (empty — the station holds nothing)\n");
        return out;
    }
    let kw = rows
        .iter()
        .map(|r| r.get("kind").and_then(Value::as_str).unwrap_or("?").len())
        .max()
        .unwrap_or(0);
    for r in &rows {
        // `id`, via `envelope` — a station row spells it `id` and an
        // assignments row `job_id`, and asking the wrong one reported a
        // filed packet as NOT VISIBLE on this very surface.
        let id = crate::envelope::job_id(r).unwrap_or("????????");
        let line = format!(
            "  {}  {:kw$}  {:8}  {}  {}",
            &id[..8.min(id.len())],
            r.get("kind").and_then(Value::as_str).unwrap_or("?"),
            r.get("priority").and_then(Value::as_str).unwrap_or("?"),
            r.get("opened_on").and_then(Value::as_str).unwrap_or("-"),
            crate::envelope::job_title(r).unwrap_or("?"),
        );
        out.push_str(&fit(&line, width));
        out.push('\n');
    }
    out
}

/// What the packet holds NOW for each key the patch sent — the whole
/// point of the verb. Returns the report and whether every key took;
/// a 204 that changed nothing must fail loudly, not print "patched".
pub(crate) fn confirm_patch(now: &Value, sent: &Value) -> (String, bool) {
    let mut out = String::new();
    let mut all_took = true;
    let empty = serde_json::Map::new();
    let sent_obj = sent.as_object().unwrap_or(&empty);
    for (k, v) in sent_obj {
        let current = now.get(k);
        if v.is_null() {
            match current {
                None => out.push_str(&format!("  {k}: removed\n")),
                Some(c) => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: sent null but the packet still holds {c}\n"
                    ));
                }
            }
        } else {
            match current {
                Some(c) if c == v => out.push_str(&format!("  {k}: {}\n", fit(&c.to_string(), 80))),
                Some(c) => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: wrote {} but the packet holds {}\n",
                        fit(&v.to_string(), 60),
                        fit(&c.to_string(), 60)
                    ));
                }
                None => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: wrote a value but the packet has no such key\n"
                    ));
                }
            }
        }
    }
    (out, all_took)
}

fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}

/// Fetch rows to resolve a reference against: open first (most lookups
/// are live work), closed only if nothing matched.
async fn fetch_and_resolve(http: &reqwest::Client, job_ref: &str) -> Result<String> {
    if looks_like_uuid(job_ref) {
        return Ok(job_ref.to_string());
    }
    for status in ["open", "closed"] {
        let rows = crate::gate::rows(
            crate::gate::api(
                http,
                reqwest::Method::GET,
                &format!("/api/jobs?status={status}&limit=500"),
                None,
            )
            .await?,
        );
        match resolve(&rows, job_ref) {
            Ok(row) => {
                return row
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .context("matched a job with no id");
            }
            Err(e) if e.to_string().starts_with("no job matches") => continue,
            Err(e) => return Err(e),
        }
    }
    bail!(
        "no job matches {job_ref:?} in the newest 500 open or 500 closed — \
         give the full uuid if it is older than that"
    )
}

pub async fn get(job_ref: &str, raw: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let id = fetch_and_resolve(&http, job_ref).await?;
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("the job read returned no body")?;
    if raw {
        println!("{}", serde_json::to_string_pretty(&job)?);
    } else {
        print!("{}", render_packet(&job, width()));
    }
    Ok(())
}

/// `boss job station <name>` — what is queued at a station.
///
/// `--json` prints the NORMALIZED rows rather than the raw body, which
/// is the opposite choice from `get --json` and deliberate: a machine
/// reading a station wants one spelling for the packet's id, and the
/// raw body is one `boss-api` call away if it wants the envelope.
pub async fn station(name: &str, raw: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let body = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/stations/{name}/queue"),
        None,
    )
    .await?
    .with_context(|| format!("the station read for {name:?} returned no body"))?;
    if raw {
        let packets: Vec<Value> = crate::gate::rows(Some(body.clone()))
            .iter()
            .map(|r| {
                json!({
                    "id": crate::envelope::job_id(r),
                    "kind": r.get("kind"),
                    "status": r.get("status"),
                    "priority": r.get("priority"),
                    "opened_on": r.get("opened_on"),
                    "owner_id": r.get("owner_id"),
                    "title": crate::envelope::job_title(r),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "station": body.get("station"),
                "kind": body.get("kind"),
                "discipline": body.get("discipline"),
                "wip_limit": body.get("wip_limit"),
                "over_limit": body.get("over_limit"),
                "total": body.get("total"),
                "on_this_page": packets.len(),
                "packets": packets,
            }))?
        );
    } else {
        print!("{}", station_table(&body, width()));
    }
    Ok(())
}

pub async fn list(kind: Option<String>, status: String, limit: u32) -> Result<()> {
    let http = reqwest::Client::new();
    let mut path = format!("/api/jobs?status={status}&limit={limit}");
    if let Some(k) = &kind {
        path.push_str(&format!("&kind={k}"));
    }
    let rows = crate::gate::rows(crate::gate::api(&http, reqwest::Method::GET, &path, None).await?);
    let w = width();
    for r in &rows {
        println!("{}", list_line(r, w));
    }
    println!("boss job: {} row(s)", rows.len());
    Ok(())
}

pub async fn file(
    kind: &str,
    title: &str,
    priority: Option<String>,
    metadata: Option<std::path::PathBuf>,
    subject_id: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();
    let md = match &metadata {
        Some(p) => Some(
            serde_json::from_str(
                &std::fs::read_to_string(p)
                    .with_context(|| format!("reading metadata {}", p.display()))?,
            )
            .with_context(|| format!("{} is not JSON", p.display()))?,
        ),
        None => None,
    };
    // The packet is owned by whoever filed it. This used to stamp the
    // train conductor's id on every hand-filed packet — the same
    // mis-attribution `completed_by` exposed on steps (backlog
    // 5083d6f5). Resolved BEFORE the POST so an unnamed caller is
    // refused with the fix rather than filing under automation.
    let owner = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    let body = envelope(
        kind,
        title,
        priority.as_deref(),
        subject_id.as_deref(),
        &owner,
        &now.format("%Y-%m-%d").to_string(),
        md,
    );
    let created = crate::gate::api(&http, reqwest::Method::POST, "/api/jobs", Some(body))
        .await?
        .context("the create returned no body")?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .context("the create returned no id — refusing to call that filed")?
        .to_string();

    // A 201 is a claim; the read-back is the fact.
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("created a job the API will not read back")?;
    println!(
        "boss job: filed {id}  \"{}\" — confirmed by reading it back",
        job.get("title").and_then(Value::as_str).unwrap_or("?")
    );
    Ok(())
}

pub async fn patch(job_ref: &str, path: &std::path::Path) -> Result<()> {
    let http = reqwest::Client::new();
    let sent: Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("{} is not JSON", path.display()))?;
    if !sent.is_object() {
        bail!("a metadata patch must be a JSON object of key -> value (null removes)");
    }
    let id = fetch_and_resolve(&http, job_ref).await?;
    crate::gate::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}/metadata"),
        Some(sent.clone()),
    )
    .await?;

    // The status code said yes; the packet is the authority.
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the patched job back")?;
    let now = job.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let (report, all_took) = confirm_patch(&now, &sent);
    print!("{report}");
    if !all_took {
        bail!(
            "the API answered 204 but the packet does not hold what was sent — \
             a silent no-op (write-once field, or a key the API ignores)"
        );
    }
    println!("boss job: {id} patched — confirmed by reading it back");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_defaults_land_and_explicit_values_win() {
        let e = envelope(
            "backlog-item",
            "t",
            None,
            None,
            "actor:x",
            "2026-08-30",
            None,
        );
        assert_eq!(e["tags"], json!([]));
        assert_eq!(e["subject"]["id"], "bosspipeline");
        assert_eq!(e["subject"]["subject_kind"], "custom");
        assert_eq!(e["owner_id"], "actor:x");
        assert_eq!(e["status"], "open");
        assert_eq!(e["priority"], "standard");
        assert_eq!(e["opened_on"], "2026-08-30");
        assert_eq!(e["metadata"], json!({}));

        let e = envelope(
            "k",
            "t",
            Some("urgent"),
            Some("boss-dev-0"),
            "a",
            "2026-08-30",
            Some(json!({"x": 1})),
        );
        assert_eq!(e["priority"], "urgent");
        assert_eq!(e["subject"]["id"], "boss-dev-0");
        assert_eq!(e["metadata"]["x"], 1);
    }

    #[test]
    fn a_short_prefix_is_refused_an_eight_char_one_resolves() {
        let rows = vec![json!({"id": "abcdef12-3456-7890-aaaa-bbbbccccdddd",
                               "title": "x", "metadata": {}})];
        assert!(resolve(&rows, "abcdef1").is_err());
        assert!(resolve(&rows, "abcdef12").is_ok());
        // A full uuid never reaches resolve — it goes straight to the API.
        assert!(looks_like_uuid("abcdef12-3456-7890-aaaa-bbbbccccdddd"));
        assert!(!looks_like_uuid("abcdef12"));
        assert!(!looks_like_uuid("abcdef12-3456-7890-aaaa-bbbbccccddd?"));
    }

    #[test]
    fn ambiguity_is_refused_not_chosen() {
        let rows = vec![
            json!({"id": "abcdef12-aaaa-1111-2222-333344445555", "title": "one", "metadata": {}}),
            json!({"id": "abcdef12-bbbb-1111-2222-333344445555", "title": "two", "metadata": {}}),
        ];
        let e = resolve(&rows, "abcdef12").unwrap_err().to_string();
        assert!(e.contains("2 jobs match"), "{e}");
        // A refusal that does not LIST the matches sends the reader back
        // to the API to find out what it was ambiguous between — and
        // picking one silently is worse still. Both, by id and title.
        assert!(e.contains("abcdef12-aaaa-1111-2222-333344445555"), "{e}");
        assert!(e.contains("abcdef12-bbbb-1111-2222-333344445555"), "{e}");
        assert!(e.contains("one") && e.contains("two"), "{e}");

        // A branch is the other way a packet is referred to in practice,
        // and two cars can share one id prefix OR one branch name. The
        // listing must read from whichever key the row carries its id
        // under, so an assignment row is listed rather than shown as
        // `?` (envelope::job_id is the one definition).
        let by_branch = vec![
            json!({"job_id": "11111111-aaaa-1111-2222-333344445555",
                   "job_title": "car one", "metadata": {"branch": "feat/x"}}),
            json!({"job_id": "22222222-bbbb-1111-2222-333344445555",
                   "job_title": "car two", "metadata": {"branch": "feat/x"}}),
        ];
        let e = resolve(&by_branch, "feat/x").unwrap_err().to_string();
        assert!(e.contains("2 jobs match"), "{e}");
        assert!(e.contains("11111111-aaaa-1111-2222-333344445555"), "{e}");
        assert!(e.contains("22222222-bbbb-1111-2222-333344445555"), "{e}");
        assert!(e.contains("car one") && e.contains("car two"), "{e}");
    }

    #[test]
    fn patch_confirmation_reports_what_the_packet_now_holds() {
        // Every key took: the happy path reads as a receipt.
        let (out, ok) = confirm_patch(
            &json!({"a": "x", "b": 2}),
            &json!({"a": "x", "b": 2, "gone": null}),
        );
        assert!(ok, "{out}");
        assert!(out.contains("gone: removed"));

        // The silent-204 case this verb exists for: a key the API
        // ignored is a FAILURE, named per key.
        let (out, ok) = confirm_patch(&json!({"a": "x"}), &json!({"a": "y", "new": 1}));
        assert!(!ok);
        assert!(out.contains("! a: wrote"), "{out}");
        assert!(out.contains("! new:"), "{out}");

        // Sent null but the key survived: also a failure.
        let (out, ok) = confirm_patch(&json!({"stuck": 1}), &json!({"stuck": null}));
        assert!(!ok);
        assert!(out.contains("still holds"), "{out}");
    }

    /// `GET /api/jobs/d44f6152-…` trimmed to three steps. Captured with
    /// `boss-api` on 2026-09-11.
    fn packet() -> Value {
        json!({
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
          "steps": [
            { "kind": "trigger", "title": "Filed to the backlog", "spec_slug": "filed",
              "assignee_id": null, "status": "completed", "metadata": {} },
            { "kind": "task", "title": "Measure the claim, choose a route",
              "spec_slug": "triage", "assignee_id": "claude@algedonic.dev",
              "status": "ready", "metadata": { "authority_role": "platform-admin" } },
            { "kind": "task", "title": "Re-measure the claim", "spec_slug": "measure",
              "assignee_id": null, "status": "pending",
              "metadata": { "authority_role": "platform-admin" } }
          ]
        })
    }

    #[test]
    fn the_render_answers_what_kind_which_step_is_ready_and_who_owns_it() {
        let out = render_packet(&packet(), 120);

        // WHAT KIND OF PACKET: the kind AND the protocol version it is
        // pinned to. Without the version a reader cannot resolve a step
        // slug against the Workflow row the packet is actually running.
        assert!(out.contains("backlog-item v5"), "{out}");
        assert!(out.contains("status open"), "{out}");
        assert!(out.contains("owner emp-david"), "{out}");
        assert!(out.contains("custom/bosspipeline"), "{out}");

        // WHICH STEP IS READY: marked, and the slug is the handle —
        // `title` is prose that gets reworded between versions.
        let now: Vec<&str> = out
            .lines()
            .filter(|l| l.trim_start().starts_with('*'))
            .collect();
        assert_eq!(now.len(), 1, "exactly one step is where it is: {out}");
        assert!(now[0].contains("triage"), "{out}");

        // WHO OWNS IT: the holder, and who is ALLOWED to complete it.
        assert!(now[0].contains("claude@algedonic.dev"), "{out}");
        assert!(now[0].contains("platform-admin"), "{out}");

        // Every step appears with its slug and kind, not just the ready
        // one — a reader asks "what comes next" as often as "what now".
        for slug in ["filed", "triage", "measure"] {
            assert!(out.contains(slug), "missing step {slug}: {out}");
        }
        assert!(out.contains("trigger"), "a step's kind rides too: {out}");
        assert!(out.contains("steps (3)"), "{out}");

        // Narrow terminals: every table row fits, so the step list stays
        // one packet-step per line rather than wrapping into a mess. The
        // TITLE line is content and is left whole — truncating what the
        // packet is about to make a column line up is the wrong trade.
        let narrow = render_packet(&packet(), 64);
        for line in narrow.lines().filter(|l| l.starts_with("  ")) {
            assert!(line.chars().count() <= 64, "{line:?} in:\n{narrow}");
        }
    }

    #[test]
    fn the_render_lists_metadata_keys_and_says_where_the_full_copy_is() {
        let out = render_packet(&packet(), 120);
        assert!(out.contains("metadata (2 key(s))"), "{out}");
        assert!(out.contains("channel"), "{out}");
        assert!(
            out.contains("a claim with a second line"),
            "a folded value, not a dropped second line: {out}"
        );
        // A reduction that does not say where the full copy is throws
        // away the only copy (CLAUDE.md §Diagnosis).
        assert!(out.contains("--json"), "{out}");
    }

    #[test]
    fn a_gate_run_renders_its_verdict_and_names_what_failed() {
        let red = json!({
          "id": "326f1532-d981-41dc-a080-0fcf6033e0b0",
          "kind": "gate-run", "workflow_version": 3, "status": "closed",
          "title": "Gate fix/x", "metadata": { "branch": "fix/x" },
          "steps": [{
            "spec_slug": "record-verdict", "kind": "gate-verdict",
            "title": "Record the receipt", "status": "completed",
            "metadata": { "receipt": "{\"verdict\":\"red\",\"mode\":\"full\",\"head\":\"abc1234\",\"checks\":[{\"name\":\"test\",\"result\":\"fail\",\"seconds\":90}],\"fails\":[\"test: one case panicked\"]}" }
          }]
        });
        let out = render_packet(&red, 120);
        assert!(out.contains("receipt"), "{out}");
        assert!(out.contains("red"), "{out}");
        assert!(out.contains("head abc1234"), "{out}");
        // A verdict someone must go re-derive is not a verdict.
        assert!(out.contains("test: one case panicked"), "{out}");
        // A packet with no receipt renders no receipt line at all.
        assert!(!render_packet(&packet(), 120).contains("receipt"));
    }

    #[test]
    fn the_station_table_names_the_stations_own_facts_and_one_row_per_packet() {
        // `GET /api/stations/q.platform-admin.task/queue`, trimmed to two
        // rows. Captured with `boss-api` on 2026-09-11: the rows are
        // packet envelopes keyed `id`, and they carry NO steps.
        let body = json!({
          "station": "q.platform-admin.task", "kind": "constraint",
          "discipline": ["priority", "age"], "wip_limit": null,
          "over_limit": false, "lens": null, "upstream": null, "total": 61,
          "data": [
            { "id": "ac356440-2aab-4d0d-af09-93a6f6488ba6",
              "kind": "rotate-a-credential", "status": "open", "priority": "urgent",
              "opened_on": "2026-09-03", "owner_id": "emp-david",
              "title": "Maiden rotation: the broker's first self-managed token (v2)",
              "metadata": {} },
            { "id": "290a5269-c026-4ea7-bd11-1410521aa391",
              "kind": "backlog-item", "status": "open", "priority": "urgent",
              "opened_on": "2026-09-10", "owner_id": "emp-david",
              "title": "ESTATE ALARM: disk_tight:w-1 persisted", "metadata": {} }
          ]
        });
        let out = station_table(&body, 120);

        // The station's own facts, which are the reason to ask a station
        // rather than list jobs: depth, discipline, and the WIP limit.
        assert!(out.contains("q.platform-admin.task"), "{out}");
        assert!(out.contains("constraint"), "{out}");
        assert!(out.contains("priority, age"), "{out}");

        // A LIMIT IS NOT A FILTER: the page shows 2 of 61 and must say
        // so, or the reader answers a smaller question without knowing.
        assert!(out.contains("61"), "the station's real depth: {out}");
        assert!(out.contains('2'), "and how many this page holds: {out}");

        // One line per packet, keyed `id` — the spelling that made a
        // filed design-doc read as NOT VISIBLE on its own station.
        assert!(out.contains("ac356440"), "{out}");
        assert!(out.contains("290a5269"), "{out}");
        assert!(out.contains("rotate-a-credential"), "{out}");
        assert!(out.lines().all(|l| l.chars().count() <= 120), "{out}");

        // An EMPTY queue says it is empty. A table with no rows under a
        // header reads identically to a failed read, which is the whole
        // defect class this verb exists for.
        let empty = json!({"station": "loading-dock", "kind": "batch",
                           "discipline": ["priority"], "total": 0, "data": []});
        assert!(
            station_table(&empty, 120).contains("empty"),
            "{}",
            station_table(&empty, 120)
        );
    }

    #[test]
    fn list_lines_cut_to_width_and_carry_the_short_id() {
        let row = json!({"id": "abcdef12-3456-7890-aaaa-bbbbccccdddd",
                         "kind": "backlog-item", "status": "open",
                         "title": "a very long title that will not fit in a narrow terminal at all"});
        let line = list_line(&row, 40);
        assert!(line.starts_with("abcdef12  backlog-item  open"), "{line}");
        assert_eq!(line.chars().count(), 40, "{line}");
        assert!(line.ends_with("..."));
        // Wide enough: untouched.
        assert!(!list_line(&row, 200).ends_with("..."));
    }
}
