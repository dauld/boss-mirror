//! A run's WORK PROFILE — where its tool time and its context went —
//! and the week's reading over many of them (backlog 2f23f4c6).
//!
//! THE MEASUREMENT THIS KEEPS. On 2026-09-24, 96 builder transcripts
//! (11,399 tool calls, 53.8 run-hours) were read by hand: searching and
//! reading were 45% of the calls, 6.5% of the tool time and 89% of the
//! context bytes (~25 MB); builds and tests were 8% of the calls and
//! 64% of the tool time (22.8 h, a mean of 89 s); 5% of the searches
//! came back empty. That answered David's question (would a code index
//! help? — no evidence) and then it was gone: nothing recorded it, so
//! the next week's answer would take the same afternoon. `boss dispatch
//! --report` already reads every run's transcript for its four token
//! counts; it now reads the tool calls too and records this profile,
//! and the IT department retro reads [`ProfileRollup`] over its week.
//!
//! TELEMETRY, NOT A FACT OF THE RUN'S RECORD. David, 2026-09-16: "the
//! audit_log just needs to capture all work that is done; sensors
//! record data that don't flow into the audit_log." How a run spent its
//! tool time is a measurement of the work, not the work, so it takes
//! the `surface_opens` shape: its own table (`agent_run_profiles`),
//! keyed on the run id it describes, no event, no rebuilder — and a
//! rebuild of `agent_runs` (which DELETEs and replays) leaves it
//! alone, which a column on that projection could not survive. Losing
//! it costs the measurement and nothing else. The run's plain
//! `tool_calls` count is the one number that DOES ride the record: it
//! was a column of `agents.run.recorded` from the first day and was
//! simply never filled.
//!
//! THE FOUR CLASSES are the ones the measurement used: search/read,
//! build/test, edit, other. Which call lands in which is decided where
//! the transcript is read (`boss-cli`'s `transcript_profile`), because
//! only that side knows the harness's tool names; this side holds the
//! numbers and the arithmetic over them, once, for every reader.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How many of a run's most-read files its profile keeps, and how many
/// the week's reading names.
pub const TOP_FILES: usize = 10;

/// One class of tool call: how many, how long the harness waited on
/// them, and how many bytes of result they put in the context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassTotal {
    pub calls: u64,
    /// From the call's line to its result's line, summed. A call whose
    /// result never arrived (the run was cut off) adds its call and no
    /// time, rather than a guess.
    pub wall_ms: u64,
    /// The result text as it entered the context — a persisted output
    /// counts its preview, because the preview is what the model read.
    pub result_bytes: u64,
}

impl ClassTotal {
    pub fn plus(self, o: ClassTotal) -> ClassTotal {
        ClassTotal {
            calls: self.calls.saturating_add(o.calls),
            wall_ms: self.wall_ms.saturating_add(o.wall_ms),
            result_bytes: self.result_bytes.saturating_add(o.result_bytes),
        }
    }
}

/// The four classes, named — a struct rather than a map so a reader
/// can never meet a fifth spelling of one of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByClass {
    pub search_read: ClassTotal,
    pub build_test: ClassTotal,
    pub edit: ClassTotal,
    pub other: ClassTotal,
}

impl ByClass {
    /// Each class with the name the rollup reports it under.
    pub fn named(&self) -> [(&'static str, ClassTotal); 4] {
        [
            ("search_read", self.search_read),
            ("build_test", self.build_test),
            ("edit", self.edit),
            ("other", self.other),
        ]
    }

    pub fn total(&self) -> ClassTotal {
        self.named()
            .iter()
            .fold(ClassTotal::default(), |acc, (_, c)| acc.plus(*c))
    }

    pub fn plus(self, o: ByClass) -> ByClass {
        ByClass {
            search_read: self.search_read.plus(o.search_read),
            build_test: self.build_test.plus(o.build_test),
            edit: self.edit.plus(o.edit),
            other: self.other.plus(o.other),
        }
    }
}

/// A file and how many times the run read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReads {
    pub path: String,
    pub reads: u64,
}

/// One run's profile, as its transcript says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkProfile {
    pub tool_calls: u64,
    pub by_class: ByClass,
    /// Tool calls made before the run's first edit of anything outside
    /// its scratch directory. `None` is a run that edited nothing — a
    /// run that built nothing, whatever it reported.
    pub calls_before_first_edit: Option<u64>,
    /// Search-class calls, and how many of them answered nothing.
    pub searches: u64,
    pub empty_searches: u64,
    /// The run's most-read files, most reads first, at most
    /// [`TOP_FILES`]. Paths are repo-relative where the transcript's
    /// worktree prefix could be stripped, so two runs' reads of one
    /// file are one row.
    pub top_files_read: Vec<FileReads>,
}

