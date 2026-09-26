//! Where a dispatched run's tool time and context went, read from the
//! same transcript [`crate::transcript_usage`] meters (backlog
//! 2f23f4c6).
//!
//! THE MEASUREMENT THIS KEEPS. On 2026-09-24, 96 builder transcripts
//! were read by hand: searching and reading were 45% of the calls, 6.5%
//! of the tool time and 89% of the context bytes; builds and tests were
//! 8% of the calls and 64% of the tool time; 5% of the searches came
//! back empty. [`work_profile`] takes those numbers from one run's
//! transcript every time `boss dispatch --report` meters it, and the
//! IT department retro reads them over its week
//! (`boss_jobs::agent_runs::rollup`).
//!
//! THE TRANSCRIPT'S SHAPE, measured on this car's own and on 100 of the
//! dev pod's subagent transcripts (2026-09-26). An assistant line
//! carries one content block; a `tool_use` block has an `id`, a `name`
//! and an `input`. The result arrives on a later `user` line as a
//! `tool_result` block naming that `tool_use_id`, its `content` a
//! string (or a list of `text` blocks), and every line carries its own
//! `timestamp`. So a call's wall time is its result line's timestamp
//! less its call line's, and its context bytes are the result text as
//! the model received it — for an output the harness persisted to a
//! file, that is the preview, which is exactly what entered the
//! context. On the pod almost every search is a `Bash` command (2,131
//! of 2,851 calls in those 100 transcripts; no `Grep` or `Glob` at
//! all), so a `Bash` call is classed by the program its command runs.
//!
//! THE FOUR CLASSES, and what lands in each:
//!   - search_read — `Read`, `Grep`, `Glob`, `LS`, `WebFetch`,
//!     `WebSearch`, and a `Bash` whose program reads (grep, find, ls,
//!     cat, sed without -i, git show/log/diff/grep, `boss-api GET`, …);
//!   - build_test — a `Bash` running cargo, wt-cargo, wt-web, bun, npm,
//!     make, `boss gate`, the gate script, or a script whose name says
//!     gate/test/lint/preflight/check;
//!   - edit — `Edit`, `Write`, `MultiEdit`, `NotebookEdit` of a file
//!     OUTSIDE the scratch directory, and a `Bash` sed -i / perl -i;
//!   - other — everything else, including a write of the run's own
//!     scratch files (a commit message, a gate script), which is
//!     plumbing the builder rules require, not a change to the car.
//!
//! The first `edit` is where "calls before the first edit" stops
//! counting, and a run with none is a run that built nothing.

use std::collections::HashMap;

use boss_jobs::agent_runs::{ByClass, ClassTotal, FileReads, WorkProfile, rank_files};
use serde_json::Value;

/// Which of the four a call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    SearchRead,
    BuildTest,
    Edit,
    Other,
}

/// What one call contributed: its class, whether it was a SEARCH (a
/// search/read that looks for something rather than opening a file
/// already named), and the files it read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub class: Class,
    pub search: bool,
    pub reads: Vec<String>,
}

const SEARCH_PROGRAMS: &[&str] = &["grep", "rg", "egrep", "fgrep", "ag", "find", "fd"];
const READ_PROGRAMS: &[&str] = &[
    "ls", "tree", "cat", "head", "tail", "sed", "awk", "wc", "less", "more", "jq", "stat", "file",
    "diff", "nl", "cut",
];
/// The read programs whose non-flag arguments are the files they read.
const FILE_READERS: &[&str] = &["cat", "head", "tail", "sed", "less", "more", "nl", "wc"];
const GIT_READS: &[&str] = &[
    "show",
    "log",
    "diff",
    "grep",
    "status",
    "blame",
    "ls-files",
    "ls-tree",
    "rev-parse",
    "merge-base",
    "cat-file",
];
const BUILD_PROGRAMS: &[&str] = &[
    "cargo",
    "wt-cargo",
    "wt-web",
    "bun",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "make",
    "pytest",
    "rustc",
    "tsc",
    "svelte-check",
];
/// A script run by path whose name says one of these is a build or a
/// check (`bash <scratch>/gate.sh`, `bash infra/gate.sh --lint`).
const BUILD_SCRIPT_WORDS: &[&str] = &["gate", "test", "lint", "preflight", "check"];
/// Words that only wrap the program they run.
const WRAPPERS: &[&str] = &["timeout", "nice", "time", "env", "nohup", "exec", "command"];
/// A segment that sets up the shell rather than doing the work.
const SETUP: &[&str] = &["cd", "export", "set", "source", "."];

