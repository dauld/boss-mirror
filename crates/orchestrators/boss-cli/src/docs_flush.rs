//! Markdown surgery for flushing design decisions into a design doc.
//!
//! Input: a markdown file + a list of decisions (one per anchor).
//! Output: the same markdown with the resolved questions removed
//! from `## Open questions` and corresponding entries appended to
//! `## Decisions`.
//!
//! Handles both anchor formats:
//! 1. Explicit `### Q1: <title>` subheadings
//! 2. Fallback `{slug}-open-N` positional ids referring to the Nth
//!    item in a numbered list inside the Open questions section
//!
//! Pure string/line ops — no markdown AST. The conventions we
//! actually use in docs/design/*.md are narrow enough that regex
//! + line scanning is simpler than a full parser.

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushDecision {
    pub anchor: String,
    pub kind: DecisionKind,
    pub resolution: String,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    Accept,
    Override,
}

impl DecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Override => "override",
        }
    }
}

/// Apply a set of decisions to a markdown file and return the new text.
///
/// Strategy:
/// 1. Split the file into lines.
/// 2. Find the bounds of `## Open questions`, `## Decisions` (if any),
///    and the `## ...` section that follows Open questions.
/// 3. For each decision, remove the corresponding question from
///    Open questions.
/// 4. Append the decision to Decisions (creating the section if
///    missing, right after Open questions).
pub fn apply_decisions(
    markdown: &str,
    decisions: &[FlushDecision],
    today: NaiveDate,
) -> Result<String> {
    if decisions.is_empty() {
        return Ok(markdown.to_string());
    }

    let lines: Vec<String> = markdown.lines().map(str::to_string).collect();

    let open_section = find_h2_section(&lines, "open questions")
        .ok_or_else(|| anyhow!("no `## Open questions` section found"))?;

    // Partition the file around the Open questions section.
    let mut before_open = lines[..open_section.start_line].to_vec();
    let open_header_line = lines[open_section.start_line].clone();
    let open_body = lines[open_section.start_line + 1..open_section.end_line].to_vec();
    let after_open = lines[open_section.end_line..].to_vec();

    // Remove resolved questions from the open section body.
    let already_resolved = already_resolved_anchors(&lines, &open_section);
    let (new_open_body, removed_titles, removed_bodies, list_fully_drained) =
        remove_decisions_from_open(&open_body, decisions, &already_resolved)?;

    // If the list was entirely drained (no numbered items remain),
    // replace the whole section body with a stable placeholder so
    // the Open questions section doesn't become a lonely intro
    // paragraph + stranded `---` separator. Same pass also promotes
    // the `**Status**:` line in the preamble to include "approved"
    // so the parser's `from_status_line` matches it and the
    // frontend's status chip reflects reality.
    let new_open_body = if list_fully_drained {
        promote_status_line_to_approved(&mut before_open, today);
        build_empty_section_placeholder(decisions.len(), today)
    } else {
        new_open_body
    };

    // Rebuild the file with the new Open questions body, but we still
    // need to inject or extend the Decisions section inside
    // `after_open`.
    let decisions_block =
        format_decisions_block(decisions, &removed_titles, &removed_bodies, today);
    let after_open_updated = merge_into_decisions(&after_open, &decisions_block)?;

    let mut out_lines = Vec::new();
    out_lines.extend(before_open);
    out_lines.push(open_header_line);
    out_lines.extend(new_open_body);
    out_lines.extend(after_open_updated);

    let mut out = out_lines.join("\n");
    // Preserve a trailing newline on the file.
    if markdown.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

struct H2Section {
    start_line: usize, // index of `## ...` line
    end_line: usize,   // exclusive — index of next `## ...` or lines.len()
}

fn find_h2_section(lines: &[String], heading_lower: &str) -> Option<H2Section> {
    let target = heading_lower.to_lowercase();
    let mut start_line = None;
    for (i, l) in lines.iter().enumerate() {
        let trimmed = l.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ")
            && rest.trim().to_lowercase() == target
        {
            start_line = Some(i);
            break;
        }
    }
    let start_line = start_line?;
    let mut end_line = lines.len();
    for (i, l) in lines.iter().enumerate().skip(start_line + 1) {
        let trimmed = l.trim_start();
        if trimmed.starts_with("## ") && !trimmed.starts_with("### ") {
            end_line = i;
            break;
        }
    }
    Some(H2Section {
        start_line,
        end_line,
    })
}

/// Walk the open-questions body and remove lines that belong to the
/// resolved questions. Returns the filtered body, a map from
/// anchor → extracted question title (used when formatting decisions
/// below so the D entry carries the question's original title),
/// and a flag indicating whether the numbered list in the section
/// was fully drained (i.e. the caller should replace the whole
/// section body with an empty-section placeholder).
/// A live `### Qn…` subheading — the explicit-anchor question form
/// the tracker convention mandates (CLAUDE.md §Design docs).
fn is_open_question_heading(line: &str) -> bool {
    let t = line.trim_start();
    let Some(rest) = t.strip_prefix("### Q") else {
        return false;
    };
    let digits: &str = rest.split([':', ' ']).next().unwrap_or("");
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Anchors the document already records as settled, anywhere in it.
///
/// Two forms count. A heading that kept its anchor and gained the
/// tag — `### Q1: … (resolved)` — and a heading that has left the
/// Open questions section altogether, which is what the flush itself
/// produces when it moves a question into Decision history.
///
/// This exists so the flusher can tell two failures apart. Both used
/// to read `explicit anchor `Q1` not found in Open questions`, and
/// they need opposite responses: a question that is already answered
/// means the job has nothing left to do, while a question that was
/// never there means the decision is pointed at the wrong document.
/// On 2026-08-16 two flush jobs for protocol-cadence.md sat `failed`
/// from 14:40Z, unnoticed, for exactly this reason — the work had
/// been done by an earlier car and the error could not say so
/// (5b8f274a).
fn already_resolved_anchors(
    lines: &[String],
    open: &H2Section,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        // Note `is_explicit_q_anchor` takes the ANCHOR, not the
        // heading — so the anchor has to come out of the line first.
        let Some(rest) = t.strip_prefix("### ") else {
            continue;
        };
        let Some(anchor) = rest.split([':', ' ']).next() else {
            continue;
        };
        if !is_explicit_q_anchor(anchor) {
            continue;
        }
        let inside_open = i > open.start_line && i < open.end_line;
        let tagged = t.to_ascii_lowercase().contains("(resolved)");
        if !inside_open || tagged {
            out.insert(anchor.to_string());
        }
    }
    out
}

fn remove_decisions_from_open(
    body: &[String],
    decisions: &[FlushDecision],
    already_resolved: &std::collections::BTreeSet<String>,
) -> Result<(
    Vec<String>,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, Vec<String>>,
    bool,
)> {
    let mut titles = std::collections::HashMap::new();
    let mut bodies: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    // Determine which anchors use the explicit ### Q<n>: format vs
    // the numbered-list fallback.
    let mut explicit: Vec<(String, usize)> = Vec::new(); // (anchor, decision_idx)
    let mut fallback: Vec<(usize, usize)> = Vec::new(); // (0-based item index, decision_idx)
    for (di, d) in decisions.iter().enumerate() {
        if is_explicit_q_anchor(&d.anchor) {
            explicit.push((d.anchor.clone(), di));
        } else if let Some(idx) = parse_fallback_index(&d.anchor) {
            fallback.push((idx, di));
        } else {
            return Err(anyhow!(
                "unknown anchor format `{}` — expected Q<n> or {{slug}}-open-<n>",
                d.anchor
            ));
        }
    }

    // Handle the explicit path: delete `### Q<n>:` subheadings +
    // their body until the next `### ` or the end of the section.
    let mut keep = vec![true; body.len()];
    for (anchor, _) in &explicit {
        let mut found = false;
        let mut i = 0;
        while i < body.len() {
            let trimmed = body[i].trim_start();
            if let Some(rest) = trimmed.strip_prefix("### ")
                && let Some(tail) = rest.strip_prefix(anchor)
                && (tail.starts_with(':') || tail.starts_with(' '))
            {
                let title = strip_trailing_status_tag(tail.trim_start_matches(':').trim());
                titles.insert(anchor.clone(), title);
                // Skip from this heading until the next ### or end.
                let mut j = i + 1;
                while j < body.len() {
                    let t = body[j].trim_start();
                    if t.starts_with("### ") {
                        break;
                    }
                    j += 1;
                }
                // Carry the question's body out before deleting it.
                // Without this the flush destroys what was PROPOSED and
                // keeps only what was answered, so a resolution reading
                // "Agreed" becomes unrecoverable — job e9cc393f, found
                // on event-kind-registry.md Q3/Q4, where the agreement
                // had to be reconstructed from a shipped schema
                // comment. The flush moves content; it does not consume
                // it.
                bodies.insert(anchor.clone(), body[i + 1..j].to_vec());
                keep[i..j].fill(false);
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            // Say which of the two this is. Only one of them wants a
            // human.
            if already_resolved.contains(anchor) {
                return Err(anyhow!(
                    "`{anchor}` is already resolved in this doc — nothing left to apply. \
                     This flush is a duplicate of one that already ran; no decision was lost."
                ));
            }
            return Err(anyhow!(
                "explicit anchor `{anchor}` not found in Open questions, and it is not \
                 recorded as resolved either — the decision may be pointed at the wrong doc"
            ));
        }
    }
    let body_after_explicit: Vec<String> = body
        .iter()
        .enumerate()
        .filter_map(|(i, l)| if keep[i] { Some(l.clone()) } else { None })
        .collect();

    // Handle the numbered-list fallback path: find the first numbered
    // list inside the section and remove the Nth item. Do it repeatedly
    // for each target, always re-parsing from the current state so
    // we work against the current index order.
    //
    // IMPORTANT: fallback indexes are frozen at reindex time. If the
    // caller gives us [0, 2, 4] they mean the 0th, 2nd, and 4th items
    // of the *original* list, not the current one. So we build a
    // single pass over the list and filter by original index.
    let mut fallback_indexes: Vec<usize> = fallback.iter().map(|(i, _)| *i).collect();
    fallback_indexes.sort();
    fallback_indexes.dedup();

    let (filtered, picked_titles, fallback_drained) = if fallback_indexes.is_empty() {
        // No fallback items touched — we can still check whether
        // the explicit-anchor pass left any numbered-list items.
        // It's very unlikely a doc mixes formats, but if it does
        // and the explicit pass happened to empty a list, we
        // still want the drained signal.
        let drained = !body_after_explicit.iter().any(|l| is_numbered_list_item(l));
        (body_after_explicit, Vec::new(), drained)
    } else {
        let (body, titles_vec, drained) =
            remove_numbered_items(&body_after_explicit, &fallback_indexes)?;
        (body, titles_vec, drained)
    };

    // Drained means DRAINED: no numbered items left AND no `### Qn`
    // headings left. The first version computed it from numbered
    // items alone, so on an explicit-anchor doc (every current design
    // doc) ANY partial flush read as "fully drained" and the
    // placeholder pass deleted the unresolved questions wholesale —
    // the 2026-08-12 live failure (1dd28e4c). A surviving question
    // heading is the definition of not-drained.
    let fallback_drained =
        fallback_drained && !filtered.iter().any(|l| is_open_question_heading(l));

    // Attach picked titles back to their anchors.
    for (pos, (_idx, di)) in fallback.iter().enumerate() {
        if let Some(t) = picked_titles.get(pos)
            && let Some(d) = decisions.get(*di)
        {
            titles.insert(d.anchor.clone(), strip_trailing_status_tag(t));
        }
    }

    Ok((filtered, titles, bodies, fallback_drained))
}

/// Rewrite the `**Status**:` preamble line to include "approved"
/// so the doc-tracker parser flips it from `in-review` to
/// `approved`. The parser's `from_status_line` does a case-
/// insensitive substring match; "approved" anywhere in the value
/// is enough to resolve the status correctly.
///
/// Bails out (leaves the line untouched) when the current status
/// is already an equally or more-graduated state — `approved`,
/// `shipped`, `reopened`, or `superseded`. A `reopened` doc that
/// just got its new questions resolved should stay `reopened`;
/// the author decides whether to promote it back to `shipped`.
/// A `shipped` doc shouldn't have had open questions (the
/// reindex validator blocks that), but guard anyway.
///
/// The worker preserves whatever prose was there — e.g. "design,
/// pre-implementation." — as a parenthetical hint so readers
/// still see the pre-code phase but the machine-readable status
/// is approved. Doc authors can override this by editing the
/// Status line by hand after flush.
fn promote_status_line_to_approved(before_open: &mut [String], today: NaiveDate) {
    const TERMINAL_MARKERS: &[&str] = &["approved", "shipped", "reopened", "superseded"];
    for line in before_open.iter_mut() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("**Status**:") {
            continue;
        }
        let lower = line.to_lowercase();
        if TERMINAL_MARKERS.iter().any(|m| lower.contains(m)) {
            return;
        }
        // Extract the existing prose (everything after the first `:`).
        let prose = line
            .split_once(':')
            .map(|(_, rest)| rest.trim().trim_end_matches('.').to_string())
            .unwrap_or_default();
        *line = if prose.is_empty() {
            format!("**Status**: approved ({today}).")
        } else {
            format!("**Status**: approved — {prose} ({today}).")
        };
        return;
    }
}

/// Strip one trailing parenthesized status tag (e.g. " (open)",
/// " (resolved)", " (pending)") from a question title so the
/// flush runner can safely re-append its own `(resolved)` marker
/// without creating `"... (open) (resolved)"` double-tags.
fn strip_trailing_status_tag(title: &str) -> String {
    const TAGS: &[&str] = &["(open)", "(resolved)", "(pending)", "(in-progress)"];
    let t = title.trim_end();
    for tag in TAGS {
        if let Some(without) = t.strip_suffix(tag) {
            return without.trim_end().to_string();
        }
    }
    t.to_string()
}

/// Body of the Open questions section after all items have been
/// flushed. Replaces the original intro + (now-empty) list with a
/// stable placeholder so the section doesn't decay into stranded
/// prose fragments. Matches the shape the design-doc-review doc
/// uses for its own Open questions section after its dogfood flush.
fn build_empty_section_placeholder(count: usize, today: NaiveDate) -> Vec<String> {
    let plural = if count == 1 {
        "question was"
    } else {
        "questions were"
    };
    vec![
        String::new(),
        format!("All {count} open {plural} resolved {today} via the in-app"),
        "decision tracker and flushed to git. See the Decisions".to_string(),
        "section below. This section is kept empty as the landing".to_string(),
        "place for any new questions that surface during".to_string(),
        "implementation.".to_string(),
        String::new(),
        "---".to_string(),
        String::new(),
    ]
}

fn is_explicit_q_anchor(anchor: &str) -> bool {
    let chars: Vec<char> = anchor.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    if !(chars[0] == 'Q' || chars[0] == 'q') {
        return false;
    }
    chars[1..].iter().all(|c| c.is_ascii_digit())
}

fn parse_fallback_index(anchor: &str) -> Option<usize> {
    // Format: `{slug}-open-<n>`. Split on `-open-` and parse the tail.
    let idx = anchor.rfind("-open-")?;
    let tail = &anchor[idx + "-open-".len()..];
    tail.parse::<usize>().ok()
}

/// Remove items at the given 0-based indexes from the first numbered
/// list found in the body. Returns (new_body, picked_titles,
/// list_fully_drained) where `list_fully_drained` is true when
/// every item in the original list got removed.
///
/// "Numbered list" here means consecutive lines starting with
/// `\d+\. `. Multi-line items are detected by indentation (any
/// following line that starts with whitespace is part of the
/// previous item).
fn remove_numbered_items(
    body: &[String],
    target_indexes: &[usize],
) -> Result<(Vec<String>, Vec<String>, bool)> {
    // Locate the first numbered list.
    let list_start = body
        .iter()
        .position(|l| is_numbered_list_item(l))
        .ok_or_else(|| anyhow!("no numbered list in Open questions section"))?;

    let mut list_end = list_start;
    while list_end < body.len() {
        let l = &body[list_end];
        if is_numbered_list_item(l) || (list_end > list_start && is_list_continuation(l)) {
            list_end += 1;
        } else if l.trim().is_empty()
            && list_end + 1 < body.len()
            && (is_numbered_list_item(&body[list_end + 1])
                || is_list_continuation(&body[list_end + 1]))
        {
            // Blank line in the middle of the list — keep going.
            list_end += 1;
        } else {
            break;
        }
    }

    // Parse the list into items: Vec<Vec<String>>, one per list item.
    let mut items: Vec<Vec<String>> = Vec::new();
    for l in &body[list_start..list_end] {
        if is_numbered_list_item(l) {
            items.push(vec![l.clone()]);
        } else if let Some(last) = items.last_mut() {
            last.push(l.clone());
        }
    }

    // Extract titles for the target indexes.
    let mut picked_titles = Vec::new();
    for &idx in target_indexes {
        let item = items
            .get(idx)
            .ok_or_else(|| anyhow!("list item index {idx} out of range (len={})", items.len()))?;
        let title = extract_item_title(item);
        picked_titles.push(title);
    }

    // Filter out the target items.
    let kept_items: Vec<Vec<String>> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| {
            if target_indexes.contains(&i) {
                None
            } else {
                Some(item)
            }
        })
        .collect();

    // Renumber the kept items: `1.`, `2.`, `3.`, ...
    let mut renumbered: Vec<Vec<String>> = Vec::new();
    for (new_idx, item) in kept_items.into_iter().enumerate() {
        let new_num = new_idx + 1;
        let mut new_item = item;
        // Replace the leading "<n>. " with "<new_num>. ".
        let line0 = &new_item[0];
        let after_num = line0
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches('.')
            .trim_start();
        new_item[0] = format!("{new_num}. {after_num}");
        renumbered.push(new_item);
    }

    // Rebuild the body: before the list, the new list, after the list.
    let mut out: Vec<String> = Vec::new();
    out.extend_from_slice(&body[..list_start]);
    for item in &renumbered {
        out.extend_from_slice(item);
    }
    if renumbered.is_empty() {
        // Ensure at least a blank line where the list used to be so
        // the rendered markdown doesn't collapse two paragraphs.
        out.push(String::new());
    }
    out.extend_from_slice(&body[list_end..]);

    let list_fully_drained = renumbered.is_empty();
    Ok((out, picked_titles, list_fully_drained))
}

