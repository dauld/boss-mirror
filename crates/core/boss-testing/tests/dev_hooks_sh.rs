//! `infra/dev/hooks/*.sh` — the Claude Code hooks that make an
//! operator's session and its dispatches a record (design 511fa7d4,
//! decided 2026-09-18; car 2b of c87fb59b, backlog da925366).
//!
//! `.claude/settings.json` (project scope, checked in) registers five
//! hooks; each is a script here, run by Claude Code with the hook's
//! payload on stdin. What they must do, and what this file pins:
//!
//! - `session-start.sh` files a `work-session` packet (`boss job file`)
//!   carrying the actor, host, cwd, instant and source, and remembers
//!   its id under the session's own id so the other hooks find it.
//!   A resumed session keeps its packet.
//! - `prompt-submit.sh` writes the heartbeat (`boss job patch`:
//!   `last_active_at`, `prompt_count`) and NEVER the prompt text —
//!   the one privacy rule this car ships, pinned below by planting a
//!   sentence and asserting it reaches nothing.
//! - `agent-start.sh` pipes a PreToolUse payload into `boss dispatch
//!   <session> --from-hook`, prints the verb's stdout (the updatedInput
//!   answer) and nothing else, and stores the run id the verb names
//!   under the call's `tool_use_id`.
//! - `agent-stop.sh` reports that run when the Agent tool returns
//!   (`boss dispatch <run> --report --summary … --tokens IN,OUT`).
//! - `session-end.sh` completes the packet's `active` step with
//!   `ended = clean` through `boss-api`, inside SessionEnd's 1.5 s
//!   budget.
//!
//! THE RULE THEY ALL SHARE: exit 0, whatever happened. A hook that
//! exits 2 blocks the operator's action, and an unreachable system of
//! record, a missing `boss`, or an unparseable payload must never do
//! that — visibility is best-effort; the executor never waits on it
//! (CLAUDE.md §Diagnosis: an arm that needs the patient is not an
//! arm). Pinned by running every hook against a `boss` that fails, and
//! against no `boss` at all.
//!
//! A stub `boss` and `boss-api` on PATH record every call (argv, one
//! per line, and stdin) under scratch, so each assertion reads what the
//! hook actually asked the door to do. The stub `boss job file` answers
//! the real verb's confirmation line, because that line is what the
//! session-start hook parses the packet id from.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOOKS: &str = "infra/dev/hooks";
const SETTINGS: &str = ".claude/settings.json";
const SESSION: &str = "7a1e2b3c-0000-4000-8000-00000000abcd";
const RUN: &str = "5b1d2c3e-0000-4000-8000-000000000001";

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("dev-hooks-{name}"));
        let bin = root.join("bin");
        let state = root.join("state");
        boss_testing::create_dir(&bin);
        boss_testing::create_dir(&state);
        Self { root, bin, state }
    }

    /// A `boss` that records argv and stdin, answers `job file` with
    /// the real confirmation line, and `dispatch --from-hook` with an
    /// updatedInput answer on stdout and the run line on stderr. With
    /// `fails`, every call exits 1 after recording.
    fn stub_boss(&self, fails: bool) {
        let log = self.root.join("boss-calls");
        let exit = if fails { "exit 1" } else { "exit 0" };
        write_exec(
            &self.bin.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=$(ls \"{log}\" 2>/dev/null | wc -l)\n\
                 d=\"{log}/$n\"; mkdir -p \"$d\"\n\
                 for a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$d/argv\"\n\
                 printf '%s' \"${{BOSS_SESSION_AGENT_DEFINITIONS:-}}\" > \"$d/definitions_env\"\n\
                 cat > \"$d/stdin\"\n\
                 [ \"$2\" = patch ] && cp \"$4\" \"$d/body\"\n\
                 {exit_early}\n\
                 case \"$1 $2\" in\n\
                   'job file') echo 'boss job: filed {SESSION}  \"Session: emp-david on boss-dev-0\" — confirmed by reading it back' ;;\n\
                   'dispatch {SESSION}'|'dispatch -') \n\
                      if grep -q Packet: \"$d/stdin\"; then\n\
                        echo 'agent_run={RUN}' >&2\n\
                        printf '%s' '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{{\"prompt\":\"Packet: da925366\\n== THE RUN ==\"}}}}}}'\n\
                      else\n\
                        echo 'boss dispatch --from-hook: the prompt names no packet' >&2\n\
                      fi ;;\n\
                 esac\n\
                 exit 0\n",
                log = log.display(),
                exit_early = if fails { exit } else { ":" },
            ),
        );
        let api_log = self.root.join("api-calls");
        write_exec(
            &self.bin.join("boss-api"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=$(ls \"{log}\" 2>/dev/null | wc -l)\n\
                 d=\"{log}/$n\"; mkdir -p \"$d\"\n\
                 for a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$d/argv\"\n\
                 [ -n \"$3\" ] && cp \"$3\" \"$d/body\"\n\
                 printf '%s' \"${{BOSS_DOOR_FRESHNESS:-}}\" > \"$d/freshness\"\n\
                 {exit_early}\n\
                 case \"$1\" in\n\
                   GET) printf '%s' '{{\"id\":\"{SESSION}\",\"kind\":\"work-session\",\"steps\":[{{\"id\":\"st-opened\",\"spec_slug\":\"opened\",\"status\":\"completed\"}},{{\"id\":\"st-active\",\"spec_slug\":\"active\",\"status\":\"ready\",\"metadata\":{{\"authority_role\":\"platform-admin\"}}}}]}}' ;;\n\
                 esac\n\
                 echo 'HTTP:200' >&2\n\
                 exit 0\n",
                log = api_log.display(),
                exit_early = if fails { exit } else { ":" },
            ),
        );
    }

    fn calls(&self, which: &str) -> Vec<(Vec<String>, String)> {
        let dir = self.root.join(which);
        let mut n = 0;
        let mut out = Vec::new();
        while dir.join(n.to_string()).is_dir() {
            let d = dir.join(n.to_string());
            // NUL-separated: a summary spans lines.
            let argv = std::fs::read_to_string(d.join("argv"))
                .unwrap_or_default()
                .split('\0')
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .collect();
            // The file the verb was handed, else what it read on stdin.
            let body = std::fs::read_to_string(d.join("body"))
                .or_else(|_| std::fs::read_to_string(d.join("stdin")))
                .unwrap_or_default();
            out.push((argv, body));
            n += 1;
        }
        out
    }

    /// What `BOSS_SESSION_AGENT_DEFINITIONS` held for the n-th `boss`
    /// call — the snapshot the dispatch door reads (backlog e1c4dc93).
    fn definitions_env(&self, n: usize) -> String {
        std::fs::read_to_string(
            self.root
                .join("boss-calls")
                .join(n.to_string())
                .join("definitions_env"),
        )
        .unwrap_or_default()
    }

    /// What `BOSS_DOOR_FRESHNESS` held for the n-th `boss-api` call.
    fn api_freshness(&self, n: usize) -> String {
        std::fs::read_to_string(
            self.root
                .join("api-calls")
                .join(n.to_string())
                .join("freshness"),
        )
        .unwrap_or_default()
    }

    /// Put the stub `boss-api` where the pod's real one lives: behind
    /// a symlink into a checkout one commit behind an `origin/main`
    /// that changed it — so `door_is_stale` calls it stale, as it did
    /// the pod's copy for most of 2026-09-19 (backlog 0b36dd65).
    fn make_boss_api_stale(&self) {
        let checkout = self.root.join("checkout");
        boss_testing::create_dir(&checkout);
        let door = checkout.join("boss-api");
        std::fs::rename(self.bin.join("boss-api"), &door).expect("move the stub");
        git(&checkout, &["init", "-q", "-b", "main"]);
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-qm", "the door"]);
        let old = git(&checkout, &["rev-parse", "HEAD"]);
        let body = std::fs::read_to_string(&door).expect("read the stub");
        write_exec(&door, &format!("{body}# changed on origin/main\n"));
        git(&checkout, &["commit", "-qam", "the door, changed"]);
        let main = git(&checkout, &["rev-parse", "HEAD"]);
        git(
            &checkout,
            &["update-ref", "refs/remotes/origin/main", &main],
        );
        git(&checkout, &["reset", "-q", "--hard", &old]);
        std::os::unix::fs::symlink(&door, self.bin.join("boss-api")).expect("symlink the door");
    }

    /// Run one hook with `payload` on stdin and the stub bin dir on
    /// PATH (`with_bin`), or a PATH with no `boss` at all.
    fn run(&self, hook: &str, payload: &str, with_bin: bool) -> (i32, String, String) {
        let path = if with_bin {
            format!("{}:/usr/bin:/bin", self.bin.display())
        } else {
            "/usr/bin:/bin".to_string()
        };
        let mut child = Command::new(repo_root().join(HOOKS).join(hook))
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("BOSS_HOOK_STATE", &self.state)
            .env("BOSS_ACTOR", "emp-david")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("run {hook}: {e}"));
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(payload.as_bytes()).expect("write payload");
        }
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=t@test", "-c", "user.name=test"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn start_payload(source: &str) -> String {
    format!(
        r#"{{"session_id":"s-1","transcript_path":"/x/t.jsonl","cwd":"/work/boss","hook_event_name":"SessionStart","source":"{source}"}}"#
    )
}

