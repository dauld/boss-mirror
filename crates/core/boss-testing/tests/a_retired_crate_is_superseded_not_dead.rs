//! `boss-cybernetics` left the tree as SUPERSEDED-BY, and the record
//! must say what superseded it (backlog 467175e7, design 8382bbb2).
//!
//! WHY THIS IS A TEST. The design's disposition was explicit that the
//! crate was right and was overtaken, not wrong — so its retirement is
//! only honest if the decision record names the owner of every
//! responsibility it held, and says the reason is coherence and one
//! owner per rule rather than the lines it removed. A note nobody
//! checks is a note the next edit to that section can quietly drop,
//! and then the question "where did the budget gate go?" is answered by
//! archaeology instead of by the record.
//!
//! The second half is the packet's own warning: "a retirement that
//! leaves a launcher pointing at a deleted service is a worse defect
//! than the one being fixed". The scripts that stop, start and validate
//! a fleet named the unit in their arrays; a systemctl call on a unit
//! that does not exist answers `inactive` rather than erroring
//! (CLAUDE.md §Doors), so nothing at run time would ever have said so.

use boss_testing::repo_root;
use std::path::Path;

const RETIRED: &str = "boss-cybernetics";

fn decision_record() -> String {
    std::fs::read_to_string(repo_root().join("docs/architecture-decisions.md"))
        .expect("the decision record is readable")
}

#[test]
fn the_crate_and_its_config_are_gone() {
    for path in ["crates/core/boss-cybernetics", "infra/cybernetics"] {
        assert!(
            !repo_root().join(path).exists(),
            "{path} was retired by 467175e7; its successors are named in \
             docs/architecture-decisions.md"
        );
    }
}

#[test]
fn the_record_names_what_superseded_each_responsibility() {
    let text = decision_record();
    let from = text
        .find("**`boss-cybernetics` is retired as SUPERSEDED-BY")
        .expect("the decision record carries the superseded-by note for boss-cybernetics");
    let note = &text[from..];
    let note = &note[..note.find("\n## ").unwrap_or(note.len())];
    // One successor per stated responsibility, each named by the thing
    // a reader can open — a file, a verb, a table, a workflow.
    for successor in [
        "agent_budget.rs",
        "`boss dispatch`",
        "`agent_runs`",
        "`agent-run` workflow",
        "**station**",
    ] {
        assert!(
            note.contains(successor),
            "the superseded-by note must name {successor} as a successor"
        );
    }
    assert!(
        note.contains("coherence and one owner\nper rule")
            || note.contains("coherence and one owner per rule"),
        "the note must give the reason as coherence and one owner per rule"
    );
    assert!(
        note.contains("**not**\na line-count win") || note.contains("**not** a line-count win"),
        "the note must say plainly that this is not a line-count win"
    );
}

/// Every non-comment line of a shell or config file under `dir`, with
/// its path, that names the retired unit.
fn live_mentions(dir: &Path, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            live_mentions(&path, hits);
            continue;
        }
        let is_runnable = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("sh" | "toml" | "yaml" | "yml" | "env" | "service")
        );
        if !is_runnable {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        text.lines()
            .enumerate()
            .filter(|(_, line)| !line.trim_start().starts_with('#'))
            .filter(|(_, line)| line.contains(RETIRED))
            .for_each(|(n, line)| {
                hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()))
            });
    }
}

#[test]
fn nothing_the_estate_runs_names_the_retired_unit() {
    let root = repo_root();
    let mut hits = Vec::new();
    live_mentions(&root.join("infra"), &mut hits);
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest");
    if workspace.contains(RETIRED) {
        hits.push("Cargo.toml names the retired crate".into());
    }
    assert!(
        hits.is_empty(),
        "these still name {RETIRED}, which no longer exists — a stop, start \
         or validate loop over it answers `inactive` instead of erroring:\n{}",
        hits.join("\n")
    );
}