fn is_numbered_list_item(line: &str) -> bool {
    let mut chars = line.chars();
    let mut saw_digit = false;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            saw_digit = true;
        } else if c == '.' {
            break;
        } else {
            return false;
        }
    }
    saw_digit && chars.next() == Some(' ')
}

fn is_list_continuation(line: &str) -> bool {
    line.starts_with("   ") || line.starts_with("\t")
}

/// Extract the question title from a numbered list item that may
/// span multiple lines. Handles three common shapes:
///
/// 1. `1. **Bolded question?** body text here.` — return the
///    content between the opening `**` and its matching closing
///    `**`, regardless of what special chars appear inside
///    (backticks for inline code, periods inside URLs, etc.).
/// 2. `1. **Bolded question that` / `   wraps across lines?** body` —
///    the bolded section spans multiple lines; we join first and
///    then scan.
/// 3. `1. Plain prose question.` — no bold block; take everything
///    up to the first sentence terminator that isn't inside an
///    inline-code span.
///
/// Sentence terminators inside inline code (`` `schema.sql` ``) do
/// not truncate the title, and a bolded title may span lines.
fn extract_item_title(item: &[String]) -> String {
    if item.is_empty() {
        return String::new();
    }
    // Join the item body into one string (collapse continuation
    // indentation + inter-line whitespace into single spaces). We
    // only scan within a reasonable budget so we don't walk a
    // giant body — the title always lives at the start.
    let mut joined = String::new();
    for (i, line) in item.iter().enumerate() {
        if i > 0 {
            joined.push(' ');
        }
        joined.push_str(line.trim_start());
        // Stop after ~500 chars of body; the title is always near
        // the top.
        if joined.len() > 500 {
            break;
        }
    }

    // Strip the leading "N. " prefix.
    let after_num = joined
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches('.')
        .trim_start();

    // Path 1: starts with `**` — find the matching close.
    // Fall through if the input is malformed (no closing `**`).
    if let Some(rest) = after_num.strip_prefix("**")
        && let Some(end) = find_closing_bold(rest)
    {
        return rest[..end].trim().to_string();
    }

    // Path 2: take everything up to the first sentence terminator
    // that isn't inside an inline-code span. Code spans are bounded
    // by matching backticks.
    first_sentence_outside_code(after_num)
}

