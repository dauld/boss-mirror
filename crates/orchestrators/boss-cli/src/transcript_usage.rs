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
//!
//! WHICH MODEL. The same transcript says which model every turn was
//! billed as, so the record names that model rather than the one the
//! step's agent block declared ([`RunModels`], backlog 6bb85880).

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

/// Which model a run ran on, as its transcript says (backlog 6bb85880).
///
/// THE WORD THIS REPLACES. The record took the model from the run
/// packet — the Workflow agent block's `model`, `opus-5[1m]` on all 20
/// blocks — and priced the run at that row. Measured 2026-09-24: the
/// newest subagent transcripts say `claude-opus-5-5` on every billed
/// turn, because the agent definitions say `model: opus`, an alias the
/// harness resolves to the newest Opus. The block was a declaration;
/// the transcript is the receipt, and the meter already reads it.
///
/// Two readings, both the harness's own: every billed turn's
/// `message.model` (the API id the turn was billed as), and the model
/// attachment's `identity.modelId` (what the session was launched as,
/// which carries the `[1m]` context suffix the card spells). The turns
/// are the authority; the identity is believed only when it names the
/// same model, and then only for its spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunModels {
    /// Every distinct model id a billed turn names, first-seen order.
    /// `<synthetic>` is the harness speaking — a zero-usage line no model
    /// produced — and is not a model.
    pub billed: Vec<String>,
    /// The harness's model attachment, when the transcript has one.
    pub identity: Option<String>,
}

impl RunModels {
    /// The model as `agent_rate_card` spells it, or `None` when the
    /// transcript names none. Several billed models are named together
    /// (`opus-5-5+haiku-4-5`): the four counts are summed across them
    /// and cannot be priced at one row's rates, so no row names the
    /// pair and the run reads as unpriced — rather than being priced,
    /// wholly, at whichever model came first.
    pub(crate) fn recorded(&self) -> Option<String> {
        match self.billed.as_slice() {
            [] => self
                .identity
                .as_deref()
                .map(|i| card_spelling(i).to_string()),
            [one] => {
                let spelled = self
                    .identity
                    .as_deref()
                    .filter(|i| i.split_once('[').map_or(*i, |(base, _)| base) == one)
                    .unwrap_or(one);
                Some(card_spelling(spelled).to_string())
            }
            many => Some(
                many.iter()
                    .map(|m| card_spelling(m))
                    .collect::<Vec<_>>()
                    .join("+"),
            ),
        }
    }
}

/// An API model id as the rate card spells it: without the `claude-`
/// prefix (20260910030644 keys the card on `opus-5`, not `claude-opus-5`).
/// Nothing else is rewritten — a dated snapshot id keeps its date and
/// reads as unpriced until a row names it, because matching is exact.
pub(crate) fn card_spelling(api_id: &str) -> &str {
    api_id.strip_prefix("claude-").unwrap_or(api_id)
}

/// The models a transcript names, per [`RunModels`].
pub(crate) fn read_models(jsonl: &str) -> RunModels {
    jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .fold(RunModels::default(), |mut acc, v| {
            let billed = (v.get("type").and_then(Value::as_str) == Some("assistant"))
                .then(|| v.pointer("/message/model").and_then(Value::as_str))
                .flatten()
                .filter(|m| *m != "<synthetic>" && !m.is_empty());
            if let Some(m) = billed
                && !acc.billed.iter().any(|b| b == m)
            {
                acc.billed.push(m.to_string());
            }
            if acc.identity.is_none()
                && v.pointer("/attachment/type").and_then(Value::as_str) == Some("model")
            {
                acc.identity = v
                    .pointer("/attachment/identity/modelId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            acc
        })
}

/// What the report read, and from where.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Metered {
    pub path: PathBuf,
    pub usage: Usage,
    /// The model the transcript says ran (backlog 6bb85880).
    pub models: RunModels,
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
    let models = read_models(&text);
    Ok(Metered {
        path,
        usage,
        models,
    })
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

    /// The shape measured on this car's own transcript, 2026-09-25: the
    /// harness's model attachment names `claude-opus-5-5[1m]`, every
    /// billed turn names `claude-opus-5-5`, and a `<synthetic>` line —
    /// the harness speaking, zero usage — rides beside them.
    const OPUS_5_5: &str = concat!(
        r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5[1m]","marketingName":"Opus 5.5 (1M context)"}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_a","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":900,"output_tokens":7}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_s","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_b","model":"claude-opus-5-5","usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":1000,"output_tokens":9}}}"#,
        "\n",
    );

    /// WHICH MODEL RAN is read from the transcript it was billed in
    /// (backlog 6bb85880): the record said `opus-5[1m]` — the Workflow
    /// block's word — for runs whose every turn said `claude-opus-5-5`.
    #[test]
    fn the_model_is_read_from_the_transcript_and_spelled_as_the_card_spells_it() {
        let m = read_models(OPUS_5_5);
        assert_eq!(m.billed, vec!["claude-opus-5-5".to_string()]);
        assert_eq!(m.identity.as_deref(), Some("claude-opus-5-5[1m]"));
        assert_eq!(
            m.recorded().as_deref(),
            Some("opus-5-5[1m]"),
            "the identity names the billed model and the context it ran at"
        );

        // No identity attachment (a transcript older than it): the
        // billed id alone, still without the `claude-` prefix.
        let bare = RunModels {
            billed: vec!["claude-opus-5-5".into()],
            identity: None,
        };
        assert_eq!(bare.recorded().as_deref(), Some("opus-5-5"));

        // An identity that is NOT the billed model is not believed: the
        // turns are what was billed.
        let disagree = RunModels {
            billed: vec!["claude-opus-5-5".into()],
            identity: Some("claude-opus-5[1m]".into()),
        };
        assert_eq!(disagree.recorded().as_deref(), Some("opus-5-5"));

        // Two billed models cannot be priced at one row's rates. Both
        // are named, and no row names the pair, so the run reads as
        // unpriced rather than wholly priced at either one.
        let two = RunModels {
            billed: vec!["claude-opus-5-5".into(), "claude-haiku-4-5".into()],
            identity: Some("claude-opus-5-5[1m]".into()),
        };
        assert_eq!(two.recorded().as_deref(), Some("opus-5-5+haiku-4-5"));

        assert_eq!(RunModels::default().recorded(), None, "nothing said");
    }

    #[test]
    fn a_metered_run_carries_the_models_its_transcript_names() {
        let root = boss_testing::scratch_dir("transcript-usage-model");
        let path = write(&root, "s/subagents/agent-m.jsonl", OPUS_5_5);
        let got = meter(Some(&path), None, "r-1", SystemTime::UNIX_EPOCH).expect("named");
        assert_eq!(got.models.recorded().as_deref(), Some("opus-5-5[1m]"));
        assert_eq!(got.usage.turns, 3, "the synthetic line is a turn of zero");
        assert_eq!(got.usage.output, 16);
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
