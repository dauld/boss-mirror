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
            let mut listed = String::new();
            for m in &matches {
                listed.push_str(&format!(
                    "\n  {}  {}",
                    m.get("id").and_then(Value::as_str).unwrap_or("?"),
                    m.get("title").and_then(Value::as_str).unwrap_or("?")
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
    let id = row.get("id").and_then(Value::as_str).unwrap_or("????????");
    let line = format!(
        "{}  {}  {}  {}",
        &id[..8.min(id.len())],
        row.get("kind").and_then(Value::as_str).unwrap_or("?"),
        row.get("status").and_then(Value::as_str).unwrap_or("?"),
        row.get("title").and_then(Value::as_str).unwrap_or("?")
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

/// The packet, for a person: envelope, steps with who holds them,
/// metadata in full. `--json` skips this and prints the body.
pub(crate) fn render_job(job: &Value) -> String {
    let g = |k: &str| job.get(k).and_then(Value::as_str).unwrap_or("-");
    let mut out = format!(
        "{}  {}\n{}\nstatus: {}   priority: {}   opened: {}\n",
        g("id"),
        g("kind"),
        g("title"),
        g("status"),
        g("priority"),
        g("opened_on"),
    );
    if let Some(steps) = job.get("steps").and_then(Value::as_array) {
        out.push_str("steps:\n");
        for s in steps {
            let sg = |k: &str| s.get(k).and_then(Value::as_str);
            out.push_str(&format!(
                "  [{}] {}{}\n",
                sg("status").unwrap_or("?"),
                sg("title").unwrap_or("?"),
                sg("assignee_id")
                    .map(|a| format!("  <- {a}"))
                    .unwrap_or_default()
            ));
        }
    }
    if let Some(md) = job.get("metadata") {
        out.push_str("metadata:\n");
        out.push_str(&serde_json::to_string_pretty(md).unwrap_or_default());
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
        print!("{}", render_job(&job));
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
    let body = envelope(
        kind,
        title,
        priority.as_deref(),
        subject_id.as_deref(),
        crate::train::actor_id(),
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