/// Scan forward looking for a closing `**` that's not inside an
/// inline code span. Returns the byte offset of the first `*` of
/// the closing delimiter.
fn find_closing_bold(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i + 1 < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && c == b'*' && bytes[i + 1] == b'*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Walk forward through the string and return the substring up to
/// (but not including) the first `.`, `?`, or `!` that's outside of
/// any inline code span (delimited by backticks). Used as the
/// fallback title for plain-prose questions.
fn first_sentence_outside_code(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && (c == b'.' || c == b'?' || c == b'!') {
            return s[..i].trim().to_string();
        }
        i += 1;
    }
    s.trim().to_string()
}

/// Build the block of markdown to append to the Decisions section.
fn format_decisions_block(
    decisions: &[FlushDecision],
    titles: &std::collections::HashMap<String, String>,
    bodies: &std::collections::HashMap<String, Vec<String>>,
    today: NaiveDate,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in decisions {
        let title = titles
            .get(&d.anchor)
            .cloned()
            .unwrap_or_else(|| d.anchor.clone());
        // Heading: use the anchor (or the question title) so the
        // decision is clearly linked to the question it resolves.
        out.push(String::new());
        out.push(format!("### {}: {} (resolved)", d.anchor, title));
        out.push(String::new());
        out.push(format!("Resolved {} — {}.", today, d.kind.as_str()));
        // The question as it stood, carried over rather than deleted.
        // A resolution of "Agreed" is meaningless without the thing
        // agreed to, and the Open questions section it used to live in
        // is about to lose it (e9cc393f).
        if let Some(body) = bodies.get(&d.anchor) {
            let trimmed: Vec<&String> = body
                .iter()
                .skip_while(|l| l.trim().is_empty())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .skip_while(|l| l.trim().is_empty())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !trimmed.is_empty() {
                out.push(String::new());
                out.push("**The question was:**".to_string());
                out.push(String::new());
                for l in trimmed {
                    out.push(if l.trim().is_empty() {
                        String::from(">")
                    } else {
                        format!("> {l}")
                    });
                }
            }
        }
        out.push(String::new());
        out.push(d.resolution.clone());
        if let Some(rationale) = &d.rationale {
            out.push(String::new());
            out.push(format!("**Rationale:** {rationale}"));
        }
        out.push(String::new());
    }
    out
}