fn unquote(t: &str) -> &str {
    t.trim_matches(|c| c == '\'' || c == '"')
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// The words of the first segment of `cmd` that does the work: the
/// command is cut at `&&`, `||`, `;`, `|` and newlines, segments that
/// only set up the shell are passed over, and leading `VAR=value`
/// assignments and wrappers (`timeout 60`, `nice`) are dropped.
fn working_words(cmd: &str) -> Vec<&str> {
    cmd.split(['\n', ';', '|', '&'])
        .map(|seg| {
            let words: Vec<&str> = seg.split_whitespace().collect();
            let mut i = 0;
            while i < words.len() {
                let w = words[i];
                if w.contains('=') && !w.starts_with('-') && !w.starts_with('=') {
                    i += 1;
                } else if WRAPPERS.contains(&w) {
                    i += 1;
                    // `timeout 60 cargo …`: the duration is the wrapper's.
                    while i < words.len()
                        && (words[i].starts_with('-')
                            || words[i].chars().next().is_some_and(|c| c.is_ascii_digit()))
                    {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            words[i..].to_vec()
        })
        .find(|w| w.first().is_some_and(|p| !SETUP.contains(p)))
        .unwrap_or_default()
}

/// A path that is the run's own plumbing rather than the tree: its
/// scratch directory and the harness's persisted tool outputs.
pub(crate) fn is_scratch(path: &str) -> bool {
    path.starts_with("/tmp/") || path.contains("/.claude/projects/")
}

/// A path as the tree names it: past a worktree's root when it is in
/// one, past `cwd` when it is under it, else as written — so two runs'
/// reads of one file, from two worktrees, are one row.
pub(crate) fn repo_relative(path: &str, cwd: Option<&str>) -> String {
    const WT: &str = "/.claude/worktrees/";
    if let Some(at) = path.find(WT) {
        let rest = &path[at + WT.len()..];
        if let Some((_, inside)) = rest.split_once('/') {
            return inside.to_string();
        }
    }
    cwd.and_then(|c| path.strip_prefix(&format!("{}/", c.trim_end_matches('/'))))
        .unwrap_or(path)
        .to_string()
}

fn bash_call(cmd: &str) -> Call {
    let words = working_words(cmd);
    let program = words.first().map(|w| basename(unquote(w))).unwrap_or("");
    let second = words.get(1).map(|w| unquote(w)).unwrap_or("");
    let class_only = |class| Call {
        class,
        search: false,
        reads: Vec::new(),
    };
    let in_place = words
        .iter()
        .any(|w| *w == "-i" || w.starts_with("-i.") || *w == "-pi" || *w == "--in-place");
    if (program == "sed" || program == "perl") && in_place {
        return class_only(Class::Edit);
    }
    let script = words
        .iter()
        .skip(1)
        .map(|w| unquote(w))
        .find(|w| !w.starts_with('-'))
        .map(basename)
        .unwrap_or("");
    let runs_build_script = (program == "bash" || program == "sh")
        && BUILD_SCRIPT_WORDS.iter().any(|w| script.contains(w));
    if BUILD_PROGRAMS.contains(&program)
        || program.ends_with("gate.sh")
        || (program == "boss" && second == "gate")
        || runs_build_script
    {
        return class_only(Class::BuildTest);
    }
    let search = SEARCH_PROGRAMS.contains(&program) || (program == "git" && second == "grep");
    let read = search
        || READ_PROGRAMS.contains(&program)
        || (program == "git" && GIT_READS.contains(&second))
        || (program == "boss-api" && second == "GET");
    if !read {
        return class_only(Class::Other);
    }
    let reads = if FILE_READERS.contains(&program) {
        // sed's first bare word is its script, not a file.
        let skip = usize::from(program == "sed");
        words
            .iter()
            .skip(1)
            .map(|w| unquote(w))
            .filter(|w| !w.starts_with('-'))
            .skip(skip)
            .filter(|w| w.contains('/') && !w.contains(['>', '<', '$', '*']))
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    Call {
        class: Class::SearchRead,
        search,
        reads,
    }
}

/// Classify one `tool_use` block by its tool name and input.
pub(crate) fn classify(name: &str, input: &Value) -> Call {
    let text = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let plain = |class, search| Call {
        class,
        search,
        reads: Vec::new(),
    };
    match name {
        "Bash" => bash_call(text("command")),
        "Read" | "NotebookRead" => {
            let path = [text("file_path"), text("notebook_path")]
                .into_iter()
                .find(|p| !p.is_empty())
                .unwrap_or("");
            Call {
                class: Class::SearchRead,
                search: false,
                reads: (!path.is_empty())
                    .then(|| path.to_string())
                    .into_iter()
                    .collect(),
            }
        }
        "Grep" | "Glob" | "WebSearch" => plain(Class::SearchRead, true),
        "LS" | "WebFetch" => plain(Class::SearchRead, false),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = [text("file_path"), text("notebook_path")]
                .into_iter()
                .find(|p| !p.is_empty())
                .unwrap_or("");
            if is_scratch(path) {
                plain(Class::Other, false)
            } else {
                plain(Class::Edit, false)
            }
        }
        _ => plain(Class::Other, false),
    }
}

/// The result text as the model received it.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A search that found nothing: no text, the harness's own "no output"
/// line, the search tools' own "none" answers, or grep's bare exit 1.
pub(crate) fn is_empty_result(text: &str) -> bool {
    matches!(
        text.trim(),
        "" | "(Bash completed with no output)"
            | "No matches found"
            | "No files found"
            | "Exit code 1"
    )
}

fn stamp(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
}

fn content_blocks(v: &Value) -> impl Iterator<Item = &Value> {
    v.pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// One run's profile, from its transcript's JSONL. Lines that are not
/// JSON, and blocks that are not tool calls or results, are skipped. A
/// call whose result never arrived counts as a call with no time and
/// no bytes; a result naming no call it follows is ignored.
pub(crate) fn work_profile(jsonl: &str) -> WorkProfile {
    struct Open {
        class: Class,
        search: bool,
        at: Option<chrono::DateTime<chrono::Utc>>,
    }
    let mut open: HashMap<String, Open> = HashMap::new();
    let mut by_class = ByClass::default();
    let mut calls = 0u64;
    let mut first_edit: Option<u64> = None;
    let mut searches = 0u64;
    let mut empty = 0u64;
    let mut reads: HashMap<String, u64> = HashMap::new();
    let add = |b: ByClass, c: Class, t: ClassTotal| -> ByClass {
        match c {
            Class::SearchRead => ByClass {
                search_read: b.search_read.plus(t),
                ..b
            },
            Class::BuildTest => ByClass {
                build_test: b.build_test.plus(t),
                ..b
            },
            Class::Edit => ByClass {
                edit: b.edit.plus(t),
                ..b
            },
            Class::Other => ByClass {
                other: b.other.plus(t),
                ..b
            },
        }
    };
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = stamp(&v);
        let cwd = v.get("cwd").and_then(Value::as_str);
        for block in content_blocks(&v) {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if open.contains_key(id) {
                        continue;
                    }
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let call = classify(name, block.get("input").unwrap_or(&Value::Null));
                    if call.class == Class::Edit && first_edit.is_none() {
                        first_edit = Some(calls);
                    }
                    calls += 1;
                    by_class = add(
                        by_class,
                        call.class,
                        ClassTotal {
                            calls: 1,
                            ..ClassTotal::default()
                        },
                    );
                    if call.search {
                        searches += 1;
                    }
                    for path in call.reads.iter().filter(|p| !is_scratch(p)) {
                        *reads.entry(repo_relative(path, cwd)).or_default() += 1;
                    }
                    open.insert(
                        id.to_string(),
                        Open {
                            class: call.class,
                            search: call.search,
                            at,
                        },
                    );
                }
                Some("tool_result") => {
                    let Some(call) = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| open.get(id))
                    else {
                        continue;
                    };
                    let text = result_text(block.get("content"));
                    let wall_ms = match (call.at, at) {
                        (Some(from), Some(to)) => {
                            u64::try_from((to - from).num_milliseconds()).unwrap_or(0)
                        }
                        _ => 0,
                    };
                    if call.search && is_empty_result(&text) {
                        empty += 1;
                    }
                    by_class = add(
                        by_class,
                        call.class,
                        ClassTotal {
                            calls: 0,
                            wall_ms,
                            result_bytes: text.len() as u64,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    WorkProfile {
        tool_calls: calls,
        by_class,
        calls_before_first_edit: first_edit,
        searches,
        empty_searches: empty,
        top_files_read: rank_files(
            reads
                .into_iter()
                .map(|(path, reads)| FileReads { path, reads })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run in miniature, in the shape the pod's transcripts have: a
    /// scratch write, two searches (one empty), two reads of one file
    /// from a worktree, an edit, and a build — each result arriving on
    /// its own `user` line with its own timestamp.
    fn transcript() -> String {
        let use_line = |ts: &str, id: &str, name: &str, input: Value| {
            serde_json::json!({
                "type": "assistant",
                "timestamp": ts,
                "cwd": "/work/boss/.claude/worktrees/agent-x",
                "message": { "id": format!("msg-{id}"), "content": [
                    { "type": "tool_use", "id": id, "name": name, "input": input }
                ]}
            })
            .to_string()
        };
        let result_line = |ts: &str, id: &str, content: Value| {
            serde_json::json!({
                "type": "user",
                "timestamp": ts,
                "message": { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": id, "content": content, "is_error": false }
                ]}
            })
            .to_string()
        };
        let src = "/work/boss/.claude/worktrees/agent-x/crates/a/src/lib.rs";
        [
            use_line(
                "2026-09-26T05:00:00.000Z",
                "t1",
                "Write",
                // shared-tmp-ok: text inside a transcript fixture the
                // classifier reads; nothing opens or creates this path.
                serde_json::json!({ "file_path": "/tmp/claude-0/s/run-1/commit-msg.txt", "content": "x" }),
            ),
            result_line("2026-09-26T05:00:00.100Z", "t1", serde_json::json!("File created")),
            use_line(
                "2026-09-26T05:00:01.000Z",
                "t2",
                "Bash",
                serde_json::json!({ "command": "cd /work/boss/.claude/worktrees/agent-x && grep -rn needle crates/" }),
            ),
            result_line("2026-09-26T05:00:01.500Z", "t2", serde_json::json!("crates/a/src/lib.rs:3:needle")),
            use_line(
                "2026-09-26T05:00:02.000Z",
                "t3",
                "Bash",
                serde_json::json!({ "command": "grep -rn missing crates/" }),
            ),
            result_line("2026-09-26T05:00:02.200Z", "t3", serde_json::json!("(Bash completed with no output)")),
            use_line(
                "2026-09-26T05:00:03.000Z",
                "t4",
                "Read",
                serde_json::json!({ "file_path": src }),
            ),
            result_line("2026-09-26T05:00:03.050Z", "t4", serde_json::json!("0123456789")),
            use_line(
                "2026-09-26T05:00:04.000Z",
                "t5",
                "Bash",
                serde_json::json!({ "command": format!("sed -n '1,80p' {src}") }),
            ),
            result_line("2026-09-26T05:00:04.050Z", "t5", serde_json::json!([{ "type": "text", "text": "abcde" }])),
            use_line(
                "2026-09-26T05:00:05.000Z",
                "t6",
                "Edit",
                serde_json::json!({ "file_path": src, "old_string": "a", "new_string": "b" }),
            ),
            result_line("2026-09-26T05:00:05.020Z", "t6", serde_json::json!("ok")),
            use_line(
                "2026-09-26T05:00:06.000Z",
                "t7",
                "Bash",
                serde_json::json!({ "command": "wt-cargo test -p boss-a > /tmp/claude-0/s/run-1/t.log 2>&1" }),
            ),
            result_line("2026-09-26T05:01:36.000Z", "t7", serde_json::json!("(Bash completed with no output)")),
            "not json".to_string(),
        ]
        .join("\n")
    }

    /// THE PROFILE, read off the shape the transcript really has: the
    /// scratch write is plumbing (other), the first edit is the sixth
    /// call, the build holds the time, the searches and reads hold the
    /// bytes, and one of the two searches found nothing.
    #[test]
    fn a_transcript_is_profiled_by_class_with_wall_time_and_result_bytes() {
        let p = work_profile(&transcript());
        assert_eq!(p.tool_calls, 7);
        assert_eq!(
            p.calls_before_first_edit,
            Some(5),
            "the scratch write is not the first edit"
        );
        assert_eq!(p.by_class.other.calls, 1);
        assert_eq!(p.by_class.edit.calls, 1);
        assert_eq!(p.by_class.build_test.calls, 1);
        assert_eq!(p.by_class.build_test.wall_ms, 90_000);
        assert_eq!(p.by_class.search_read.calls, 4);
        assert_eq!(p.by_class.search_read.wall_ms, 500 + 200 + 50 + 50);
        assert_eq!(
            p.by_class.search_read.result_bytes,
            ("crates/a/src/lib.rs:3:needle".len()
                + "(Bash completed with no output)".len()
                + 10
                + 5) as u64
        );
        assert_eq!(p.searches, 2);
        assert_eq!(p.empty_searches, 1);
        assert_eq!(
            p.top_files_read,
            vec![FileReads {
                path: "crates/a/src/lib.rs".into(),
                reads: 2
            }],
            "the Read and the sed of one file are one row, repo-relative"
        );
    }

    #[test]
    fn a_run_that_edits_nothing_says_so() {
        let jsonl = serde_json::json!({
            "type": "assistant", "timestamp": "2026-09-26T05:00:00Z",
            "message": { "content": [
                { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "git log --oneline -3" } }
            ]}
        })
        .to_string();
        let p = work_profile(&jsonl);
        assert_eq!(p.tool_calls, 1);
        assert_eq!(p.calls_before_first_edit, None);
        assert_eq!(p.by_class.search_read.calls, 1);
        assert_eq!(p.by_class.search_read.wall_ms, 0, "no result, no time");
    }

    /// The Bash classes, one command each, in the spellings the pod's
    /// transcripts use.
    #[test]
    fn a_bash_command_is_classed_by_the_program_it_runs() {
        let class = |cmd: &str| bash_call(cmd).class;
        assert_eq!(class("cd /w && wt-cargo test -p x"), Class::BuildTest);
        assert_eq!(class("timeout 600 cargo build"), Class::BuildTest);
        assert_eq!(
            class("bash /tmp/s/run-1/gate.sh > /tmp/s/run-1/gate.log 2>&1"),
            Class::BuildTest
        );
        assert_eq!(class("bash infra/gate.sh --lint"), Class::BuildTest);
        assert_eq!(class("boss gate fix/x --wait"), Class::BuildTest);
        assert_eq!(class("wt-web bash /tmp/s/web.sh"), Class::BuildTest);
        assert_eq!(class("git show origin/main:README.md"), Class::SearchRead);
        assert_eq!(class("boss-api GET /api/jobs/x"), Class::SearchRead);
        assert_eq!(class("ls -t /a/*.jsonl | head -5"), Class::SearchRead);
        assert_eq!(class("sed -i 's/a/b/' crates/x.rs"), Class::Edit);
        assert_eq!(class("git commit -F /tmp/m.txt"), Class::Other);
        assert_eq!(class("git push origin HEAD:fix/x"), Class::Other);
        assert_eq!(class("bash /tmp/s/probe.sh"), Class::Other);
        assert_eq!(class("mkdir -p /tmp/s/run-1"), Class::Other);
        assert!(bash_call("find . -name '*.rs'").search);
        assert!(bash_call("git grep -n needle").search);
        assert!(!bash_call("cat a/b.rs").search, "a read names its file");
    }

    #[test]
    fn a_path_is_named_as_the_tree_names_it() {
        assert_eq!(
            repo_relative("/work/boss/.claude/worktrees/agent-a/crates/x.rs", None),
            "crates/x.rs"
        );
        assert_eq!(
            repo_relative("/work/boss/crates/x.rs", Some("/work/boss")),
            "crates/x.rs"
        );
        assert_eq!(
            repo_relative("/etc/hosts", Some("/work/boss")),
            "/etc/hosts"
        );
    }
}
