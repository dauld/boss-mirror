//! A listing's `data` array is read in ONE place in this crate:
//! `handlers::common::rows_or_refuse`. Every other spelling of that read
//! is refused here, by file and line.
//!
//! WHY THIS IS A TEST AND NOT A HABIT (backlog d4698bc2). The chain
//! `.get("data") … .as_array() … .unwrap_or_default()` turns an ERROR
//! SHAPE — a dark, narrowed or restarted far side answering 200 with no
//! rows array — into an empty list, so the pass reads "nothing to do"
//! and reports healthy. It was cured one handler at a time, each found
//! by accident by someone working on something else: retro.open
//! (80a77466), sensor.poll twice (6c4c432a, 0767c830), five handlers in
//! 37fc5837, and the last twelve sites in d4698bc2 — among them a
//! census that ended its paging early on a missing array and a retro
//! whose "no packet this week" read would have opened a twin. A grep
//! sweep found them, and a grep sweep is exactly what cannot keep them
//! found: the next copy is written by someone who has never seen this.
//!
//! So the rule is mechanical and has NO exceptions to remember: any
//! `.get("data")` whose next move reads the value as an array
//! (`as_array`, `is_array`) outside `rows_or_refuse` fails this test.
//! A read that is deliberately empty on error still goes through
//! `rows_or_refuse` and states its fallback at the call site, where a
//! reviewer can see the choice being made.
//!
//! THE SINGLE-ROW NEIGHBOUR (backlog f2eac973). `.get("data").cloned()
//! .unwrap_or(job)` falls back to the whole body rather than to an
//! empty list, and it was a different question until it was measured:
//! every single-row door these handlers read answers the row BARE
//! (`GET /api/jobs/{id}` is a flattened `JobDetail`, `GET
//! /api/credentials/{id}` a bare `CredentialRow`), so the envelope it
//! hedged for is never sent and the fallback was the only live path —
//! any 200 body read as the row, and a kind check skipped it without a
//! word. So the second rule, as mechanical as the first: any
//! `.get("data")` whose chain falls back with `unwrap_or` fails this
//! test, and a single row is read through `common::row_or_refuse`,
//! which refuses a body that carries no row `id`.

use std::path::{Path, PathBuf};

/// The one function allowed to read `data` as an array.
const THE_READER: &str = "fn rows_or_refuse";