/// A profile as held: the run it describes and when it was read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunProfile {
    pub run_id: String,
    pub recorded_at: DateTime<Utc>,
    pub profile: WorkProfile,
}

/// One class's share of the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClassShare {
    pub class: &'static str,
    #[serde(flatten)]
    pub total: ClassTotal,
    /// Percent of the window's tool time, to one decimal. `None` when
    /// the window holds no tool time at all — no share, not zero.
    pub time_share_pct: Option<f64>,
    /// Percent of the window's result bytes, to one decimal.
    pub context_share_pct: Option<f64>,
}

/// The week's reading: the numbers the IT retro's `collect` step reads
/// and its `analyze` step ranks — computed here, once, so the ranking
/// is a read and not an afternoon (the 2026-09-24 analysis).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileRollup {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub runs: u64,
    /// Every class, the largest share of tool time first.
    pub classes: Vec<ClassShare>,
    /// The class holding the largest share of tool time, and of result
    /// bytes. `None` on an empty window.
    pub largest_time_share: Option<&'static str>,
    pub largest_context_share: Option<&'static str>,
    pub searches: u64,
    pub empty_searches: u64,
    pub empty_search_share_pct: Option<f64>,
    /// The median of the runs' calls-before-first-edit, over the runs
    /// that edited something.
    pub median_calls_before_first_edit: Option<u64>,
    /// Runs that edited nothing outside their scratch directory — the
    /// runs that built nothing (a landed twin, a wrong repo, a refusal),
    /// oldest reading first. Whether that was the right answer is the
    /// run's own outcome; this names them so someone asks.
    pub runs_that_edited_nothing: Vec<String>,
    /// The most-read files across the window, summed over each run's
    /// own top list (so a file outside every run's top ten is not
    /// counted), most reads first, at most [`TOP_FILES`].
    pub top_files_read: Vec<FileReads>,
}

fn pct(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| ((part as f64) * 1000.0 / (whole as f64)).round() / 10.0)
}

/// Most reads first, then path, so equal counts land in a stable order.
pub fn rank_files(mut files: Vec<FileReads>) -> Vec<FileReads> {
    files.sort_by(|a, b| b.reads.cmp(&a.reads).then_with(|| a.path.cmp(&b.path)));
    files.truncate(TOP_FILES);
    files
}

