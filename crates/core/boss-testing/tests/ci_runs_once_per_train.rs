//! CI runs on the pull request and NOT again on the push to main.
//!
//! Measured 2026-09-12 off the forge's task list (backlog 47fc2bc1):
//! every train cost two full CI runs — the PR's, and a second one on
//! the squash commit the merge put on main ("train: merge …" runs 679,
//! 681). The jobs run serially on one runner slot, ~17 min a run, so
//! the second run held the slot while the NEXT train's PR waited: train
//! 18:51's CI began at 19:30Z. Nothing reads the second run — the
//! conductor's verdict is the PR head's combined status
//! (`/commits/{head_sha}/status`), the cluster image is built by the
//! forge converge, not by CI — and the squash commit carries the PR
//! head's tree, because the track lock keeps main still between
//! assembly and merge. It also built a second 3.48 GB `boss-ci:<sha>`
//! image per train into the disk the floor sweep defends (05db904e).
//!
//! So the workflow's trigger is `pull_request` alone. This pins it: a
//! `push:` trigger reappearing is the duplicate coming back.

use std::path::Path;

fn ci_yml() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.forgejo/workflows/ci.yml");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The `on:` block's top-level trigger keys, read the way the runner
/// does: the lines indented exactly two spaces between `on:` and the
/// next top-level key.
fn trigger_keys(yml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_on = false;
    for line in yml.lines() {
        if line.starts_with("on:") {
            in_on = true;
            continue;
        }
        if in_on {
            if !line.starts_with(' ') && !line.trim().is_empty() && !line.starts_with('#') {
                break;
            }
            if let Some(rest) = line.strip_prefix("  ")
                && !rest.starts_with(' ')
                && !rest.starts_with('#')
                && let Some((k, _)) = rest.split_once(':')
            {
                keys.push(k.trim().to_string());
            }
        }
    }
    keys
}

#[test]
fn ci_is_triggered_by_the_pull_request_alone() {
    let keys = trigger_keys(&ci_yml());
    assert_eq!(
        keys,
        vec!["pull_request".to_string()],
        "ci.yml `on:` triggers — a `push` trigger runs the whole suite a second time on the \
         squash commit, on the one runner slot, for a result nobody reads; got {keys:?}"
    );
}

#[test]
fn the_trigger_reader_sees_a_push_trigger_when_there_is_one() {
    let yml = "name: CI\n\non:\n  push:\n    branches: [main]\n  pull_request:\n    branches: [main]\n\njobs:\n  x: {}\n";
    assert_eq!(trigger_keys(yml), vec!["push", "pull_request"]);
}
