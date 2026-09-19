//! The mirror's CodeQL scan runs from a config file, and every query it
//! excludes is a recorded decision with its evidence beside it (backlog
//! 3ac9ad30, David 2026-09-19).
//!
//! WHAT WAS MEASURED. Publish PR #239 on the public mirror stalled on
//! CodeQL for the second time in a row: `.github/workflows/codeql.yml`
//! ran the default query suites with no config, and the check reported
//! 100 annotations, all of one shape — a NAME heuristic, not a finding.
//! `rust/cleartext-logging` fired 81 times on any value called
//! `account_id`, `current_uid(...)`, `accounts` or `employee` written to
//! a log; `rust/hard-coded-cryptographic-value` fired 9 times on
//! `#[cfg(test)]` key and nonce constants; `rust/uncontrolled-allocation-
//! size` twice on `with_capacity(seed.tax_kind.len())`; and the four
//! `rust/cleartext-transmission` hits were the same names in a URL to a
//! LAN service. The real residue (a path-injection pair, one request-
//! forgery, one sanitizer in a test file) was invisible under it.
//!
//! THE RULE. An exclusion is a subtraction from what the scan can see,
//! and a subtraction nobody can audit is a silent quieting. So each
//! `- exclude:` in `.github/codeql/codeql-config.yml` carries a comment
//! block naming the PR whose annotations were read and the count that
//! was read — "81 of 100 on #239" — and this test refuses one that does
//! not. The companion readback item makes the remaining alerts visible
//! per publish, so an exclusion that hides a real class shows up as a
//! real finding arriving late, and the count here is what it is
//! compared against.
//!
//! THE FACT THAT LIVES TWICE (CLAUDE.md §9a). The workflow must name
//! the config for it to apply at all — an unreferenced config file is a
//! recorded decision the scan never reads — and the workflow must stay
//! SHA-pinned (scorecard's Pinned-Dependencies check). Both are read
//! here from the workflow, not asserted from memory.

use boss_testing::repo_root;
use regex::Regex;

const CONFIG: &str = ".github/codeql/codeql-config.yml";
const WORKFLOW: &str = ".github/workflows/codeql.yml";

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} is readable: {e}", p.display()))
}

/// One recorded exclusion: the query id and the comment block written
/// directly above it (contiguous `#` lines, no blank line between).
struct Exclusion {
    id: String,
    why: String,
}

/// Reads the `query-filters:` section as CodeQL's own shape:
///
/// ```yaml
/// query-filters:
///   # why …
///   - exclude:
///       id: rust/cleartext-logging
/// ```
///
/// The comment block is collected line by line and attached to the next
/// `- exclude:`; a blank line resets it, so a comment that drifts away
/// from its exclusion stops counting as its why. A differently-shaped
/// entry (`include`, an `id` outside an `exclude`) is a loud failure.
fn exclusions(config: &str) -> Vec<Exclusion> {
    let section = config
        .split_once("\nquery-filters:")
        .unwrap_or_else(|| panic!("{CONFIG} has no top-level `query-filters:` section"))
        .1;
    let mut out = Vec::new();
    let mut why: Vec<String> = Vec::new();
    let mut pending: Option<Vec<String>> = None;
    for raw in section.lines() {
        let line = raw.trim();
        if line.is_empty() {
            why.clear();
            continue;
        }
        if !raw.starts_with(' ') && !raw.starts_with('#') {
            break; // the next top-level key ends the section
        }
        if let Some(text) = line.strip_prefix('#') {
            why.push(text.trim().to_string());
            continue;
        }
        if line == "- exclude:" {
            assert!(
                pending.is_none(),
                "{CONFIG}: an `- exclude:` follows another with no `id:` between them"
            );
            pending = Some(std::mem::take(&mut why));
            continue;
        }
        if let Some(id) = line.strip_prefix("id:") {
            let why = pending
                .take()
                .unwrap_or_else(|| panic!("{CONFIG}: `id:` outside an `- exclude:` block"));
            out.push(Exclusion {
                id: id.trim().trim_matches('"').to_string(),
                why: why.join(" "),
            });
            continue;
        }
        panic!(
            "{CONFIG}: `{line}` is not a shape this pin reads (only `# why`, `- exclude:` and \
             `id:` lines) — an `include:` or a `tags:` filter widens or narrows the scan by a \
             route this test cannot audit, so teach it the shape rather than let the pin go quiet"
        );
    }
    assert!(
        pending.is_none(),
        "{CONFIG}: the last `- exclude:` names no `id:`"
    );
    out
}

