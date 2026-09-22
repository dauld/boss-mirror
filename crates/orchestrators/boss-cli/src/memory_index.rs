//! The agent memory index, and the budget it must stay under.
//!
//! `MEMORY.md` is the index a session is handed at startup: one line per
//! memory, and the only thing that tells an agent a memory exists. It
//! lives OUTSIDE this repo (under `$HOME/.claude/projects/…`) and is
//! loaded by a harness we do not control, which truncates it SILENTLY
//! once it grows too large — no warning, no marker, nothing in the
//! session that distinguishes a complete index from a cut one.
//!
//! That is the one forbidden failure mode: a store that sheds content by
//! growing (backlog 4ea1b28c; David's resolution on design a5368918's
//! `refusal` anchor, 2026-09-19 — the day line 152 of 152 vanished).
//! CLAUDE.md §Diagnosis states the rule it breaks: treat any code that
//! reduces a record before storing it as throwing away the only copy.
//!
//! We cannot make the harness refuse. We CAN declare our own budget,
//! measure against it, and say so where an actor who can act will read
//! it — `boss orient`, the verb every session runs before it picks up
//! work. That is the mechanism recorded on the packet: a cadence rule
//! files into a station 190 deep, and a pod startup check fires into a
//! log before anyone is reading. The measurement is a pure function so
//! the wording is pinned, and the verdict is a LINE, never a nonzero
//! exit — an instrument that refuses to report the rest of the approach
//! is the boot guard that took the system of record down (2026-09-07).

use std::path::{Path, PathBuf};

/// Where the index lives, relative to `$HOME`. The project slug is the
/// main checkout's path with its separators flattened (`/work/boss` ->
/// `-work-boss`), which is the harness's own spelling; the dev pod has
/// exactly one project. Until this constant landed the tree did not
/// know the path at all, which is why no check could be written for it.
pub const DEFAULT_RELATIVE_PATH: &str = ".claude/projects/-work-boss/memory/MEMORY.md";

/// Points the check at another index — a second project, or a fixture.
pub const PATH_ENV: &str = "BOSS_MEMORY_INDEX";

/// Lines the index may hold. Two measured failure points: 152 lines /
/// ~21 KB dropped its last line on 2026-09-19, and on 2026-09-22 a
/// 157-line / 22,290-byte file reached a session as 155 lines — the two
/// entries written that day silently absent. They disagree on
/// bytes-per-line, so the harness's real ceiling is not a line count at
/// all; 140 is deliberately under BOTH, to leave headroom to notice
/// rather than to sit on the edge the way the file does today.
pub const BUDGET_LINES: usize = 140;

/// Bytes the index may hold — the same budget measured the other way,
/// because a few over-long hooks blow it at a legal line count (131 such
/// hooks were trimmed on 2026-09-19, 25,675 -> 21,388 bytes, with no
/// entry deleted).
pub const BUDGET_BYTES: usize = 20_000;

/// Every size at which this index has been SEEN to truncate, as
/// (lines, bytes): 2026-09-19, the day line 152 of 152 vanished, and
/// 2026-09-22, when 157 lines on disk reached a session as 155. The
/// budget above is under both, and a test holds it there — a budget at
/// or above a size where truncation has been observed is not a budget.
pub const OBSERVED_TRUNCATIONS: [(usize, usize); 2] = [(152, 21_388), (157, 22_290)];

/// The index's size: entries and bytes. A trailing newline is a
/// terminator, not an entry.
pub fn measure(text: &str) -> (usize, usize) {
    (text.lines().count(), text.len())
}

/// The path the check reads, from [`PATH_ENV`] or `$HOME`.
pub fn index_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var(PATH_ENV) {
        let p = p.trim().to_string();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(DEFAULT_RELATIVE_PATH))
}

/// Measure the index at `path`, or `None` if it cannot be read.
pub fn read_index(path: &Path) -> Option<(usize, usize)> {
    std::fs::read_to_string(path).ok().map(|t| measure(&t))
}

/// PURE: what `boss orient` prints about the index. `measured` is
/// `None` when the file could not be read.
///
/// The numbers ride on BOTH sides in every case, because the whole
/// defect is that an agent cannot otherwise tell a truncated index from
/// a complete one — an "under budget" carrying no numbers is the same
/// silence one layer up. The under-budget line is short, and it is the
/// evidence that nothing was dropped: no evidence is not a pass.
pub fn lines(path: &str, measured: Option<(usize, usize)>) -> Vec<String> {
    let Some((n, bytes)) = measured else {
        return vec![format!(
            "  MEMORY — UNREADABLE: no index at {path} (set {PATH_ENV} if it lives elsewhere). \
             An index nobody can measure is this check's own failure mode, one layer up."
        )];
    };
    if n <= BUDGET_LINES && bytes <= BUDGET_BYTES {
        return vec![format!(
            "  MEMORY — {n} lines / {bytes} bytes against a budget of {BUDGET_LINES} lines / \
             {BUDGET_BYTES} bytes — complete, nothing dropped"
        )];
    }
    let over_lines = n.saturating_sub(BUDGET_LINES);
    let over_bytes = bytes.saturating_sub(BUDGET_BYTES);
    vec![
        format!(
            "  MEMORY — OVER BUDGET: {n} lines / {bytes} bytes against a budget of \
             {BUDGET_LINES} lines / {BUDGET_BYTES} bytes — {over_lines} lines and \
             {over_bytes} bytes over."
        ),
        "    The loader drops the TAIL silently: the NEWEST memories, the ones just written, are \
         the ones a session never sees (measured 2026-09-22 — 157 lines on disk, 155 delivered, \
         no marker either side)."
            .to_string(),
        format!(
            "    Consolidate or delete entries in {path} until it is under budget. Nothing else \
             will tell you it is happening."
        ),
    ]
}