fn start_payload_in(source: &str, cwd: &Path) -> String {
    format!(
        r#"{{"session_id":"s-1","transcript_path":"/x/t.jsonl","cwd":"{cwd}","hook_event_name":"SessionStart","source":"{source}"}}"#,
        cwd = cwd.display()
    )
}

fn hook_scripts() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo_root().join(HOOKS))
        .expect("the hooks directory exists")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sh") && n != "lib.sh")
        .collect();
    names.sort();
    names
}

/// The five hooks the design names, executable, each registered in
/// the project settings under the event it serves — and the settings
/// file is tracked (the `.claude/` ignore has an exception for it).
#[test]
fn the_five_hooks_are_in_the_tree_and_registered() {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        hook_scripts(),
        vec![
            "agent-start.sh",
            "agent-stop.sh",
            "prompt-submit.sh",
            "session-end.sh",
            "session-start.sh",
        ]
    );
    for name in hook_scripts() {
        let path = repo_root().join(HOOKS).join(&name);
        let mode = std::fs::metadata(&path)
            .expect("hook exists")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{name} must be executable");
    }
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(SETTINGS)).expect(".claude/settings.json"),
    )
    .expect("settings.json is JSON");
    let command_for = |event: &str| -> Vec<(String, String)> {
        settings["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("hooks.{event} is registered"))
            .iter()
            .flat_map(|group| {
                let matcher = group["matcher"].as_str().unwrap_or("").to_string();
                group["hooks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |h| {
                        (
                            matcher.clone(),
                            h["command"].as_str().unwrap_or("").to_string(),
                        )
                    })
            })
            .collect()
    };
    let expect = [
        ("SessionStart", "", "session-start.sh"),
        ("UserPromptSubmit", "", "prompt-submit.sh"),
        ("PreToolUse", "Agent", "agent-start.sh"),
        ("PostToolUse", "Agent", "agent-stop.sh"),
        ("SessionEnd", "", "session-end.sh"),
    ];
    for (event, matcher, script) in expect {
        let found = command_for(event);
        assert!(
            found.iter().any(|(m, c)| m == matcher
                && c.contains(&format!("{HOOKS}/{script}"))
                && c.contains("$CLAUDE_PROJECT_DIR")),
            "{event} runs {script} (matcher {matcher:?}) from $CLAUDE_PROJECT_DIR: {found:?}"
        );
    }
    // Additive: the allow entries the operator's untracked copy held
    // on 2026-09-19 ride along, so replacing that file loses nothing.
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("permissions.allow is kept");
    assert!(allow.iter().any(|a| a == "Bash(boss train cancel:*)"));
    let ignore = std::fs::read_to_string(repo_root().join(".gitignore")).expect(".gitignore");
    assert!(
        ignore.lines().any(|l| l == "!.claude/settings.json"),
        "the settings file is the one tracked thing under .claude/"
    );
    let tracked = Command::new("git")
        .current_dir(repo_root())
        .args(["ls-files", "--error-unmatch", SETTINGS])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git");
    assert!(tracked.success(), "{SETTINGS} is tracked");
}

