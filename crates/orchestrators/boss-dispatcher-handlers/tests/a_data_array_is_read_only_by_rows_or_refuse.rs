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
//! What it does NOT judge, on purpose: `.get("data").cloned()
//! .unwrap_or(job)` — the single-object envelope unwrap, which falls
//! back to the whole body rather than to an empty list, and is a
//! different question.

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

/// Every array read of `data` in `text`, by 1-based line, excluding the
/// one inside [`THE_READER`].
fn offending_lines(text: &str) -> Vec<usize> {
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
        let chain: String = text[from..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(CHAIN_WINDOW)
            .collect();
        if chain.contains("as_array") || chain.contains("is_array") {
            out.push(text[..at].matches('\n').count() + 1);
        }
    }
    out
}

#[test]
fn a_data_array_is_read_only_by_rows_or_refuse() {
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
    let offenders: Vec<String> = files
        .iter()
        .flat_map(|f| {
            let text = std::fs::read_to_string(f).expect("read source");
            let rel = f.strip_prefix(&src).unwrap_or(f).display().to_string();
            offending_lines(&text)
                .into_iter()
                .map(move |line| format!("src/{rel}:{line}"))
        })
        .collect();
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