/// The reading over `profiles`, which the caller has already narrowed
/// to `[since, until)`. Pure, so both adapters answer the same thing.
pub fn rollup(
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    profiles: &[RunProfile],
) -> ProfileRollup {
    let mut ordered: Vec<&RunProfile> = profiles.iter().collect();
    ordered.sort_by(|a, b| {
        a.recorded_at
            .cmp(&b.recorded_at)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    let by_class = ordered
        .iter()
        .fold(ByClass::default(), |acc, p| acc.plus(p.profile.by_class));
    let whole = by_class.total();
    let mut classes: Vec<ClassShare> = by_class
        .named()
        .into_iter()
        .map(|(class, total)| ClassShare {
            class,
            total,
            time_share_pct: pct(total.wall_ms, whole.wall_ms),
            context_share_pct: pct(total.result_bytes, whole.result_bytes),
        })
        .collect();
    classes.sort_by_key(|c| std::cmp::Reverse(c.total.wall_ms));
    let largest = |key: fn(&ClassTotal) -> u64| {
        by_class
            .named()
            .into_iter()
            .filter(|(_, t)| key(t) > 0)
            .max_by(|a, b| key(&a.1).cmp(&key(&b.1)).then_with(|| b.0.cmp(a.0)))
            .map(|(name, _)| name)
    };
    let searches = ordered.iter().map(|p| p.profile.searches).sum::<u64>();
    let empty_searches = ordered
        .iter()
        .map(|p| p.profile.empty_searches)
        .sum::<u64>();
    let mut before: Vec<u64> = ordered
        .iter()
        .filter_map(|p| p.profile.calls_before_first_edit)
        .collect();
    before.sort_unstable();
    let summed = ordered
        .iter()
        .flat_map(|p| p.profile.top_files_read.iter())
        .fold(
            std::collections::BTreeMap::<String, u64>::new(),
            |mut acc, f| {
                *acc.entry(f.path.clone()).or_default() += f.reads;
                acc
            },
        );
    ProfileRollup {
        since,
        until,
        runs: ordered.len() as u64,
        classes,
        largest_time_share: largest(|t| t.wall_ms),
        largest_context_share: largest(|t| t.result_bytes),
        searches,
        empty_searches,
        empty_search_share_pct: pct(empty_searches, searches),
        median_calls_before_first_edit: before.get(before.len() / 2).copied(),
        runs_that_edited_nothing: ordered
            .iter()
            .filter(|p| p.profile.calls_before_first_edit.is_none())
            .map(|p| p.run_id.clone())
            .collect(),
        top_files_read: rank_files(
            summed
                .into_iter()
                .map(|(path, reads)| FileReads { path, reads })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, h, 0, 0).unwrap()
    }

    fn class(calls: u64, wall_ms: u64, result_bytes: u64) -> ClassTotal {
        ClassTotal {
            calls,
            wall_ms,
            result_bytes,
        }
    }

    fn run(id: &str, h: u32, profile: WorkProfile) -> RunProfile {
        RunProfile {
            run_id: id.into(),
            recorded_at: at(h),
            profile,
        }
    }

    /// The 2026-09-24 shape in miniature: searching holds most of the
    /// context and little of the time, building holds most of the time.
    /// The reading ranks both — they are different classes, which is
    /// the finding — and names the run that edited nothing.
    #[test]
    fn the_week_ranks_time_and_context_separately_and_names_the_run_that_built_nothing() {
        let builder = WorkProfile {
            tool_calls: 10,
            by_class: ByClass {
                search_read: class(5, 1_000, 8_900),
                build_test: class(2, 6_000, 500),
                edit: class(2, 100, 300),
                other: class(1, 2_900, 300),
            },
            calls_before_first_edit: Some(6),
            searches: 5,
            empty_searches: 1,
            top_files_read: vec![
                FileReads {
                    path: "crates/a.rs".into(),
                    reads: 3,
                },
                FileReads {
                    path: "crates/b.rs".into(),
                    reads: 1,
                },
            ],
        };
        let twin = WorkProfile {
            tool_calls: 4,
            by_class: ByClass {
                search_read: class(4, 0, 0),
                ..ByClass::default()
            },
            calls_before_first_edit: None,
            searches: 4,
            empty_searches: 0,
            top_files_read: vec![FileReads {
                path: "crates/b.rs".into(),
                reads: 4,
            }],
        };
        let r = rollup(
            at(0),
            at(23),
            &[run("r-twin", 9, twin), run("r-built", 8, builder)],
        );
        assert_eq!(r.runs, 2);
        assert_eq!(r.largest_time_share, Some("build_test"));
        assert_eq!(r.largest_context_share, Some("search_read"));
        assert_eq!(r.classes[0].class, "build_test", "ranked by time share");
        assert_eq!(r.classes[0].time_share_pct, Some(60.0));
        let search = r.classes.iter().find(|c| c.class == "search_read").unwrap();
        assert_eq!(search.total.calls, 9);
        assert_eq!(search.context_share_pct, Some(89.0));
        assert_eq!(r.searches, 9);
        assert_eq!(r.empty_searches, 1);
        assert_eq!(r.empty_search_share_pct, Some(11.1));
        assert_eq!(r.median_calls_before_first_edit, Some(6));
        assert_eq!(r.runs_that_edited_nothing, vec!["r-twin".to_string()]);
        assert_eq!(
            r.top_files_read,
            vec![
                FileReads {
                    path: "crates/b.rs".into(),
                    reads: 5
                },
                FileReads {
                    path: "crates/a.rs".into(),
                    reads: 3
                },
            ],
            "summed across runs, most reads first"
        );
    }

    /// An empty window has no shares — `None`, never a 0% that reads as
    /// a measurement.
    #[test]
    fn an_empty_window_states_no_share() {
        let r = rollup(at(0), at(1), &[]);
        assert_eq!(r.runs, 0);
        assert_eq!(r.largest_time_share, None);
        assert_eq!(r.largest_context_share, None);
        assert_eq!(r.empty_search_share_pct, None);
        assert_eq!(r.median_calls_before_first_edit, None);
        assert!(r.classes.iter().all(|c| c.time_share_pct.is_none()));
    }

    #[test]
    fn a_profile_round_trips_as_the_wire_spells_it() {
        let p = WorkProfile {
            tool_calls: 1,
            calls_before_first_edit: Some(0),
            ..WorkProfile::default()
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["by_class"]["build_test"]["wall_ms"], 0);
        assert_eq!(v["calls_before_first_edit"], 0);
        let back: WorkProfile = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }
}
