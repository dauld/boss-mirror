//! What a dispatched run CONSUMED, read from the harness's own
//! transcript of it (backlog e6b2066f).
//!
//! THE NUMBER THIS REPLACES. A builder's handback carried the harness's
//! `subagent_tokens`, and `boss dispatch --report --tokens` recorded it
//! as the run's `total_tokens` — priced at a blend, read by both budget
//! desks. It is the size of the run's FINAL context window: it matched
//! the last turn's four counts within 1% on 62 of 68 runs, while the
//! tokens the run was billed for, summed over every turn, were a median
//! 48x larger, 96.8% of them cache reads. Real spend was a median 4.9x
//! the recorded figure.
//!
//! THE NUMBER THAT WAS ALWAYS THERE. Claude Code writes every subagent's
//! turns to `<projects>/<project>/<session>/subagents/agent-<id>.jsonl`,
//! and every assistant line carries the turn's `message.usage`:
//! `input_tokens` (uncached), `cache_creation_input_tokens`,
//! `cache_read_input_tokens` and `output_tokens`. [`sum_usage`] sums
//! those four over the run — reading the record rather than retyping a
//! figure from a handback, the "receipt copied, not retyped" rule.
//!
//! ONE TURN IS SEVERAL LINES. The harness writes a line per content
//! block (thinking, text, each tool call), each carrying the SAME
//! `message.id` and the same prompt counts, with `output_tokens` growing
//! as the turn streams. A naive sum counts a three-block turn three
//! times, so turns are keyed on `message.id` (else `requestId`) and each
//! count is the turn's largest — measured on this car's own transcript,
//! where one turn's lines read output 5, then 155.
//!
//! WHICH TRANSCRIPT. [`find_transcripts`] looks for the one whose text
//! holds `agent-run <run id>` — the phrase every run section prints
//! (`dispatch::run_section`), whether the prompt carried it or the
//! builder read it from a prompt file — among subagent transcripts
//! written since the run opened. Exactly one is the run's; none, or
//! more than one, is said out loud and the report falls back to the
//! count it was given, because guessing between two transcripts would
//! record one run's spend against another.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// A run's four billed counts, summed over its turns, plus the two
/// readings that say what the old figure was and where the floor is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Of `cache_write`, the tokens written to the 1-HOUR cache, which
    /// costs more than the 5-minute rate the card prices every write
    /// at — so a nonzero figure here says the price is a floor.
    pub cache_write_1h: u64,
    /// Distinct turns summed.
    pub turns: u64,
    /// The LAST turn's four-way sum: the final context size, which is
    /// what the harness's `subagent_tokens` reports. Kept beside the
    /// sum so the two can be compared on the record.
    pub final_context: u64,
}

impl Usage {
    /// Everything the run processed.
    pub(crate) fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }
}

/// One turn's counts, as the largest seen on any of its lines.
#[derive(Debug, Clone, Copy, Default)]
struct Turn {
    input: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    cache_write_1h: u64,
}

impl Turn {
    fn widen(self, other: Turn) -> Turn {
        Turn {
            input: self.input.max(other.input),
            cache_write: self.cache_write.max(other.cache_write),
            cache_read: self.cache_read.max(other.cache_read),
            output: self.output.max(other.output),
            cache_write_1h: self.cache_write_1h.max(other.cache_write_1h),
        }
    }

    fn context(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }
}

