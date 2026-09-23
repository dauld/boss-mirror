//! `boss channels --core-changes` — is the core settling, read as the
//! ABSOLUTE number of core changes each day (backlog bd93d2be).
//!
//! David's call, 2026-09-19, recorded on the packet so it is not
//! re-argued: the crest signal toward 1.0.0 is the number of changes
//! made to core per day, TRENDING DOWN — not core's SHARE of all work.
//! A flat 33% share can be held while core work grows, if everything
//! else grows with it, and it can be "improved" by doing more work
//! elsewhere, which is gaming rather than stabilising. An absolute
//! count only falls when the core genuinely stops needing changes. The
//! point is to check in honestly, not to hit a number: NO target and NO
//! threshold, the rule 79fdc808 set for the tier mix this reading sits
//! beside in the platform retro. One answers "is the core settling",
//! the other "is work moving outward", and they mean something only
//! together.
//!
//! WHAT "A CHANGE" IS, pinned here and printed with every reading so
//! nobody re-derives it: a FILE-TOUCH in the `core` tier of the one
//! tier map (`infra/platform/tiers.toml` — `crates/core/`, less its
//! `seeds/`, which the map calls data), per commit on origin/main's
//! first-parent line, summed per day. A file two trains touch in one
//! day counts twice. A commit count would hide a 400-file day; a line
//! count is dominated by generated and mechanical edits. A day is the
//! commit's own committer date as recorded (the forge writes Pacific,
//! so the day is David's day). That definition reproduces the packet's
//! baseline, measured by hand the same way before this verb existed:
//! read over origin/main on 2026-09-23 it prints 101, 61, 93, 155, 96,
//! 139 and 400 for 09-11..09-18 against the packet's 101, 61, 93, 155,
//! 96, 140 and 400 — the one difference a `crates/core/**/seeds/` file
//! the tier map calls data. (09-10 and 09-19 were partial days there.)
//!
//! The record is git, not the jobs API: every landed change is a commit
//! on main, stamped or not, so unlike the tier mix this reading has no
//! coverage gap. What it CAN be is stale — a checkout that has not
//! fetched answers a quiet week — so the reading names the ref's sha and
//! newest commit day, and a window with no commit at all withholds its
//! mean instead of reporting a settled core.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;

/// The ref the reading is taken over — what landed, not a local branch.
pub(crate) const CORE_CHANGES_REF: &str = "origin/main";

/// The tier whose changes are counted, named in the tier map.
const CORE_TIER: &str = "core";

/// The marker `--format` puts before each commit's date, so a commit
/// line cannot be mistaken for a path (no path carries a U+0001).
const COMMIT_MARK: char = '\u{1}';

/// One landed commit: its day and the paths it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Landed {
    pub(crate) day: NaiveDate,
    pub(crate) paths: Vec<String>,
}

/// Parse `git log --name-only --format=%x01%cs` output into commits.
/// A line after the marker is the day; every other non-empty line is a
/// path of the commit above it.
pub(crate) fn parse_log(output: &str) -> Vec<Landed> {
    output
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .fold(Vec::new(), |mut landed: Vec<Landed>, line| {
            match line.strip_prefix(COMMIT_MARK) {
                // An unparseable day is dropped with its paths rather
                // than guessed onto a neighbour: the paths that follow
                // land on a commit that is never pushed.
                Some(day) => landed.push(Landed {
                    day: day.trim().parse().unwrap_or(NaiveDate::MIN),
                    paths: Vec::new(),
                }),
                None => {
                    if let Some(commit) = landed.last_mut() {
                        commit.paths.push(line.trim().to_string());
                    }
                }
            }
            landed
        })
        .into_iter()
        .filter(|c| c.day != NaiveDate::MIN)
        .collect()
}

/// One day of the reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Day {
    pub(crate) day: NaiveDate,
    /// Core file-touches landed that day.
    pub(crate) core: usize,
    /// Commits landed that day — zero means a quiet PIPELINE, which is
    /// not the same thing as a quiet core, so it is printed beside.
    pub(crate) commits: usize,
}