/// The lines `boss orient` prints: path resolved, file measured, verdict
/// worded. Best-effort by construction — there is no failure here that
/// should stop the approach printing.
pub fn report() -> Vec<String> {
    match index_path() {
        Some(p) => lines(&p.to_string_lossy(), read_index(&p)),
        None => lines("$HOME is unset", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers ride on the line in every verdict. An agent handed
    /// "MEMORY: ok" cannot tell a complete index from a cut one, which
    /// is the defect itself (4ea1b28c's `what_it_should_say`).
    #[test]
    fn every_verdict_carries_the_measurement_and_the_budget() {
        for m in [(10, 100), (BUDGET_LINES + 9, BUDGET_BYTES + 900)] {
            let out = lines("/m/MEMORY.md", Some(m)).join("\n");
            assert!(out.contains(&m.0.to_string()), "{out}");
            assert!(out.contains(&m.1.to_string()), "{out}");
            assert!(out.contains(&BUDGET_LINES.to_string()), "{out}");
            assert!(out.contains(&BUDGET_BYTES.to_string()), "{out}");
        }
    }

    /// Over budget is LOUD and names the repair. The measured shape of
    /// the loss — the NEWEST entries, not the oldest — is part of the
    /// repair: it says which end of the file a session cannot trust.
    #[test]
    fn over_budget_is_loud_and_says_which_end_is_lost() {
        let out = lines("/m/MEMORY.md", Some((157, 22_290))).join("\n");
        assert!(out.contains("OVER BUDGET"), "{out}");
        assert!(out.contains("17 lines and 2290 bytes over"), "{out}");
        assert!(out.contains("NEWEST"), "{out}");
        assert!(out.contains("/m/MEMORY.md"), "{out}");
    }

    /// Either half alone trips it: a short file of long hooks truncates
    /// exactly as a long file of short ones does.
    #[test]
    fn either_half_of_the_budget_trips_it() {
        let by_lines = lines("/m", Some((BUDGET_LINES + 1, 1))).join("\n");
        let by_bytes = lines("/m", Some((1, BUDGET_BYTES + 1))).join("\n");
        assert!(by_lines.contains("OVER BUDGET"), "{by_lines}");
        assert!(by_bytes.contains("OVER BUDGET"), "{by_bytes}");
        let at_budget = lines("/m", Some((BUDGET_LINES, BUDGET_BYTES))).join("\n");
        assert!(!at_budget.contains("OVER BUDGET"), "{at_budget}");
    }

    /// An unreadable index is not a pass. Silence about a file we could
    /// not measure reads exactly like silence about a file that fits.
    #[test]
    fn an_unreadable_index_is_not_a_pass() {
        let out = lines("/m/MEMORY.md", None).join("\n");
        assert!(out.contains("UNREADABLE"), "{out}");
        assert!(out.contains(PATH_ENV), "{out}");
    }

    /// A trailing newline is a terminator, not an entry.
    #[test]
    fn measure_counts_entries_not_terminators() {
        assert_eq!(measure("a\nb\n"), (2, 4));
        assert_eq!(measure("a\nb"), (2, 3));
        assert_eq!(measure(""), (0, 0));
    }

    /// The budget is under every size at which truncation has been SEEN
    /// (4ea1b28c), and each of those sizes reads as OVER BUDGET. A
    /// budget that would have called either observation healthy is not
    /// a budget.
    #[test]
    fn the_budget_sits_under_every_observed_failure() {
        for (n, bytes) in OBSERVED_TRUNCATIONS {
            assert!(BUDGET_LINES < n, "{BUDGET_LINES} lines against {n}");
            assert!(BUDGET_BYTES < bytes, "{BUDGET_BYTES} bytes against {bytes}");
            let out = lines("/m/MEMORY.md", Some((n, bytes))).join("\n");
            assert!(out.contains("OVER BUDGET"), "{out}");
        }
    }

    /// A real file on disk, measured the way orient measures it — a
    /// fixture of what we hope `read_to_string` counts would pin
    /// nothing. A missing path reads as unreadable, not as zero.
    #[test]
    fn a_real_file_is_measured_and_a_missing_one_is_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("MEMORY.md");
        std::fs::write(&f, "- [a](a.md) — one\n- [b](b.md) — two\n").expect("write");
        assert_eq!(read_index(&f), Some((2, 40)));
        assert_eq!(read_index(&dir.path().join("absent.md")), None);
    }

    /// The default path is the harness's own spelling, joined onto
    /// `$HOME` — the one thing the tree did not know before this module
    /// (4ea1b28c's `the_blocker_the_packet_did_not_name`).
    #[test]
    fn the_default_path_is_the_harness_spelling_under_home() {
        assert_eq!(
            PathBuf::from("/work/home")
                .join(DEFAULT_RELATIVE_PATH)
                .to_string_lossy(),
            "/work/home/.claude/projects/-work-boss/memory/MEMORY.md"
        );
    }
}