/// Sum a transcript's per-turn usage. `None` when no line carries any —
/// no count is not a count of zero. Lines that are not JSON, or not an
/// assistant turn, are skipped: the transcript holds user turns, tool
/// results and summaries beside the turns that were billed.
pub(crate) fn sum_usage(jsonl: &str) -> Option<Usage> {
    let mut order: Vec<String> = Vec::new();
    let mut turns: std::collections::HashMap<String, Turn> = std::collections::HashMap::new();
    for (n, line) in jsonl.lines().enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(u) = v.pointer("/message/usage").filter(|u| u.is_object()) else {
            continue;
        };
        let count = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let turn = Turn {
            input: count("input_tokens"),
            cache_write: count("cache_creation_input_tokens"),
            cache_read: count("cache_read_input_tokens"),
            output: count("output_tokens"),
            cache_write_1h: u
                .pointer("/cache_creation/ephemeral_1h_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        let key = v
            .pointer("/message/id")
            .or_else(|| v.get("requestId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{n}"));
        match turns.get(&key) {
            Some(seen) => {
                let widened = seen.widen(turn);
                turns.insert(key, widened);
            }
            None => {
                order.push(key.clone());
                turns.insert(key, turn);
            }
        }
    }
    let last = order.last().and_then(|k| turns.get(k)).copied()?;
    Some(order.iter().filter_map(|k| turns.get(k)).fold(
        Usage {
            final_context: last.context(),
            ..Usage::default()
        },
        |acc, t| Usage {
            input: acc.input.saturating_add(t.input),
            cache_write: acc.cache_write.saturating_add(t.cache_write),
            cache_read: acc.cache_read.saturating_add(t.cache_read),
            output: acc.output.saturating_add(t.output),
            cache_write_1h: acc.cache_write_1h.saturating_add(t.cache_write_1h),
            turns: acc.turns + 1,
            final_context: acc.final_context,
        },
    ))
}

/// Where the harness keeps its projects: `$CLAUDE_CONFIG_DIR/projects`
/// when the session set one, else `$HOME/.claude/projects`.
pub(crate) fn projects_root() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| PathBuf::from(d).join("projects"))
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".claude").join("projects"))
        })
}

/// The phrase a run's own transcript holds and no other run's does.
pub(crate) fn needle(run_id: &str) -> String {
    format!("agent-run {run_id}")
}

/// Every subagent transcript under `root` (`<project>/<session>/
/// subagents/agent-*.jsonl`) written at or after `since` whose text
/// holds [`needle`]. The caller decides what none or several mean.
pub(crate) fn find_transcripts(root: &Path, run_id: &str, since: SystemTime) -> Vec<PathBuf> {
    let needle = needle(run_id);
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let mut found: Vec<PathBuf> = dirs(root)
        .into_iter()
        .flat_map(|project| dirs(&project))
        .flat_map(|session| dirs(&session.join("subagents")))
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
        })
        .filter(|f| {
            std::fs::metadata(f)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since)
        })
        .filter(|f| {
            std::fs::read_to_string(f)
                .map(|text| text.contains(&needle))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
}

/// What the report read, and from where.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Metered {
    pub path: PathBuf,
    pub usage: Usage,
}