#[test]
fn session_start_files_the_packet_and_a_resume_keeps_it() {
    let f = Fixture::new("start");
    f.stub_boss(false);
    let (code, out, err) = f.run("session-start.sh", &start_payload("startup"), true);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out, "",
        "nothing on stdout — it would be added to the model's context"
    );
    let calls = f.calls("boss-calls");
    assert_eq!(calls.len(), 1, "{calls:?}");
    let argv = &calls[0].0;
    assert_eq!(&argv[..4], ["job", "file", "--kind", "work-session"]);
    let md_path = argv
        .iter()
        .position(|a| a == "--metadata")
        .map(|i| &argv[i + 1])
        .expect("--metadata <file>");
    let md: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(md_path).expect("metadata file"))
            .expect("metadata is JSON");
    assert_eq!(md["actor"], "emp-david");
    assert_eq!(md["cwd"], "/work/boss");
    assert_eq!(md["session_id"], "s-1");
    assert_eq!(md["source"], "startup");
    assert!(md["host"].as_str().is_some_and(|h| !h.is_empty()));
    assert!(md["started_at"].as_str().is_some_and(|t| t.ends_with('Z')));
    assert_eq!(md["prompt_count"], 0);
    assert_eq!(
        std::fs::read_to_string(f.state.join("s-1").join("packet"))
            .expect("the packet id is remembered")
            .trim(),
        SESSION
    );
    assert!(
        err.contains(SESSION),
        "the journal line names the packet: {err}"
    );

    // A resume finds the packet and files nothing.
    let (code, _, err) = f.run("session-start.sh", &start_payload("resume"), true);
    assert_eq!(code, 0, "{err}");
    assert_eq!(f.calls("boss-calls").len(), 1, "no second packet");
}