/// How far past `.get("data")` (whitespace removed) the array read may
/// sit and still be the same chain. Every spelling in the tree puts it
/// in the very next call — `.and_then(Value::as_array)`,
/// `.and_then(|v| v.as_array())`, `?.as_array()?`,
/// `.filter(|d| d.is_array())` — and forty characters holds each.
const CHAIN_WINDOW: usize = 40;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `.get("data")` in `text` whose next chain (whitespace removed,
/// [`CHAIN_WINDOW`] characters) satisfies `judged`, by 1-based line,
/// excluding any inside [`THE_READER`].
fn lines_where(text: &str, judged: impl Fn(&str) -> bool) -> Vec<usize> {
    // The reader's own body: from its signature to the first line that
    // closes a top-level item.
    let reader = text.find(THE_READER).map(|start| {
        let end = text[start..]
            .find("\n}\n")
            .map_or(text.len(), |e| start + e);
        start..end
    });
    let needle = ".get(\"data\")";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(needle) {
        let at = from + i;
        from = at + needle.len();
        if reader.as_ref().is_some_and(|r| r.contains(&at)) {
            continue;
        }
        // A mention in a comment is not a read: the doc comments that
        // explain WHY these rules exist quote the very chains they
        // forbid, and a rule that refused its own explanation would be
        // cured by deleting the explanation.
        let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
        if text[line_start..at].trim_start().starts_with("//") {
            continue;
        }
        let chain: String = text[from..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(CHAIN_WINDOW)
            .collect();
        if judged(&chain) {
            out.push(text[..at].matches('\n').count() + 1);
        }
    }
    out
}

/// Every array read of `data` in `text`.
fn offending_lines(text: &str) -> Vec<usize> {
    lines_where(text, |chain| {
        chain.contains("as_array") || chain.contains("is_array")
    })
}

/// Every single-row envelope unwrap in `text`: a `.get("data")` that
/// falls back — to the body, to a borrow of it, to anything — rather
/// than refusing. Each `unwrap_or` variant is a guess about what the
/// body was, so the rule names them all; an array read that also ends
/// in `unwrap_or_default` is reported by both rules, on the same line.
fn envelope_fallback_lines(text: &str) -> Vec<usize> {
    lines_where(text, |chain| chain.contains(".unwrap_or"))
}

/// Every `src/**/*.rs` offence `detect` finds, as `src/<file>:<line>`.
fn offences(detect: fn(&str) -> Vec<usize>) -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(
        files.len() > 10,
        "the walk found {} files under {} — a walk that finds nothing passes everything",
        files.len(),
        src.display()
    );
    files
        .iter()
        .flat_map(|f| {
            let text = std::fs::read_to_string(f).expect("read source");
            let rel = f.strip_prefix(&src).unwrap_or(f).display().to_string();
            detect(&text)
                .into_iter()
                .map(move |line| format!("src/{rel}:{line}"))
        })
        .collect()
}

#[test]
fn a_data_array_is_read_only_by_rows_or_refuse() {
    let offenders = offences(offending_lines);
    assert!(
        offenders.is_empty(),
        "a listing's `data` array is read outside common::rows_or_refuse — a missing \
         array would read as zero rows (backlog d4698bc2). Route each through \
         rows_or_refuse, stating any deliberate fallback at the call site:\n  {}",
        offenders.join("\n  ")
    );
}

/// The detector itself, on the spellings it must catch and the ones it
/// must leave alone — a lint that matches nothing passes every tree.
#[test]
fn the_detector_catches_every_spelling_and_spares_the_reader() {
    let caught = [
        "let r = v.get(\"data\")\n    .and_then(Value::as_array)\n    .cloned()\n    .unwrap_or_default();",
        "v.get(\"data\").and_then(|v| v.as_array()).map(Vec::len).unwrap_or(0)",
        "listing.get(\"data\")?.as_array()?.first()",
        "v.get(\"data\").filter(|d| d.is_array()).ok_or_else(|| x)?",
    ];
    for text in caught {
        assert_eq!(offending_lines(text), vec![1], "must refuse: {text}");
    }
    let spared = [
        "let job = job.get(\"data\").cloned().unwrap_or(job);",
        ".or_else(|| created.get(\"data\").and_then(|d| d.get(\"id\")))",
        "fn rows_or_refuse() {\n    listing.get(\"data\").filter(|d| d.is_array())\n}\n",
    ];
    for text in spared {
        assert!(offending_lines(text).is_empty(), "must spare: {text}");
    }
    assert_eq!(
        offending_lines("a\nb\nx.get(\"data\")\n.and_then(Json::as_array)"),
        vec![3],
        "the line reported is the line of the read"
    );
}

#[test]
fn a_single_row_is_read_only_by_row_or_refuse() {
    let offenders = offences(envelope_fallback_lines);
    assert!(
        offenders.is_empty(),
        "a single-row read unwraps a `data` envelope and falls back to the body — the doors \
         these handlers read answer the row bare, so the fallback reads ANY 200 body as the \
         row (backlog f2eac973). Read it through common::row_or_refuse:\n  {}",
        offenders.join("\n  ")
    );
}

/// The second detector on the spellings the tree held and on the reads
/// that are not unwraps — a POST's minted-id fallback and a k8s
/// Secret's own `data` map read a KEY; neither stands in for a row.
#[test]
fn the_envelope_detector_catches_every_fallback_and_spares_key_reads() {
    let caught = [
        "let job = job.get(\"data\").cloned().unwrap_or(job);",
        "Ok(job.get(\"data\").cloned().unwrap_or(job))",
        "let run = run.get(\"data\").unwrap_or(&run);",
        "let row = row\n    .get(\"data\")\n    .cloned()\n    .unwrap_or_else(|| row.clone());",
    ];
    for text in caught {
        assert_eq!(
            envelope_fallback_lines(text).len(),
            1,
            "must refuse: {text}"
        );
    }
    let spared = [
        ".or_else(|| created.get(\"data\").and_then(|d| d.get(\"id\")))",
        "let Some(b64) = body\n    .get(\"data\")\n    .and_then(|d| d.get(key))\n    .and_then(|v| v.as_str())\nelse {",
        "let job = row_or_refuse(job, \"GET /api/jobs/x\")?;",
        "/// the old `.get(\"data\").cloned().unwrap_or(job)` hedged for an envelope",
    ];
    for text in spared {
        assert!(
            envelope_fallback_lines(text).is_empty(),
            "must spare: {text}"
        );
    }
}