/// Find `## Decisions` in the `after_open` slice (or create one
/// right at the top of it) and append the new entries.
fn merge_into_decisions(after_open: &[String], new_block: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = after_open.to_vec();

    // Find an existing `## Decisions` section.
    let mut existing_start: Option<usize> = None;
    for (i, l) in out.iter().enumerate() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("## ")
            && rest.trim().to_lowercase().starts_with("decisions")
        {
            existing_start = Some(i);
            break;
        }
    }

    if let Some(start) = existing_start {
        // Find end of the Decisions section (next ## or end).
        let mut end = out.len();
        for (i, l) in out.iter().enumerate().skip(start + 1) {
            let t = l.trim_start();
            if t.starts_with("## ") && !t.starts_with("### ") {
                end = i;
                break;
            }
        }
        // Insert the new block just before `end` (before the next H2
        // or at EOF).
        // If the last kept line in the section is blank, insert above it.
        let insert_at = trim_trailing_blank_lines(&out, start + 1, end);
        for (offset, line) in new_block.iter().enumerate() {
            out.insert(insert_at + offset, line.clone());
        }
    } else {
        // Create a new `## Decisions` section at the top of after_open
        // (i.e. right after the Open questions section).
        let mut insertion: Vec<String> = Vec::new();
        insertion.push(String::new());
        insertion.push("## Decisions".to_string());
        insertion.extend_from_slice(new_block);
        // Prepend the insertion.
        let mut merged = Vec::with_capacity(insertion.len() + out.len());
        merged.extend(insertion);
        merged.extend(out);
        out = merged;
    }

    Ok(out)
}