#[test]
fn prompt_submit_heartbeats_without_the_prompt_text() {
    let f = Fixture::new("prompt");
    f.stub_boss(false);
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    let secret = "the tenant's bank balance is forty";
    let payload = format!(
        r#"{{"session_id":"s-1","cwd":"/work/boss","hook_event_name":"UserPromptSubmit","prompt":"{secret}"}}"#
    );
    for _ in 0..2 {
        let (code, out, err) = f.run("prompt-submit.sh", &payload, true);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "", "stdout would be added to the model's context");
        assert!(!err.contains(secret));
    }
    let calls = f.calls("boss-calls");
    assert_eq!(calls.len(), 2, "{calls:?}");
    for (i, (argv, _)) in calls.iter().enumerate() {
        assert_eq!(&argv[..3], ["job", "patch", SESSION]);
        let patch: serde_json::Value =
            serde_json::from_str(&calls[i].1).expect("the patch file handed to the verb is JSON");
        assert_eq!(patch["prompt_count"], (i + 1) as u64);
        assert!(
            patch["last_active_at"]
                .as_str()
                .is_some_and(|t| t.ends_with('Z'))
        );
        let text = patch.to_string();
        assert!(
            !text.contains(secret),
            "the prompt text never reaches the record: {text}"
        );
        assert!(!text.contains("prompt\""), "{text}");
    }
    // Every planted file under state is checked too.
    for entry in walkdir(&f.state) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(
            !text.contains(secret),
            "{} holds the prompt",
            entry.display()
        );
    }
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn agent_payload(event: &str, prompt: &str, extra: &str) -> String {
    format!(
        r#"{{"session_id":"s-1","cwd":"/work/boss","hook_event_name":"{event}","tool_name":"Agent","tool_use_id":"toolu_01","tool_input":{{"prompt":"{prompt}","description":"build"}}{extra}}}"#
    )
}