/// The reading over a window: every calendar day from `since` to
/// `until`, a day with nothing landed included as zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreChanges {
    pub(crate) days: Vec<Day>,
}

impl CoreChanges {
    pub(crate) fn of(landed: &[Landed], since: NaiveDate, until: NaiveDate) -> CoreChanges {
        let by_day: BTreeMap<NaiveDate, (usize, usize)> = landed
            .iter()
            .filter(|c| c.day >= since && c.day <= until)
            .fold(BTreeMap::new(), |mut m, c| {
                let core = c
                    .paths
                    .iter()
                    .filter(|p| boss_core::tiers::tier_of(p).is_some_and(|t| t.name == CORE_TIER))
                    .count();
                let e = m.entry(c.day).or_insert((0, 0));
                e.0 += core;
                e.1 += 1;
                m
            });
        CoreChanges {
            days: since
                .iter_days()
                .take_while(|day| *day <= until)
                .map(|day| {
                    let (core, commits) = by_day.get(&day).copied().unwrap_or((0, 0));
                    Day { day, core, commits }
                })
                .collect(),
        }
    }

    /// The reading as printed lines — the unit first, one row per day,
    /// then the window's total and mean, then the direction. Pure, so
    /// the withholding rule is tested without git.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut out = vec![UNIT_LINE.to_string()];
        out.extend(self.days.iter().map(|d| {
            format!(
                "    {}  {:>6} core  ({} commit{})",
                d.day,
                d.core,
                d.commits,
                if d.commits == 1 { "" } else { "s" }
            )
        }));
        let commits: usize = self.days.iter().map(|d| d.commits).sum();
        if commits == 0 {
            out.push(
                "  no landed commit in the window — a stale or unfetched ref answers exactly \
                 this, so nothing is read: git fetch origin, then read again"
                    .to_string(),
            );
        } else {
            let core: usize = self.days.iter().map(|d| d.core).sum();
            out.push(format!(
                "  window: {core} core file-touches over {} day(s), mean {:.1}/day \
                 (the newest day is partial until it ends)",
                self.days.len(),
                core as f64 / self.days.len() as f64
            ));
        }
        out.push(DIRECTION_LINE.to_string());
        out
    }
}

/// The unit, printed with every reading so nobody re-derives it
/// (the packet's own requirement).
const UNIT_LINE: &str = "  counted: file-touches in the core tier of infra/platform/tiers.toml \
     (crates/core/, its seeds/ are data) per commit on the first-parent line, summed per day — \
     a file two commits touch counts twice; a day is the commit's own committer date";

/// The direction the reading is read in, printed with every reading so
/// a reader never supplies a target of their own (bd93d2be, 79fdc808).
const DIRECTION_LINE: &str = "  direction: DOWN — the core settling shows as fewer core changes \
     a day; no target and no threshold, by decision (backlog bd93d2be): compare the mean with \
     last week's, name the change, and read it beside the tier mix";

/// `boss channels --core-changes [--since <date>]` — the reading over
/// origin/main in this checkout, the form the platform retro's
/// `collect` step quotes into `core_changes` every week (bd93d2be).
/// Reads git alone: no jobs API.
pub fn run(since: Option<NaiveDate>, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let until = now.date_naive();
    let since = since.unwrap_or_else(|| {
        (now - chrono::Duration::days(crate::channels::TIER_WINDOW_DAYS)).date_naive()
    });
    if since > until {
        bail!("--since {since} is after today ({until}): an empty window reads nothing");
    }
    let head = git(&[
        "log",
        "-1",
        "--format=%h %cs",
        "--abbrev=12",
        CORE_CHANGES_REF,
    ])
    .with_context(|| format!("{CORE_CHANGES_REF} does not resolve in this checkout"))?;
    // A day earlier than the window: `--since` is read in this host's
    // zone while the day is the commit's own, so the edge is filtered
    // by `CoreChanges::of`, never by git.
    let log = git(&[
        "log",
        CORE_CHANGES_REF,
        "--first-parent",
        "--diff-merges=first-parent",
        "--find-renames",
        "--name-only",
        "--format=%x01%cs",
        &format!("--since={}", since - chrono::Duration::days(1)),
    ])?;
    let reading = CoreChanges::of(&parse_log(&log), since, until);
    println!("boss channels --core-changes — is the core settling: core changes per day");
    println!(
        "\n  read over {CORE_CHANGES_REF} @ {} (sha, newest commit day) since {since}",
        head.trim()
    );
    for line in reading.lines() {
        println!("{line}");
    }
    Ok(())
}