/// Return the index in [start, end) where the tail of blank lines begins.
fn trim_trailing_blank_lines(lines: &[String], start: usize, end: usize) -> usize {
    let mut i = end;
    while i > start && lines[i - 1].trim().is_empty() {
        i -= 1;
    }
    i
}

// ---------------------------------------------------------------------------

/// Placeholder — used in tests that don't care about the date.
#[cfg(test)]
pub fn test_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 4, 13).unwrap()
}

pub fn today() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

pub fn locate_file(repo_root: &std::path::Path, rel_path: &str) -> std::path::PathBuf {
    repo_root.join(rel_path)
}

pub fn load_markdown(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

pub fn save_markdown(path: &std::path::Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(anchor: &str, resolution: &str) -> FlushDecision {
        FlushDecision {
            anchor: anchor.to_string(),
            kind: DecisionKind::Accept,
            resolution: resolution.to_string(),
            rationale: None,
        }
    }

    /// Extract the content of the first `## <heading>` section.
    fn section_content<'a>(md: &'a str, heading: &str) -> &'a str {
        let marker = format!("## {heading}");
        let start = md.find(&marker).unwrap_or(md.len());
        let rest = &md[start..];
        // Skip the heading line itself.
        let after_heading = rest.find('\n').map(|i| start + i + 1).unwrap_or(md.len());
        let tail = &md[after_heading..];
        let next_h2 = tail
            .find("\n## ")
            .map(|i| after_heading + i)
            .unwrap_or(md.len());
        &md[after_heading..next_h2]
    }

    #[test]
    fn the_question_body_survives_a_terse_resolution() {
        // Regression for e9cc393f. doc-flatten #1 found
        // event-kind-registry.md Q3/Q4 resolved as bare "Agreed" and
        // "Sounds good" with the question bodies already deleted, so
        // what was agreed TO had to be reconstructed from a shipped
        // schema comment. A flush must MOVE content, not consume it.
        let md = r#"# Design: Foo

**Status**: in-review.

## Open questions

### Q1: Where does X live?

**Proposal**: a new crate named boss-x, because the alternative
puts a domain concept in the substrate.

### Q2: Untouched?

Still open.

## Next section

Other content.
"#;
        let out = apply_decisions(
            md,
            &[FlushDecision {
                anchor: "Q1".into(),
                kind: DecisionKind::Accept,
                resolution: "Agreed".into(),
                rationale: None,
            }],
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
        )
        .unwrap();

        // The terse resolution is preserved...
        assert!(out.contains("Agreed"), "resolution missing:\n{out}");
        // ...and so is the proposal it was agreeing to. This is the
        // assertion the original code could not satisfy.
        assert!(
            out.contains("a new crate named boss-x"),
            "the question body was destroyed by the flush:\n{out}"
        );
        assert!(
            out.contains("**The question was:**"),
            "carried body is not labelled:\n{out}"
        );
        // The untouched question stays in Open questions.
        assert!(
            out.contains("### Q2: Untouched?"),
            "unresolved question lost:\n{out}"
        );
    }

    #[test]
    fn removes_explicit_anchors_and_appends_decisions() {
        let md = r#"# Design: Foo

**Status**: in-review.

## Goal

Do the thing.

## Open questions

Each has a proposal attached.

### Q1: Where does X live?

Prose about X.

**Proposal**: a new crate.

### Q2: Async or sync?

Prose about Y.

**Proposal**: async.

## Next section

Other content.
"#;
        let decisions = vec![accept("Q1", "a new crate"), accept("Q2", "async")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let open_section = section_content(&out, "Open questions");
        assert!(
            !open_section.contains("### Q1:"),
            "Q1 should be removed from Open questions, got: {open_section:?}"
        );
        assert!(
            !open_section.contains("### Q2:"),
            "Q2 should be removed from Open questions"
        );

        let decisions_section = section_content(&out, "Decisions");
        assert!(decisions_section.contains("Q1"));
        assert!(decisions_section.contains("Q2"));
        assert!(decisions_section.contains("a new crate"));
        assert!(decisions_section.contains("async"));
        assert!(decisions_section.contains("Resolved 2026-04-13"));

        // Sections still in order.
        let goal_idx = out.find("## Goal").unwrap();
        let open_idx = out.find("## Open questions").unwrap();
        let dec_idx = out.find("## Decisions").unwrap();
        let next_idx = out.find("## Next section").unwrap();
        assert!(goal_idx < open_idx);
        assert!(open_idx < dec_idx);
        assert!(dec_idx < next_idx);
    }

    #[test]
    fn a_partial_flush_leaves_the_unresolved_questions_standing() {
        // The 2026-08-12 live failure (feedback 1dd28e4c): flushing
        // [Q1] on a doc whose questions are `### Qn` headings read as
        // "fully drained" — the drained check counted numbered-list
        // items, of which such docs have none — and replaced the whole
        // section body, deleting Q2's text with no resolution
        // recorded. Every follow-up flush then failed "anchor not
        // found". A partial flush must be partial.
        let md = r#"# Design: Foo

**Status**: in-review.

## Open questions

### Q1: Where does X live?

**Proposal**: a new crate.

### Q2: Async or sync?

Prose about Y.

## Next section
"#;
        let decisions = vec![accept("Q1", "a new crate")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let open_section = section_content(&out, "Open questions");
        assert!(!open_section.contains("### Q1:"), "Q1 resolved away");
        assert!(
            open_section.contains("### Q2:") && open_section.contains("Prose about Y"),
            "unresolved Q2 must survive a partial flush, got: {open_section:?}"
        );
        assert!(
            !out.contains("approved"),
            "a partially-open doc must not be status-promoted"
        );

        // And the second flush completes the drain cleanly.
        let out2 = apply_decisions(&out, &[accept("Q2", "async")], test_date()).unwrap();
        let open2 = section_content(&out2, "Open questions");
        assert!(!open2.contains("### Q2:"));
        assert!(section_content(&out2, "Decisions").contains("async"));
    }

    #[test]
    fn removes_fallback_numbered_items() {
        let md = r#"# Design: Bar

**Status**: in-review

## Open questions

These are unresolved.

1. **Where does X live?** Some discussion. **Proposal**: new crate.
2. **Should Y be sync?** More discussion. **Proposal**: async.
3. **What about Z?** Edge case. **Proposal**: defer.

## Next
"#;
        let decisions = vec![
            accept("bar-open-0", "new crate"),
            accept("bar-open-2", "defer"),
        ];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let open_section = section_content(&out, "Open questions");
        // Items 0 and 2 should be gone from Open questions.
        assert!(
            !open_section.contains("Where does X live"),
            "item 0 should be removed from Open questions, got: {open_section:?}"
        );
        assert!(
            open_section.contains("Should Y be sync"),
            "item 1 should remain in Open questions"
        );
        assert!(
            !open_section.contains("What about Z"),
            "item 2 should be removed from Open questions"
        );
        // Remaining item should be renumbered to 1.
        assert!(
            open_section.contains("1. **Should Y be sync"),
            "remaining item should be renumbered to 1"
        );

        let decisions_section = section_content(&out, "Decisions");
        assert!(decisions_section.contains("new crate"));
        assert!(decisions_section.contains("defer"));
    }

    #[test]
    fn appends_to_existing_decisions_section() {
        let md = r#"# Design: Baz

## Open questions

1. **Q1?** Body. **Proposal**: yes.

## Decisions

### D0: Prior decision (resolved)

Already decided earlier.

## Other
"#;
        let decisions = vec![accept("baz-open-0", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        assert!(out.contains("D0: Prior decision"));
        assert!(out.contains("baz-open-0"));
        // The new entry should be before `## Other`.
        let d0_idx = out.find("D0:").unwrap();
        let new_idx = out.find("baz-open-0").unwrap();
        let other_idx = out.find("## Other").unwrap();
        assert!(d0_idx < new_idx);
        assert!(new_idx < other_idx);
    }

    #[test]
    fn missing_open_questions_is_error() {
        let md = "# Foo\n\n**Status**: approved\n\n## Goal\n\nDone.\n";
        let decisions = vec![accept("Q1", "whatever")];
        let result = apply_decisions(md, &decisions, test_date());
        assert!(result.is_err());
    }

    #[test]
    fn empty_decisions_is_noop() {
        let md = "# Foo\n\n## Open questions\n\n1. **Q?** Body. **Proposal**: a.\n";
        let out = apply_decisions(md, &[], test_date()).unwrap();
        assert_eq!(out, md);
    }

    #[test]
    fn preserves_trailing_newline() {
        let md = "# Foo\n\n## Open questions\n\n1. **Q?** Body. **Proposal**: a.\n\n## Next\n";
        let decisions = vec![accept("foo-open-0", "a")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn rationale_appears_in_decisions_block() {
        let md = "# Foo\n\n## Open questions\n\n1. **Q?** Body. **Proposal**: a.\n\n## Next\n";
        let decisions = vec![FlushDecision {
            anchor: "foo-open-0".to_string(),
            kind: DecisionKind::Override,
            resolution: "b".to_string(),
            rationale: Some("because b is better".to_string()),
        }];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        assert!(out.contains("because b is better"));
        assert!(out.contains("override"));
    }

    #[test]
    fn explicit_anchor_detection() {
        assert!(is_explicit_q_anchor("Q1"));
        assert!(is_explicit_q_anchor("Q42"));
        assert!(!is_explicit_q_anchor("simulator-ui-open-0"));
        assert!(!is_explicit_q_anchor("Q"));
        assert!(!is_explicit_q_anchor("QA"));
    }

    #[test]
    fn fallback_index_parsing() {
        assert_eq!(parse_fallback_index("simulator-ui-open-0"), Some(0));
        assert_eq!(parse_fallback_index("foo-open-42"), Some(42));
        assert_eq!(parse_fallback_index("Q1"), None);
        assert_eq!(parse_fallback_index("not-valid"), None);
    }

    /// Regression test from the simulator-ui flush dogfood. The old
    /// title extractor stopped at the first `**` OR `.` — which
    /// truncated questions containing inline code like
    /// `` `schema.sql` `` mid-sentence. Every one of these titles
    /// should come through whole.
    #[test]
    fn title_extraction_handles_simulator_ui_fixture_shapes() {
        // Classic bolded question, single line.
        let item = vec![
            "1. **Where does boss-sim-api live?** New `crates/boss-sim-api` crate...".to_string(),
        ];
        assert_eq!(extract_item_title(&item), "Where does boss-sim-api live?");

        // Bolded question containing inline code — this is the bug
        // case. The old extractor would stop at the `.` inside
        // `schema.sql`.
        let item = vec![
            "4. **What happens if the scratch DB is out of sync with `schema.sql`".to_string(),
            "   (e.g. after pulling new migrations)?** The reset endpoint should".to_string(),
        ];
        assert_eq!(
            extract_item_title(&item),
            "What happens if the scratch DB is out of sync with `schema.sql` (e.g. after pulling new migrations)?"
        );

        // Multi-clause question that spans two lines in the bolded
        // section.
        let item = vec![
            "2. **Does the scratch stack run all 7 services, or only the ones".to_string(),
            "   the sim writes to?** The sim writes to fleet, people...".to_string(),
        ];
        assert_eq!(
            extract_item_title(&item),
            "Does the scratch stack run all 7 services, or only the ones the sim writes to?"
        );

        // Bolded question with an embedded path (slashes + dot).
        let item = vec!["5. **Who can hit `/api/sim`?** Sim UI is an operator tool...".to_string()];
        assert_eq!(extract_item_title(&item), "Who can hit `/api/sim`?");
    }

    #[test]
    fn title_extraction_fallback_to_prose_sentence() {
        // No bold block at all — should take the first sentence
        // outside code spans.
        let item = vec!["1. Plain prose question? with body text.".to_string()];
        assert_eq!(extract_item_title(&item), "Plain prose question");

        // Periods inside inline code should NOT terminate the
        // sentence — this is the backtick-aware path.
        let item = vec!["1. Scan `foo.bar.baz` for issues? body.".to_string()];
        assert_eq!(extract_item_title(&item), "Scan `foo.bar.baz` for issues");
    }

    #[test]
    fn title_extraction_empty_item_is_empty_string() {
        let item: Vec<String> = vec![];
        assert_eq!(extract_item_title(&item), "");
    }

    #[test]
    fn draining_the_list_replaces_section_with_placeholder() {
        let md = r#"# Design: Bar

## Open questions

These need decisions.

1. **Q1?** Body. **Proposal**: a.
2. **Q2?** Body. **Proposal**: b.

## Next
"#;
        let decisions = vec![accept("bar-open-0", "a"), accept("bar-open-1", "b")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let open_section = section_content(&out, "Open questions");
        // The intro paragraph should be gone.
        assert!(
            !open_section.contains("These need decisions"),
            "the original intro should be replaced: {open_section:?}"
        );
        // The placeholder language should be present.
        assert!(
            open_section.contains("All 2 open questions were resolved"),
            "should see the placeholder: {open_section:?}"
        );
        assert!(
            open_section.contains("decision tracker"),
            "placeholder should mention the tracker: {open_section:?}"
        );
        // Numbered list items should be gone.
        assert!(!open_section.contains("1. **Q1"));
        assert!(!open_section.contains("2. **Q2"));
    }

    #[test]
    fn partial_drain_keeps_original_intro() {
        let md = r#"# Design: Bar

## Open questions

These need decisions.

1. **Q1?** Body. **Proposal**: a.
2. **Q2?** Body. **Proposal**: b.
3. **Q3?** Body. **Proposal**: c.

## Next
"#;
        // Only remove 1 of 3.
        let decisions = vec![accept("bar-open-1", "b")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let open_section = section_content(&out, "Open questions");
        // Intro preserved.
        assert!(open_section.contains("These need decisions"));
        // Remaining items stay and get renumbered.
        assert!(open_section.contains("1. **Q1"));
        assert!(open_section.contains("2. **Q3"));
        // The removed item is gone.
        assert!(!open_section.contains("**Q2?**"));
    }

    #[test]
    fn flush_drain_promotes_status_line_to_approved() {
        let md = r#"# Design: Foo

**Status**: design, pre-implementation.
**Owner**: TBD.

## Open questions

1. **Q1?** Body. **Proposal**: yes.
2. **Q2?** Body. **Proposal**: no.

## Next
"#;
        let decisions = vec![accept("foo-open-0", "yes"), accept("foo-open-1", "no")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        assert!(
            out.contains("**Status**: approved"),
            "status line should include 'approved' after drain: {out:?}"
        );
        assert!(
            out.contains("design, pre-implementation"),
            "original prose should be preserved as parenthetical"
        );
        // Owner line untouched.
        assert!(out.contains("**Owner**: TBD"));
    }

    #[test]
    fn flush_drain_leaves_already_approved_status_alone() {
        let md = r#"# Design: Foo

**Status**: approved — design, pre-implementation.

## Open questions

1. **Q1?** Body. **Proposal**: yes.

## Next
"#;
        let decisions = vec![accept("foo-open-0", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        // Exact string preserved — no double "approved" suffix.
        let count = out.matches("approved").count();
        assert!(count >= 1, "should still contain 'approved'");
        assert!(
            !out.contains("approved — approved"),
            "should not double-wrap an already-approved status"
        );
    }

    #[test]
    fn partial_drain_leaves_status_unchanged() {
        let md = r#"# Design: Foo

**Status**: design, pre-implementation.

## Open questions

1. **Q1?** Body. **Proposal**: yes.
2. **Q2?** Body. **Proposal**: no.

## Next
"#;
        // Only 1 of 2.
        let decisions = vec![accept("foo-open-0", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        // Still has open questions → status should NOT be promoted.
        assert!(
            !out.contains("**Status**: approved"),
            "partial drain should leave status as-is"
        );
        assert!(out.contains("**Status**: design, pre-implementation"));
    }

    #[test]
    fn singular_placeholder_for_one_question() {
        let md = r#"# Bar

## Open questions

One question.

1. **Only question?** Body. **Proposal**: yes.

## Next
"#;
        let decisions = vec![accept("bar-open-0", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();
        let open_section = section_content(&out, "Open questions");
        assert!(
            open_section.contains("All 1 open question was resolved"),
            "singular form: {open_section:?}"
        );
    }

    #[test]
    fn title_extraction_malformed_unclosed_bold_falls_back() {
        // Opens `**` but never closes — fallback path kicks in.
        let item = vec!["1. **Unclosed bold. No terminator.".to_string()];
        // The fallback walks past the opening ** to find a sentence
        // terminator. We accept either "the whole line minus **" or
        // "up to the first .".
        let out = extract_item_title(&item);
        assert!(out.contains("Unclosed bold"), "got: {out:?}");
    }

    #[test]
    fn strips_open_marker_from_heading_before_appending_resolved() {
        // When the author authored "### Q1: ... (open)" the flush
        // should produce "### Q1: ... (resolved)" — not the double
        // "### Q1: ... (open) (resolved)" we used to emit.
        let md = r#"# Foo

**Status**: in-review.

## Open questions

### Q1: Does the flush strip existing markers? (open)

Body. **Proposal**: yes.
"#;
        let decisions = vec![accept("Q1", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let dec = section_content(&out, "Decisions");
        assert!(
            dec.contains("### Q1: Does the flush strip existing markers? (resolved)"),
            "decisions heading should have a single `(resolved)` tag, got: {dec:?}"
        );
        assert!(
            !dec.contains("(open) (resolved)"),
            "must not double-tag: {dec:?}"
        );
    }

    #[test]
    fn flush_on_reopened_doc_preserves_status() {
        // A reopened doc gets its new questions resolved — the
        // status should stay `reopened`, not get prefixed with
        // `approved — reopened — ...`. The author decides when to
        // promote back to shipped.
        let md = r#"# Foo

**Status**: reopened — original scope shipped 2026-04-18; new proposal pending.

## Open questions

### Q8: New question? (open)

Body. **Proposal**: yes.
"#;
        let decisions = vec![accept("Q8", "yes")];
        let out = apply_decisions(md, &decisions, test_date()).unwrap();

        let status_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("**Status**:"))
            .expect("status line missing");
        assert!(
            status_line.to_lowercase().contains("reopened"),
            "reopened status must survive the flush, got: {status_line:?}"
        );
        assert!(
            !status_line.to_lowercase().contains("approved"),
            "flush must not inject `approved` on top of `reopened`, got: {status_line:?}"
        );
    }

    #[test]
    fn strip_trailing_status_tag_handles_common_cases() {
        assert_eq!(strip_trailing_status_tag("Title here (open)"), "Title here");
        assert_eq!(
            strip_trailing_status_tag("Title here (resolved)"),
            "Title here"
        );
        assert_eq!(
            strip_trailing_status_tag("Title here (pending)"),
            "Title here"
        );
        assert_eq!(strip_trailing_status_tag("Title here"), "Title here");
        // Only one tag gets stripped — if authors pile them up, that's
        // their problem to clean.
        assert_eq!(
            strip_trailing_status_tag("Title (open) (resolved)"),
            "Title (open)"
        );
    }
}

#[cfg(test)]
mod already_resolved_tests {
    use super::{DecisionKind, FlushDecision, apply_decisions};
    use chrono::NaiveDate;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 16).unwrap()
    }

    fn decide(anchor: &str) -> Vec<FlushDecision> {
        vec![FlushDecision {
            anchor: anchor.to_string(),
            kind: DecisionKind::Accept,
            resolution: "yes".into(),
            rationale: None,
        }]
    }

    // The 2026-08-16 case: an earlier car already flushed Q1, so the
    // question now lives under Decisions with the tag. Two flush jobs
    // sat `failed` for hours because this said only "not found".
    #[test]
    fn a_question_already_answered_says_so() {
        let md = "# D\n\n## Open questions\n\n### Q2: still open\n\nbody\n\n\
                  ## Decisions\n\n### Q1: the one already done (resolved)\n\nyes\n";
        let e = apply_decisions(md, &decide("Q1"), day())
            .unwrap_err()
            .to_string();
        assert!(e.contains("already resolved"), "{e}");
        assert!(e.contains("no decision was lost"), "{e}");
    }

    // The other half, which must stay loud: this decision is pointed
    // at a document that never had the question.
    #[test]
    fn a_question_that_was_never_here_says_that_instead() {
        let md = "# D\n\n## Open questions\n\n### Q2: still open\n\nbody\n";
        let e = apply_decisions(md, &decide("Q7"), day())
            .unwrap_err()
            .to_string();
        assert!(e.contains("not recorded as resolved"), "{e}");
        assert!(!e.contains("already resolved in this doc"), "{e}");
    }

    // And the happy path still works — the guard must not swallow a
    // legitimate flush.
    #[test]
    fn an_open_question_still_flushes() {
        let md =
            "# D\n\n## Open questions\n\n### Q1: open\n\nbody\n\n## Decisions\n\n_None yet._\n";
        let out = apply_decisions(md, &decide("Q1"), day()).expect("flushes");
        assert!(out.contains("Q1"), "{out}");
        assert_ne!(out, md);
    }
}