#[test]
fn agent_start_dispatches_through_the_door_and_hands_back_its_answer() {
    let f = Fixture::new("agent-start");
    f.stub_boss(false);
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    let (code, out, err) = f.run(
        "agent-start.sh",
        &agent_payload("PreToolUse", "Build it.\\nPacket: da925366", ""),
        true,
    );
    assert_eq!(code, 0, "{err}");
    let answer: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("stdout is the verb's answer, only: {e}: {out:?}"));
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let calls = f.calls("boss-calls");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, ["dispatch", SESSION, "--from-hook"]);
    assert!(
        calls[0].1.contains("\"tool_name\":\"Agent\""),
        "the payload is piped through verbatim"
    );
    assert_eq!(
        std::fs::read_to_string(f.state.join("s-1").join("runs").join("toolu_01"))
            .expect("the run is remembered under the call")
            .trim(),
        RUN
    );
    assert!(
        !err.contains("agent_run="),
        "the run line is consumed, not echoed: {err}"
    );

    // No packet in the prompt: the door counts it, the hook prints
    // nothing, and no run is remembered.
    let (code, out, _) = f.run(
        "agent-start.sh",
        &agent_payload("PreToolUse", "Find where the yard reads gate-runs.", ""),
        true,
    );
    assert_eq!(code, 0);
    assert_eq!(out, "");
    assert!(!f.state.join("s-1").join("runs").join("toolu_02").exists());
    // No session packet at all: the door is still called, with `-`.
    let f2 = Fixture::new("agent-start-no-session");
    f2.stub_boss(false);
    let (code, _, _) = f2.run(
        "agent-start.sh",
        &agent_payload("PreToolUse", "Packet: da925366", ""),
        true,
    );
    assert_eq!(code, 0);
    assert_eq!(
        f2.calls("boss-calls")[0].0,
        ["dispatch", "-", "--from-hook"]
    );
}

/// WHAT THIS SESSION LOADED, snapshotted at the one moment Claude Code
/// reads the definitions directory (backlog e1c4dc93). A car that adds
/// `.claude/agents/effort-high.md` lands under sessions already
/// running; those sessions cannot load it, and the dispatch door would
/// name it on the Agent call. Measured live 2026-09-22: the harness
/// answers `Agent type 'effort-ultra-nonexistent' not found. Available
/// agents: …` — loud and immediate, never a silent fallback — but only
/// after the door has claimed the step and filed the run. The snapshot
/// is what lets the door refuse first instead. A RESUME loads the
/// directory again, so it is written there too.
#[test]
fn session_start_snapshots_the_definitions_this_session_loaded() {
    let f = Fixture::new("start-definitions");
    f.stub_boss(false);
    let project = f.root.join("project");
    let agents = project.join(".claude").join("agents");
    boss_testing::create_dir(&agents);
    for name in ["effort-low", "effort-medium"] {
        std::fs::write(agents.join(format!("{name}.md")), "---\n").expect("a definition");
    }
    std::fs::write(agents.join("notes.txt"), "not a definition").expect("a stray file");

    let (code, _, err) = f.run(
        "session-start.sh",
        &start_payload_in("startup", &project),
        true,
    );
    assert_eq!(code, 0, "{err}");
    let snapshot = f.state.join("s-1").join("definitions");
    let loaded = std::fs::read_to_string(&snapshot).expect("the snapshot is written");
    let mut names: Vec<&str> = loaded.split_whitespace().collect();
    names.sort_unstable();
    assert_eq!(names, ["effort-low", "effort-medium"], "{loaded:?}");

    // A resume reads the directory again — and the definition that
    // landed since is in the snapshot the resumed session gets.
    std::fs::write(agents.join("effort-high.md"), "---\n").expect("a third definition");
    let (code, _, err) = f.run(
        "session-start.sh",
        &start_payload_in("resume", &project),
        true,
    );
    assert_eq!(code, 0, "{err}");
    let loaded = std::fs::read_to_string(&snapshot).expect("the snapshot is rewritten");
    assert!(
        loaded.split_whitespace().any(|n| n == "effort-high"),
        "{loaded:?}"
    );

    // No definitions directory at all: an EMPTY snapshot, which says
    // this session loaded none — not an ABSENT one, which says nothing
    // is known and refuses nothing.
    let f2 = Fixture::new("start-definitions-absent");
    f2.stub_boss(false);
    let bare = f2.root.join("bare");
    boss_testing::create_dir(&bare);
    let (code, _, err) = f2.run(
        "session-start.sh",
        &start_payload_in("startup", &bare),
        true,
    );
    assert_eq!(code, 0, "{err}");
    let snapshot = f2.state.join("s-1").join("definitions");
    assert!(
        snapshot.is_file(),
        "the snapshot exists even when the directory does not"
    );
    assert_eq!(
        std::fs::read_to_string(&snapshot).expect("readable").trim(),
        ""
    );
}