/// The run's metered usage: from `explicit` when the operator named a
/// transcript, else the one transcript [`find_transcripts`] finds.
/// `Err` is a sentence saying why nothing was read — printed, and the
/// report goes on with the count it was given.
pub(crate) fn meter(
    explicit: Option<&Path>,
    root: Option<&Path>,
    run_id: &str,
    since: SystemTime,
) -> Result<Metered, String> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let root = root.ok_or(
                "no transcript directory (neither CLAUDE_CONFIG_DIR nor HOME is set) — pass \
                 --transcript <path>",
            )?;
            let mut found = find_transcripts(root, run_id, since);
            match found.len() {
                1 => found.remove(0),
                0 => {
                    return Err(format!(
                        "no subagent transcript under {} names {:?} — pass --transcript <path> \
                         to meter this run",
                        root.display(),
                        needle(run_id)
                    ));
                }
                _ => {
                    return Err(format!(
                        "{} transcripts name {:?} ({}) — refusing to guess which is this run's; \
                         pass --transcript <path>",
                        found.len(),
                        needle(run_id),
                        found
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read transcript {}: {e}", path.display()))?;
    let usage = sum_usage(&text)
        .ok_or_else(|| format!("transcript {} holds no turn usage", path.display()))?;
    Ok(Metered { path, usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two lines of one turn (the thinking block, then the tool call,
    /// output growing 5 -> 155) and one line of the next — the shape
    /// measured on this car's own transcript, 2026-09-24.
    const TRANSCRIPT: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"Your run is agent-run r-1"}}"#,
        "\n",
        r#"{"type":"assistant","requestId":"req_a","message":{"id":"msg_a","usage":{"input_tokens":2,"cache_creation_input_tokens":38126,"cache_read_input_tokens":14756,"output_tokens":5,"cache_creation":{"ephemeral_5m_input_tokens":38126,"ephemeral_1h_input_tokens":0}}}}"#,
        "\n",
        r#"{"type":"assistant","requestId":"req_a","message":{"id":"msg_a","usage":{"input_tokens":2,"cache_creation_input_tokens":38126,"cache_read_input_tokens":14756,"output_tokens":155}}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#,
        "\n",
        "not json at all\n",
        r#"{"type":"assistant","requestId":"req_b","message":{"id":"msg_b","usage":{"input_tokens":2,"cache_creation_input_tokens":6024,"cache_read_input_tokens":52882,"output_tokens":311,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":6024}}}}"#,
        "\n",
    );

    #[test]
    fn a_turn_written_as_several_lines_is_counted_once_at_its_largest() {
        let u = sum_usage(TRANSCRIPT).expect("two turns carry usage");
        assert_eq!(u.turns, 2);
        assert_eq!(u.input, 4);
        assert_eq!(u.cache_write, 38_126 + 6_024);
        assert_eq!(u.cache_read, 14_756 + 52_882);
        assert_eq!(
            u.output,
            155 + 311,
            "the streamed 5 is superseded, not added"
        );
        assert_eq!(u.cache_write_1h, 6_024);
        assert_eq!(
            u.final_context,
            2 + 6_024 + 52_882 + 311,
            "the last turn alone is the final context — the old figure"
        );
        assert_eq!(u.total(), 4 + 44_150 + 67_638 + 466);
    }

    #[test]
    fn a_transcript_with_no_turn_usage_is_no_count_not_zero() {
        assert_eq!(sum_usage(""), None);
        assert_eq!(
            sum_usage(r#"{"type":"user","message":{"content":"hi"}}"#),
            None
        );
    }

    fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn the_one_transcript_naming_the_run_is_found_and_two_are_refused() {
        let root = boss_testing::scratch_dir("transcript-usage-find");
        let mine = write(&root, "-work-boss/s-1/subagents/agent-a1.jsonl", TRANSCRIPT);
        // Another run's transcript, and the PARENT session's own file,
        // which holds the phrase too but is not a subagent transcript.
        write(
            &root,
            "-work-boss/s-1/subagents/agent-a2.jsonl",
            "agent-run r-2",
        );
        write(&root, "-work-boss/s-1.jsonl", "agent-run r-1");
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(find_transcripts(&root, "r-1", epoch), vec![mine.clone()]);

        let got = meter(None, Some(&root), "r-1", epoch).expect("found");
        assert_eq!(got.path, mine);
        assert_eq!(got.usage.turns, 2);

        // Written before the run opened: not this run's.
        let later = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(find_transcripts(&root, "r-1", later).is_empty());

        write(
            &root,
            "-work-boss/s-2/subagents/agent-b1.jsonl",
            "agent-run r-1",
        );
        let why = meter(None, Some(&root), "r-1", epoch).expect_err("two is ambiguous");
        assert!(why.contains("refusing to guess"), "{why}");
        let why = meter(None, Some(&root), "r-9", epoch).expect_err("none is none");
        assert!(why.contains("--transcript"), "{why}");

        // Named outright, the search is skipped.
        let named = meter(Some(&mine), None, "r-1", epoch).expect("named");
        assert_eq!(named.usage.output, 466);
    }
}
