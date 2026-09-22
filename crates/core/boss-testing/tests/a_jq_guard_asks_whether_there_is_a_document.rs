//! `infra/lib/jq.sh` is RUN, not read — under `sh`, against the four
//! input shapes a guard can be handed, so every property below is one
//! the helper actually has.
//!
//! THE CLASS (2026-09-21, backlog d96e38ab). On jq-1.6 — the version on
//! the pod, the forge and boss-gcp — `jq -e <any filter>` over an input
//! carrying NO document exits **0**. Over malformed content it exits 4,
//! correctly. So the one input that means "I got no answer at all" is
//! the one input every `jq -e` guard in the tree read as a pass.
//!
//! Measured on this pod's jq-1.6:
//!
//! ```text
//!   jq -e . < /dev/null                      -> 0
//!   jq -e 'type == "object"' < /dev/null     -> 0
//!   jq -e 'type == "object"' < "   \n"       -> 0     <- -s does NOT catch this
//!   jq -e . < "not json at all"              -> 4
//!   printf '{"a":1}' | jq -e 'empty'         -> 4
//! ```
//!
//! The last two lines are the mechanism: jq's exit code is RIGHT
//! whenever it read a document, and jq-1.6 simply never sets it when
//! there was none. So the missing question is not "is the filter true"
//! — jq answers that — it is "was there anything to ask it about".
//!
//! WHY `[ -s "$f" ]` IS NOT THE FIX, and this is the correction to the
//! shape the packet proposed: a whitespace-only file has size > 0, so
//! `-s` passes, and `jq -e` still exits 0 on it. `-s` also cannot be
//! asked of a value already in a shell variable, which is half the call
//! sites. The one thing true of all three silent inputs — zero-byte,
//! whitespace-only, malformed — is that jq PRODUCES NO OUTPUT for any
//! of them, so that is what the helper reads.
//!
//! WHY A HELPER AND NOT A WRAPPER AROUND `jq -e`: the call sites carry
//! multi-line filters, `--arg`, `--argjson` and `-r`, and one writes
//! jq's stdout to a file. A wrapper would have to re-order every one of
//! those, and `-r` would defeat an output test. `jq_doc_file` answers
//! the one question jq gets wrong and leaves each site's own `jq -e`
//! verbatim beside it (CLAUDE.md §9a — one definition, not sixteen
//! correct copies).

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/lib/jq.sh";

fn lib() -> PathBuf {
    repo_root().join(LIB)
}

/// Source the real library under `sh` — dash on the forge and boss-gcp,
/// and `infra/ops/verbs-allowlist.sh` carries `#!/bin/sh`, so the helper
/// has to parse there — and run one guard call. Returns its exit status.
fn ask(script: &str) -> i32 {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(". \"$1\"\n{script}\n"))
        .arg("sh")
        .arg(lib())
        .output()
        .unwrap_or_else(|e| panic!("run sh: {e}"));
    assert!(
        out.stderr.is_empty(),
        "{LIB} wrote to stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.status.code().unwrap_or(-1)
}

/// The four input shapes, written to files in one scratch dir.
fn inputs(tag: &str) -> PathBuf {
    let dir = scratch::scratch_dir(&format!("a-jq-guard-{tag}"));
    scratch::write_file(&dir.join("zero.json"), "");
    scratch::write_file(&dir.join("blank.json"), "   \n\t\n");
    scratch::write_file(&dir.join("doc.json"), "{\"a\":1}");
    scratch::write_file(&dir.join("bad.json"), "not json at all");
    dir
}

/// A ZERO-BYTE file is not a document. This is the shape a 204, a
/// killed process, a truncated write and a redirect that never ran all
/// leave behind — the one the forge's publish path met on the live
/// drift check (b88a13d5, ops-request 9340fd6e).
#[test]
fn a_zero_byte_file_holds_no_document() {
    let d = inputs("zero");
    assert_eq!(
        ask(&format!("jq_doc_file {}/zero.json", d.display())),
        1,
        "a zero-byte file must not answer 'there is a document'"
    );
}

/// A WHITESPACE-ONLY file is not a document either — and `[ -s ]`, the
/// guard this class's first repair reached for, says it is.
#[test]
fn a_whitespace_only_file_holds_no_document() {
    let d = inputs("blank");
    assert_eq!(
        ask(&format!("jq_doc_file {}/blank.json", d.display())),
        1,
        "a blank file has size > 0 and still holds no document"
    );
}

/// Malformed content is not a document. jq already exits 4 here, so
/// this leg is not the defect — it is the one that must not regress
/// when the emptiness question is added.
#[test]
fn malformed_content_holds_no_document() {
    let d = inputs("bad");
    assert_eq!(
        ask(&format!("jq_doc_file {}/bad.json", d.display())),
        1,
        "unparseable content must not answer 'there is a document'"
    );
}

/// An absent file is not a document, and the helper fails CLOSED rather
/// than letting jq's own diagnostic decide: a guard that cannot read is
/// a guard that refuses (CLAUDE.md §Doors — a wrong target answers
/// instead of erroring).
#[test]
fn an_absent_file_holds_no_document() {
    let d = inputs("absent");
    assert_eq!(
        ask(&format!("jq_doc_file {}/nothing-here.json", d.display())),
        1,
        "an absent file must not answer 'there is a document'"
    );
}