/// The dispatch door reads the snapshot through the environment, so
/// the hook that has the session's state directory is the one that
/// names the file (backlog e1c4dc93).
#[test]
fn agent_start_hands_the_door_this_sessions_definitions() {
    let f = Fixture::new("agent-start-definitions");
    f.stub_boss(false);
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    std::fs::write(f.state.join("s-1").join("definitions"), "effort-high\n").unwrap();
    let (code, _, err) = f.run(
        "agent-start.sh",
        &agent_payload("PreToolUse", "Build it.\\nPacket: da925366", ""),
        true,
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        f.definitions_env(0),
        f.state
            .join("s-1")
            .join("definitions")
            .display()
            .to_string()
    );
}

#[test]
fn agent_stop_reports_the_run_it_remembered() {
    let f = Fixture::new("agent-stop");
    f.stub_boss(false);
    let runs = f.state.join("s-1").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("toolu_01"), format!("{RUN}\n")).unwrap();
    let response = r#","tool_response":{"status":"completed","agentId":"a4d2","content":[{"type":"text","text":"Packet da925366: branch feat/x, sha abc, gate 1234."},{"type":"text","text":"Nothing worth a follow-up."}],"totalTokens":12450,"totalDurationMs":48211,"usage":{"input_tokens":8320,"output_tokens":4130}}"#;
    let (code, out, err) = f.run(
        "agent-stop.sh",
        &agent_payload("PostToolUse", "Packet: da925366", response),
        true,
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    let calls = f.calls("boss-calls");
    assert_eq!(calls.len(), 1, "{calls:?}");
    let argv = &calls[0].0;
    assert_eq!(&argv[..3], ["dispatch", RUN, "--report"]);
    let after = |flag: &str| {
        argv.iter()
            .position(|a| a == flag)
            .map(|i| argv[i + 1].clone())
    };
    let summary = after("--summary").expect("--summary");
    assert!(summary.contains("branch feat/x, sha abc") && summary.contains("Nothing worth"));
    assert_eq!(after("--tokens").as_deref(), Some("8320,4130"));
    assert!(!runs.join("toolu_01").exists(), "reported once");

    // A call the start hook never recorded is not reported.
    let (code, _, _) = f.run(
        "agent-stop.sh",
        &agent_payload("PostToolUse", "x", response),
        true,
    );
    assert_eq!(code, 0);
    assert_eq!(f.calls("boss-calls").len(), 1);
}

#[test]
fn session_end_completes_active_as_clean_through_boss_api() {
    let f = Fixture::new("end");
    f.stub_boss(false);
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    let payload = r#"{"session_id":"s-1","cwd":"/work/boss","hook_event_name":"SessionEnd","reason":"prompt_input_exit"}"#;
    let (code, out, err) = f.run("session-end.sh", payload, true);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    let calls = f.calls("api-calls");
    assert!(calls.len() >= 2, "{calls:?}");
    assert_eq!(
        calls[0].0,
        ["GET".to_string(), format!("/api/jobs/{SESSION}")]
    );
    assert_eq!(calls[1].0[0], "PUT");
    assert_eq!(
        calls[1].0[1],
        format!("/api/jobs/{SESSION}/steps/st-active")
    );
    let body: serde_json::Value = serde_json::from_str(&calls[1].1).expect("PUT body is JSON");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["metadata"]["ended"], "clean");
    assert_eq!(
        body["metadata"]["authority_role"], "platform-admin",
        "the step's own keys are kept"
    );
    let patch = calls
        .iter()
        .find(|(a, _)| a[0] == "PATCH")
        .expect("ended_at is written");
    let md: serde_json::Value = serde_json::from_str(&patch.1).expect("PATCH body is JSON");
    assert_eq!(md["end_reason"], "prompt_input_exit");
    assert!(md["ended_at"].as_str().is_some_and(|t| t.ends_with('Z')));
    assert!(
        !f.state.join("s-1").exists(),
        "the session's state is cleared"
    );
}