/// Run git in this checkout; a nonzero exit is an error naming git's
/// own words, never an empty answer — an empty log is a reading.
fn git(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    const LOG: &str = "\u{1}2026-09-18\n\
        \n\
        crates/core/boss-jobs/src/lib.rs\n\
        crates/core/boss-jobs/seeds/step_types.toml\n\
        apps/web/src/x.ts\n\
        \u{1}2026-09-18\n\
        \n\
        crates/core/boss-jobs/src/lib.rs\n\
        crates/modules/boss-people/src/lib.rs\n\
        \u{1}2026-09-16\n\
        \n\
        crates/core/boss-events/src/lib.rs\n\
        \u{1}2026-09-15\n\
        \n\
        crates/core/boss-events/src/lib.rs\n";

    #[test]
    fn a_log_parses_into_one_commit_per_marker_with_its_paths() {
        let landed = parse_log(LOG);
        assert_eq!(landed.len(), 4);
        assert_eq!(landed[0].day, d("2026-09-18"));
        assert_eq!(landed[0].paths.len(), 3);
        assert_eq!(
            landed[1].paths,
            vec![
                "crates/core/boss-jobs/src/lib.rs".to_string(),
                "crates/modules/boss-people/src/lib.rs".to_string(),
            ]
        );
    }

    #[test]
    fn a_core_file_two_commits_touch_counts_twice_and_core_seeds_are_data() {
        let r = CoreChanges::of(&parse_log(LOG), d("2026-09-16"), d("2026-09-18"));
        // Every calendar day in the window, a quiet one as zero.
        assert_eq!(
            r.days,
            vec![
                Day {
                    day: d("2026-09-16"),
                    core: 1,
                    commits: 1
                },
                Day {
                    day: d("2026-09-17"),
                    core: 0,
                    commits: 0
                },
                // lib.rs twice (two commits); the seeds file is the data
                // tier by the map, and apps/ and modules/ are not core.
                Day {
                    day: d("2026-09-18"),
                    core: 2,
                    commits: 2
                },
            ]
        );
    }

    #[test]
    fn a_commit_before_the_window_is_not_counted() {
        let r = CoreChanges::of(&parse_log(LOG), d("2026-09-16"), d("2026-09-16"));
        assert_eq!(
            r.days,
            vec![Day {
                day: d("2026-09-16"),
                core: 1,
                commits: 1
            }]
        );
    }

    #[test]
    fn the_reading_states_its_unit_its_direction_down_and_no_target() {
        let text = CoreChanges::of(&parse_log(LOG), d("2026-09-16"), d("2026-09-18"))
            .lines()
            .join("\n");
        for phrase in [
            "file-touches",
            "counts twice",
            "infra/platform/tiers.toml",
            "2026-09-17       0 core  (0 commits)",
            "2026-09-18       2 core  (2 commits)",
            "3 core file-touches over 3 day(s), mean 1.0/day",
            "direction: DOWN",
            "no target and no threshold",
        ] {
            assert!(text.contains(phrase), "reading names `{phrase}`:\n{text}");
        }
    }

    /// NO EVIDENCE IS NOT A PASS. A checkout that never fetched reads a
    /// window with nothing landed, and a mean of zero there would report
    /// a settled core from a stale ref.
    #[test]
    fn a_window_with_no_landed_commit_withholds_its_mean() {
        let text = CoreChanges::of(&[], d("2026-09-20"), d("2026-09-22"))
            .lines()
            .join("\n");
        assert!(text.contains("no landed commit"), "{text}");
        assert!(!text.contains("/day"), "no mean over nothing: {text}");
    }
}