#[test]
fn every_exclusion_names_the_pr_and_the_count_it_was_read_from() {
    let config = read(CONFIG);
    let found = exclusions(&config);
    assert!(
        !found.is_empty(),
        "{CONFIG} excludes nothing — the 100-annotation stall on #239 is back"
    );
    // A PR reference and a count read from it, in the exclusion's own
    // comment: "81 of 100" beside "#239". Either half missing is a
    // decision without its evidence.
    let pr = Regex::new(r"#\d+").unwrap();
    let count = Regex::new(r"\b\d+ of \d+\b").unwrap();
    for e in &found {
        assert!(
            e.id.starts_with("rust/") || e.id.starts_with("js/"),
            "{CONFIG}: `{}` is not a CodeQL query id (rust/… or js/…)",
            e.id
        );
        assert!(
            !e.why.is_empty(),
            "{CONFIG}: `{}` is excluded with no comment above it — say WHY, with the PR and count",
            e.id
        );
        assert!(
            pr.is_match(&e.why),
            "{CONFIG}: the why for `{}` names no PR (`#<n>`): {:?}",
            e.id,
            e.why
        );
        assert!(
            count.is_match(&e.why),
            "{CONFIG}: the why for `{}` names no count (`<hits> of <total>`): {:?}",
            e.id,
            e.why
        );
    }
    let mut ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "{CONFIG}: a query id is excluded twice");
}

#[test]
fn the_tests_are_out_of_scope_by_path() {
    let config = read(CONFIG);
    let section = config
        .split_once("\npaths-ignore:")
        .unwrap_or_else(|| panic!("{CONFIG} has no top-level `paths-ignore:` section"))
        .1;
    let patterns: Vec<&str> = section
        .lines()
        .take_while(|l| l.starts_with(' ') || l.starts_with('#') || l.trim().is_empty())
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|p| p.trim().trim_matches('"'))
        .collect();
    for want in [
        "**/tests/**",
        "**/*.test.ts",
        "**/*.spec.ts",
        "apps/web/tests/**",
    ] {
        assert!(
            patterns.contains(&want),
            "{CONFIG}: paths-ignore lacks `{want}` (has {patterns:?}) — the test corpus is \
             where the yard's sanitizer fixture and the pins' uid-logging failure messages live"
        );
    }
}

#[test]
fn the_workflow_reads_the_config_and_stays_sha_pinned() {
    let workflow = read(WORKFLOW);
    let config_ref = format!("config-file: ./{CONFIG}");
    assert!(
        workflow.lines().any(|l| l.trim() == config_ref),
        "{WORKFLOW} does not hand the init step `{config_ref}` — the config is a decision the \
         scan never reads"
    );
    assert!(
        repo_root().join(CONFIG).is_file(),
        "{WORKFLOW} references {CONFIG}, which is not in the tree"
    );
    let pinned = Regex::new(r"^\s*(?:-\s*)?uses:\s*\S+@[0-9a-f]{40}\b").unwrap();
    let uses: Vec<&str> = workflow
        .lines()
        .filter(|l| l.trim_start().starts_with("uses:") || l.trim_start().starts_with("- uses:"))
        .collect();
    assert!(!uses.is_empty(), "{WORKFLOW} uses no action at all");
    for line in uses {
        assert!(
            pinned.is_match(line),
            "{WORKFLOW}: `{}` is not pinned to a 40-hex commit SHA (scorecard Pinned-Dependencies)",
            line.trim()
        );
    }
}