/// A session ending while the pod's `boss-api` is a stale copy still
/// ends clean — and says it wrote past a stale door, on a line of its
/// own (backlog 584dc9da). Since 0b36dd65 a stale door REFUSES a write
/// with exit 78, and the pod's checkout was behind for most of
/// 2026-09-19, so without the override most sessions would fall to
/// the clock at the one moment nobody reads the journal. The distinct
/// line keeps the override countable: if closing a session past a
/// stale door is ever the wrong call, how often it happened is in the
/// journal rather than re-derived.
#[test]
fn session_end_writes_past_a_stale_door_and_says_so() {
    let f = Fixture::new("end-stale");
    f.stub_boss(false);
    f.make_boss_api_stale();
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    let payload = r#"{"session_id":"s-1","cwd":"/work/boss","hook_event_name":"SessionEnd","reason":"logout"}"#;
    let (code, out, err) = f.run("session-end.sh", payload, true);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    let calls = f.calls("api-calls");
    assert!(
        calls.iter().any(|(a, _)| a[0] == "PUT"),
        "the step is still completed: {calls:?}"
    );
    for (n, (argv, _)) in calls.iter().enumerate() {
        assert_eq!(
            f.api_freshness(n),
            "off",
            "boss-api {argv:?} ran without the override, so a stale door refuses it"
        );
    }
    assert!(
        err.contains("past a stale door"),
        "the write past a stale door is reported on its own line: {err}"
    );
    assert!(err.contains("ended clean"), "{err}");
}

/// A fresh door ends the session without the stale-door line — the
/// line counts stale doors, not session ends.
#[test]
fn session_end_on_a_fresh_door_says_nothing_about_freshness() {
    let f = Fixture::new("end-fresh");
    f.stub_boss(false);
    std::fs::create_dir_all(f.state.join("s-1")).unwrap();
    std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
    let payload = r#"{"session_id":"s-1","cwd":"/work/boss","hook_event_name":"SessionEnd","reason":"logout"}"#;
    let (code, _, err) = f.run("session-end.sh", payload, true);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("stale door"), "{err}");
    assert!(err.contains("ended clean"), "{err}");
}

/// Every hook exits 0 with a failing `boss`, with no `boss` on PATH,
/// and with a payload that is not JSON — and prints nothing on stdout.
#[test]
fn every_hook_exits_zero_whatever_the_door_answers() {
    let payloads: Vec<(&str, String)> = vec![
        ("session-start.sh", start_payload("startup")),
        (
            "prompt-submit.sh",
            r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#.into(),
        ),
        (
            "agent-start.sh",
            agent_payload("PreToolUse", "Packet: da925366", ""),
        ),
        (
            "agent-stop.sh",
            agent_payload("PostToolUse", "Packet: da925366", ""),
        ),
        (
            "session-end.sh",
            r#"{"session_id":"s-1","hook_event_name":"SessionEnd","reason":"other"}"#.into(),
        ),
    ];
    for (hook, payload) in &payloads {
        let f = Fixture::new(&format!("fail-{hook}"));
        f.stub_boss(true);
        std::fs::create_dir_all(f.state.join("s-1").join("runs")).unwrap();
        std::fs::write(f.state.join("s-1").join("packet"), format!("{SESSION}\n")).unwrap();
        std::fs::write(
            f.state.join("s-1").join("runs").join("toolu_01"),
            format!("{RUN}\n"),
        )
        .unwrap();
        let (code, out, err) = f.run(hook, payload, true);
        assert_eq!(code, 0, "{hook} with a failing boss: {err}");
        assert_eq!(out, "", "{hook} printed on stdout with a failing boss");

        let (code, out, _) = f.run(hook, payload, false);
        assert_eq!(code, 0, "{hook} with no boss on PATH");
        assert_eq!(out, "", "{hook} printed on stdout with no boss");

        let (code, out, _) = f.run(hook, "not json at all", true);
        assert_eq!(code, 0, "{hook} with a payload that is not JSON");
        assert_eq!(out, "");
    }
}