/// A real document answers yes — otherwise the guard would refuse every
/// honest input and be turned off within the week.
#[test]
fn a_real_document_answers_yes() {
    let d = inputs("doc");
    assert_eq!(
        ask(&format!("jq_doc_file {}/doc.json", d.display())),
        0,
        "a parseable document must answer 'there is a document'"
    );
}

/// `null` IS a document. The distinction matters: `jq_doc_file` answers
/// "was there anything to ask about", and the site's own `jq -e .` then
/// correctly exits 1 on a null. Folding the two questions together
/// would make a guard unable to tell "the server said null" from "the
/// server said nothing", which is the whole point of the split.
#[test]
fn a_null_document_is_still_a_document() {
    let d = inputs("null");
    scratch::write_file(&d.join("null.json"), "null\n");
    assert_eq!(
        ask(&format!("jq_doc_file {}/null.json", d.display())),
        0,
        "null is a document the server sent; absence is not"
    );
}

/// The text form, for a value already in a shell variable — the half of
/// the call sites `[ -s ]` cannot be asked about at all.
#[test]
fn the_text_form_answers_the_same_four_ways() {
    assert_eq!(ask("jq_doc_text ''"), 1, "the empty string is no document");
    assert_eq!(
        ask("jq_doc_text '   '"),
        1,
        "a whitespace-only body is no document"
    );
    assert_eq!(
        ask("jq_doc_text 'not json at all'"),
        1,
        "unparseable text is no document"
    );
    assert_eq!(
        ask("jq_doc_text '{\"a\":1}'"),
        0,
        "a parseable body is a document"
    );
    assert_eq!(
        ask("jq_doc_text"),
        1,
        "a missing argument is no document, not an unbound-variable crash"
    );
}

/// THE ROSTER. Every `jq -e` under `infra/` must ask the emptiness
/// question first — a `jq_doc_file` / `jq_doc_text` in the same
/// condition, or in the few lines above it, which is where a refusal
/// block sits when the guard gets its own message.
///
/// THE PACKET'S OPEN QUESTION — whether to refuse an unguarded `jq -e`
/// at all — was "measure the false-positive rate on the 16 before
/// deciding", and here is the measurement: of the 17 `jq -e` lines this
/// sweep found under `infra/`, **every one** could be handed an empty
/// input, and none needed an exemption. A rate of 0 is what makes the
/// strict rule affordable; the argument against — that a check firing
/// on the safe ones trains people to skip it — turns out to have no
/// instances to fire on. So there is no waiver hatch here: an escape
/// with no user is code kept just in case. The day a site genuinely
/// cannot receive silence, `jq_doc_*` in front of it is still true, one
/// line, and cheaper than arguing.
///
/// WHY HERE AND NOT `infra/lint/`: the gate runs this crate's suite
/// anyway, so the roster costs no new car on the pre-flight consist.
#[test]
fn every_jq_e_under_infra_asks_whether_there_is_a_document() {
    // A guard and the call it protects are one thought. The window is
    // wide enough for the guard to refuse in its own words first — the
    // forge's publish path and boss-gcp's both take five lines to say
    // why they are refusing — and narrow enough that the association is
    // real rather than "somewhere in the file".
    const WINDOW: usize = 8;
    let mut unguarded: Vec<String> = Vec::new();
    let mut guarded = 0usize;

    for path in shell_files(&repo_root().join("infra")) {
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // A comment ABOUT the rule is not a call site.
            if !line.contains("jq -e") || line.trim_start().starts_with('#') {
                continue;
            }
            let near = lines[i.saturating_sub(WINDOW)..=i].join("\n");
            if near.contains("jq_doc_file") || near.contains("jq_doc_text") {
                guarded += 1;
            } else {
                let rel = path.strip_prefix(repo_root()).unwrap_or(&path).to_owned();
                unguarded.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        unguarded.is_empty(),
        "a `jq -e` that can be handed silence reads it as a pass — on jq-1.6 an input \
         carrying no document exits 0, so the guard verifies nothing (backlog d96e38ab). \
         Source {LIB} and ask `jq_doc_file <file>` or `jq_doc_text <value>` first:\n  {}",
        unguarded.join("\n  ")
    );
    // A FLOOR, not the count: a scan that found nothing would pass the
    // assertion above while certifying an empty tree (the shape
    // `a_lint_that_scanned_nothing_is_red` is named for). An exact
    // number would be a contended integer every new site has to bump,
    // which is the defect 07e72962 collapsed out of the ratchet — and
    // it would buy nothing, because a removed guard shows up above.
    assert!(
        guarded >= 10,
        "only {guarded} guarded `jq -e` lines found under infra/ — the sweep left 17, so \
         this scan is reading the wrong tree rather than certifying it"
    );
}

/// Every `*.sh` under a directory, recursively.
fn shell_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(shell_files(&p));
        } else if p.extension().is_some_and(|x| x == "sh") {
            out.push(p);
        }
    }
    out.sort();
    out
}
