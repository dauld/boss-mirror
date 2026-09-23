//! `boss dispatch <session|-> --from-hook` — the Agent tool's call,
//! recorded as a run.
//!
//! THE SHOP FLOOR (design 511fa7d4, decided 2026-09-18; car 2b of
//! c87fb59b, backlog da925366). Car 2 made `boss dispatch` the door
//! that hands a step to an agent as a packet. But the operator's
//! Agent-tool call — the thing that actually starts a builder — did not
//! go through it: the verb printed a prompt, the operator pasted it,
//! and whether the paste happened was in nobody's record. This door
//! closes that gap from the other side. Claude Code fires a PreToolUse
//! hook on every Agent call (`infra/dev/hooks/agent-start.sh`,
//! registered in `.claude/settings.json`); the hook pipes the hook's
//! stdin JSON into this verb, which reads the prompt the agent is ABOUT
//! to be given and does one of four things:
//!
//! - the prompt names no packet → an UNTRACKED run: counted on the
//!   session packet (`untracked_runs`), nothing filed, nothing changed.
//!   A hook that refused an Agent call because it could not parse it
//!   would be the executor waiting on its visibility, so the answer to
//!   "no packet" is a count, never a refusal.
//! - the prompt already carries a run (`Your run is agent-run <id>`,
//!   `boss dispatch`'s own run section — the operator ran the verb by
//!   hand and pasted its output) → the run EXISTS; only the session is
//!   written onto it. Filing a twin here was the obvious defect.
//! - the prompt names a packet — a `Packet: <ref>` line, or a builder
//!   brief path `…/builders/<ref>/brief.txt` — → a dispatch exactly as
//!   the hand verb does it (`dispatch::dispatch_at`), with the prompt
//!   itself as the brief (`BriefSource::Handed`: what the agent was
//!   told, verbatim, never re-rendered) and the session on the run.
//!   The run section is then HANDED BACK to the hook as Claude Code's
//!   `updatedInput` so the agent's prompt carries its run id and the
//!   BOSS_AGENT_RUN export, the same way the printed prompt does — the
//!   gate the builder launches then records the run, and the green
//!   lands it.
//! - not an Agent call at all, or an Agent call inside a subagent
//!   (`agent_id` on the payload: a builder spawning an explorer) →
//!   nothing.
//! - the prompt names a packet whose effort selects a definition THIS
//!   session never loaded (backlog e1c4dc93) → a DENY, before the
//!   claim. This is the one refusal here, and it refuses a call the
//!   harness is about to refuse anyway: Claude Code reads
//!   `.claude/agents/*.md` once, at session start, so a car that adds
//!   one lands under sessions already running. Measured live
//!   2026-09-22 — `Agent type 'effort-ultra-nonexistent' not found.
//!   Available agents: …` — the harness's answer is loud and
//!   immediate, never a silent fallback, so what the raw failure costs
//!   is not the diagnosis but the RESIDUE it leaves: a step claimed
//!   and a run filed for a builder that never started, which the
//!   restart that fixes the session does not clean up. Refusing first
//!   leaves nothing behind and says what to do. What the session
//!   loaded is `session-start.sh`'s snapshot, named by
//!   [`crate::dispatch::SESSION_DEFINITIONS_ENV`]; no snapshot means
//!   nothing is known, and an unknown is never a refusal.
//!
//! stdout is the hook's answer and nothing else: the updatedInput JSON
//! when there is one, empty otherwise. Every status line goes to
//! stderr, and the run's id goes there too as one machine-readable
//! `agent_run=<id>` line, which the hook stores under the call's
//! `tool_use_id` so the PostToolUse hook can report the run when the
//! agent returns.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

use crate::dispatch::{BriefSource, Dispatched, Isolation, Overrides, dispatch_at};

/// The tool whose calls are dispatches.
const AGENT_TOOL: &str = "Agent";

/// The one line the hook parses off stderr.
pub(crate) const RUN_LINE_PREFIX: &str = "agent_run=";

/// What the hook's payload asks for, read off the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// Not the Agent tool, or a nested call — nothing to record.
    NotADispatch { why: String },
    /// The prompt carries a run `boss dispatch` already filed.
    ExistingRun { run: String },
    /// The prompt names a packet: dispatch it, the prompt as the brief.
    Packet { packet_ref: String },
    /// An Agent call naming no packet: counted, never refused.
    Untracked,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// A packet reference as `boss job get` takes it: a full uuid or 8+
/// hex characters of one.
fn is_packet_ref(s: &str) -> bool {
    if s.len() == 36 {
        return s.split('-').map(str::len).eq([8, 4, 4, 4, 12])
            && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    }
    s.len() >= 8 && is_hex(s)
}

/// `agent-run <uuid>` anywhere in the prompt — the run section's own
/// phrase (`dispatch::run_section`).
fn existing_run(prompt: &str) -> Option<String> {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "agent-run" && w[1].len() == 36 && is_packet_ref(w[1]))
        .map(|w| w[1].to_string())
}

/// The first `Packet: <ref>` line, then the first brief path
/// `…/builders/<ref>/brief.txt`.
fn packet_in(prompt: &str) -> Option<String> {
    let by_line = prompt.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("Packet:")?;
        let token = rest
            .split_whitespace()
            .next()?
            .trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-');
        is_packet_ref(token).then(|| token.to_string())
    });
    by_line.or_else(|| {
        prompt.split_whitespace().find_map(|word| {
            let (before, _) = word.split_once("/brief.txt")?;
            let (_, id) = before.rsplit_once("/builders/")?;
            is_packet_ref(id).then(|| id.to_string())
        })
    })
}

/// Read the hook's payload. Pure: the wire is the caller's.
pub(crate) fn parse(input: &Value) -> Parsed {
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if tool != AGENT_TOOL {
        return Parsed::NotADispatch {
            why: format!("tool_name is {tool:?}, not {AGENT_TOOL}"),
        };
    }
    if input.get("agent_id").and_then(Value::as_str).is_some() {
        return Parsed::NotADispatch {
            why: "an Agent call inside a subagent is the agent's own helper, not a dispatch"
                .to_string(),
        };
    }
    let prompt = input
        .pointer("/tool_input/prompt")
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(run) = existing_run(prompt) {
        return Parsed::ExistingRun { run };
    }
    match packet_in(prompt) {
        Some(packet_ref) => Parsed::Packet { packet_ref },
        None => Parsed::Untracked,
    }
}

/// The hook's answer for a dispatch: the tool's input with the prompt
/// replaced by the brief-plus-run-section, in the shape Claude Code
/// reads off a PreToolUse hook's stdout.
pub(crate) fn updated_input(
    input: &Value,
    prompt: &str,
    subagent_type: Option<&str>,
    isolation: Option<Isolation>,
) -> Value {
    let mut tool_input = input.get("tool_input").cloned().unwrap_or(json!({}));
    tool_input["prompt"] = json!(prompt);
    // THE EFFORT REACHES A CONTROL HERE (backlog e720dd00, 2026-09-19).
    // The block declares an effort; the harness takes reasoning effort
    // from an agent definition named as `subagent_type`. So the door
    // that already rewrites the prompt names the definition too, and
    // the declaration selects the CPU instead of describing it. `None`
    // — a checkout without the definitions — leaves the caller's own
    // type rather than naming one that cannot be loaded.
    if let Some(name) = subagent_type {
        tool_input["subagent_type"] = json!(name);
    }
    // THE LANE SETS THE ISOLATION (backlog 65cea113, 2026-09-23), by
    // the same reasoning one level over: a car-lane run gets its own
    // worktree whether or not the operator typed it, and a step-lane
    // run loses one it was handed — the worktree guard it brings
    // refuses heredocs and compound reads that touch no git.
    // `Isolation` says why at length; `None` leaves the caller's.
    match (isolation, tool_input.as_object_mut()) {
        (Some(Isolation::Worktree), Some(obj)) => {
            obj.insert("isolation".into(), json!("worktree"));
        }
        (Some(Isolation::Shared), Some(obj)) => {
            obj.remove("isolation");
        }
        _ => {}
    }
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": tool_input,
        }
    })
}

/// The hook's answer for a definition this session cannot load
/// (backlog e1c4dc93): a PreToolUse DENY, which is the only shape that
/// stops the Agent call while the door still owns the words. It names
/// the definitions and the restart, because the harness's own refusal
/// — `Agent type 'effort-high' not found. Available agents: …`,
/// measured live 2026-09-22 — arrives only AFTER the claim and the run
/// are filed, leaving a run packet for an agent that never started.
pub(crate) fn refusal(missing: &[String]) -> Value {
    let names = missing.join(", ");
    let dir = boss_jobs::agent_spec::DEFINITIONS_DIR;
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "boss dispatch: this session started before {dir} held {names}, so it cannot \
                 load that definition and the Agent call would fail with `Agent type not \
                 found` (backlog e1c4dc93). Nothing was claimed and no run was filed. Restart \
                 the session — a new session reads {dir} again — then dispatch this packet."
            ),
        }
    })
}

/// The session's next untracked count, off its current metadata.
pub(crate) fn next_untracked(session: &Value) -> u64 {
    session
        .pointer("/metadata/untracked_runs")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1
}

/// What the door did, for the hook.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Nothing,
    Untracked {
        session: Option<String>,
        count: Option<u64>,
    },
    Linked {
        run: String,
    },
    /// A definition this session never loaded: refused before the
    /// claim, so no run is filed for an agent that cannot start.
    Refused {
        missing: Vec<String>,
    },
    Dispatched(Dispatched),
}

/// The verb against an explicit base — the seam the wire tests go
/// through. `session` is the work-session packet, when the hook has one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn from_hook_at(
    http: &reqwest::Client,
    base: &str,
    repo: &Path,
    input: &Value,
    session: Option<&str>,
    actor: &str,
    owner: &str,
    worktree: &str,
    host: &str,
    definitions: Option<&Path>,
) -> Result<Outcome> {
    use reqwest::Method;
    let api_at = |method: Method, path: String, body: Option<Value>| {
        let signature = crate::identity::Signature::As(actor.to_string());
        async move { crate::gate::api_at_signed(http, base, method, &path, body, signature).await }
    };
    match parse(input) {
        Parsed::NotADispatch { why } => {
            eprintln!("boss dispatch --from-hook: nothing to record — {why}");
            Ok(Outcome::Nothing)
        }
        Parsed::Untracked => {
            let Some(session) = session else {
                eprintln!(
                    "boss dispatch --from-hook: the prompt names no packet and the hook has no \
                     session — an untracked run, uncounted"
                );
                return Ok(Outcome::Untracked {
                    session: None,
                    count: None,
                });
            };
            let row = api_at(Method::GET, format!("/api/jobs/{session}"), None)
                .await?
                .with_context(|| format!("the session {session} read returned no body"))?;
            let count = next_untracked(&row);
            api_at(
                Method::PATCH,
                format!("/api/jobs/{session}/metadata"),
                Some(json!({ "untracked_runs": count })),
            )
            .await
            .with_context(|| format!("counting an untracked run on session {session}"))?;
            eprintln!(
                "boss dispatch --from-hook: the prompt names no packet — untracked run {count} on \
                 session {}",
                &session[..8.min(session.len())]
            );
            Ok(Outcome::Untracked {
                session: Some(session.to_string()),
                count: Some(count),
            })
        }
        Parsed::ExistingRun { run } => {
            if let Some(session) = session {
                api_at(
                    Method::PATCH,
                    format!("/api/jobs/{run}/metadata"),
                    Some(json!({ "session": session })),
                )
                .await
                .with_context(|| format!("linking run {run} to session {session}"))?;
            }
            eprintln!(
                "boss dispatch --from-hook: the prompt carries run {} already — linked, not refiled",
                &run[..8]
            );
            eprintln!("{RUN_LINE_PREFIX}{run}");
            Ok(Outcome::Linked { run })
        }
        Parsed::Packet { packet_ref } => {
            // BEFORE the claim (backlog e1c4dc93): a definition this
            // session never loaded fails the Agent call whatever we
            // do, and dispatching first would leave the step claimed
            // and a run filed behind a builder that never started.
            let missing = crate::dispatch::unloaded_for_session(repo, definitions);
            if !missing.is_empty() {
                let answer = refusal(&missing);
                eprintln!(
                    "boss dispatch --from-hook: this session never loaded {} — refused, nothing \
                     claimed and no run filed; restart the session",
                    missing.join(", ")
                );
                print!("{answer}");
                return Ok(Outcome::Refused { missing });
            }
            let prompt = input
                .pointer("/tool_input/prompt")
                .and_then(Value::as_str)
                .unwrap_or("");
            let dispatched = dispatch_at(
                http,
                base,
                repo,
                &packet_ref,
                None,
                None,
                &Overrides::default(),
                false,
                actor,
                owner,
                worktree,
                host,
                BriefSource::Handed { prompt, session },
            )
            .await?;
            eprintln!("{RUN_LINE_PREFIX}{}", dispatched.run_id);
            print!(
                "{}",
                updated_input(
                    input,
                    &dispatched.prompt,
                    dispatched.subagent_type.as_deref(),
                    dispatched.isolation,
                )
            );
            Ok(Outcome::Dispatched(dispatched))
        }
    }
}

/// `boss dispatch <session|-> --from-hook`: stdin is the hook's
/// payload. The hook script owns the exit code the operator sees — it
/// exits 0 whatever this verb answers — so a refusal here is a stderr
/// line in the journal, never a blocked Agent call.
pub async fn run(session: String) -> Result<()> {
    let input: Value = {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("reading the hook payload from stdin")?;
        serde_json::from_str(&text).context("the hook payload is not JSON")?
    };
    let session = (session != "-" && !session.trim().is_empty()).then_some(session);
    let base = crate::gate::resolve_jobs_base(None)?;
    let repo = crate::brief::repo_root()?;
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    let owner = crate::owner::for_filing_at(&base).await;
    let host = crate::prove::host();
    let worktree = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_default();
    // `session-start.sh` wrote this at the one moment Claude Code
    // reads the definitions directory (backlog e1c4dc93); unset, the
    // door knows nothing about the session and refuses nothing.
    let definitions = std::env::var(crate::dispatch::SESSION_DEFINITIONS_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(std::path::PathBuf::from);
    from_hook_at(
        &reqwest::Client::new(),
        &base,
        &repo,
        &input,
        session.as_deref(),
        &actor,
        &owner,
        &worktree,
        &host,
        definitions.as_deref(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "5b1d2c3e-0000-4000-8000-000000000001";
    const PACKET: &str = "da925366-cce4-4bff-ac0b-c1e88ac6023c";

    fn call(prompt: &str) -> Value {
        json!({
            "session_id": "s-1", "cwd": "/work/boss", "hook_event_name": "PreToolUse",
            "tool_name": "Agent", "tool_use_id": "toolu_01",
            "tool_input": { "prompt": prompt, "description": "build", "subagent_type": "claude" },
        })
    }

    /// The three prompt shapes the operator writes, and the one `boss
    /// dispatch` writes.
    #[test]
    fn the_prompt_names_a_packet_a_run_or_nothing() {
        // Shape 1: a `Packet:` line, full uuid or short.
        let p = format!("You are a builder.\nPacket: {PACKET}\nRead RULES.md first.");
        assert_eq!(
            parse(&call(&p)),
            Parsed::Packet {
                packet_ref: PACKET.into()
            }
        );
        let p = "Build this.\n  Packet: da925366 (backlog)\n";
        assert_eq!(
            parse(&call(p)),
            Parsed::Packet {
                packet_ref: "da925366".into()
            }
        );
        // Shape 2: the brief path, as the briefing prompt spells it.
        let p = "Read …/scratchpad/builders/RULES.md FULLY first, then your brief at \
                 /tmp/claude-0/-work-boss/e9c8a14d/scratchpad/builders/da925366/brief.txt \
                 (the packet's detail is the spec).";
        assert_eq!(
            parse(&call(p)),
            Parsed::Packet {
                packet_ref: "da925366".into()
            }
        );
        // The RULES.md path is not a packet, and a short token that is
        // not hex is not one either.
        let p = "Read /x/builders/RULES.md and /x/builders/notes/brief.txt";
        assert_eq!(parse(&call(p)), Parsed::Untracked);
        // Shape 3: nothing named — a helper agent, an explore.
        assert_eq!(
            parse(&call("Find where the yard reads gate-runs.")),
            Parsed::Untracked
        );
        // Shape 4: `boss dispatch`'s own run section, pasted by hand.
        let p = format!(
            "== THE PACKET ==\nPacket: {PACKET}\n\n== THE RUN ==\n\nYour run is agent-run {RUN} \
             (profile `builder`, model opus-5[1m], budget $5, effort high).\n"
        );
        assert_eq!(parse(&call(&p)), Parsed::ExistingRun { run: RUN.into() });
    }

    #[test]
    fn only_a_top_level_agent_call_is_a_dispatch() {
        let mut bash = call("Packet: da925366");
        bash["tool_name"] = json!("Bash");
        assert!(matches!(parse(&bash), Parsed::NotADispatch { .. }));
        let mut nested = call("Packet: da925366");
        nested["agent_id"] = json!("a4d2c8f1");
        assert!(matches!(parse(&nested), Parsed::NotADispatch { why } if why.contains("subagent")));
        let mut no_input = call("");
        no_input["tool_input"] = json!({});
        assert_eq!(parse(&no_input), Parsed::Untracked);
    }

    /// The hook's answer keeps every other input key and replaces the
    /// prompt; the count reads absent as zero.
    /// The refusal is the hook's own stdout answer: a PreToolUse deny
    /// whose reason names every definition the session is missing and
    /// the one act that repairs it (backlog e1c4dc93).
    #[test]
    fn the_refusal_names_the_definition_and_the_restart() {
        let out = refusal(&["effort-high".to_string(), "effort-low".to_string()]);
        let hs = &out["hookSpecificOutput"];
        assert_eq!(hs["hookEventName"], "PreToolUse");
        assert_eq!(hs["permissionDecision"], "deny");
        let why = hs["permissionDecisionReason"].as_str().expect("a reason");
        assert!(why.contains("effort-high"), "{why}");
        assert!(why.contains("effort-low"), "{why}");
        assert!(why.contains("Restart"), "{why}");
        assert!(why.contains("e1c4dc93"), "{why}");
        assert!(
            why.contains("nothing was claimed") || why.contains("Nothing was claimed"),
            "{why}"
        );
    }

    #[test]
    fn the_updated_input_keeps_the_calls_other_keys() {
        let out = updated_input(
            &call("Packet: da925366"),
            "Packet: da925366\n== THE RUN ==",
            None,
            None,
        );
        let ti = &out["hookSpecificOutput"]["updatedInput"];
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(ti["prompt"], "Packet: da925366\n== THE RUN ==");
        assert_eq!(ti["description"], "build");
        // No definition in this tree: the caller's own type stands.
        assert_eq!(ti["subagent_type"], "claude");

        // The declared effort SELECTS the CPU: the door that rewrites
        // the prompt names the definition on the call (e720dd00).
        let named = updated_input(&call("Packet: da925366"), "p", Some("effort-high"), None);
        let ti = &named["hookSpecificOutput"]["updatedInput"];
        assert_eq!(ti["subagent_type"], "effort-high");
        assert_eq!(ti["description"], "build");
        assert_eq!(ti["prompt"], "p");

        // The lane decides the isolation (65cea113). A step-lane run
        // drops the operator's `isolation`, so no worktree guard
        // stands between an analyst and a heredoc; a car-lane run gets
        // one whether or not the operator typed it; unknown leaves the
        // call as it came.
        let mut isolated = call("Packet: da925366");
        isolated["tool_input"]["isolation"] = json!("worktree");
        let shared = updated_input(&isolated, "p", None, Some(Isolation::Shared));
        let ti = &shared["hookSpecificOutput"]["updatedInput"];
        assert!(ti.get("isolation").is_none(), "{ti}");
        assert_eq!(ti["description"], "build");
        let built = updated_input(&call("p"), "p", None, Some(Isolation::Worktree));
        assert_eq!(
            built["hookSpecificOutput"]["updatedInput"]["isolation"],
            "worktree"
        );
        let kept = updated_input(&isolated, "p", None, None);
        assert_eq!(
            kept["hookSpecificOutput"]["updatedInput"]["isolation"],
            "worktree"
        );

        assert_eq!(next_untracked(&json!({ "metadata": {} })), 1);
        assert_eq!(
            next_untracked(&json!({ "metadata": { "untracked_runs": 4 } })),
            5
        );
    }
}

// ---------------------------------------------------------------------
// The wire: the same in-memory jobs API shape `dispatch.rs` tests
// against, for the three outcomes that write.
// ---------------------------------------------------------------------
#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const PACKET: &str = "39d0b528-ff69-4cb8-ba82-408b641da66c";
    const RUN: &str = "5b1d2c3e-0000-4000-8000-000000000001";
    const SESSION: &str = "7a1e2b3c-0000-4000-8000-00000000abcd";

    type Calls = Arc<Mutex<Vec<(String, String, Value)>>>;

    async fn stub() -> (String, Calls) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let run: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 65536];
                let mut want = None;
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if want.is_none()
                        && let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        want = Some(end + 4 + len);
                    }
                    if want.is_some_and(|w| buf.len() >= w) {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let mut words = text.split_whitespace();
                let method = words.next().unwrap_or("GET").to_string();
                let target = words.next().unwrap_or("/").to_string();
                let body: Value = text
                    .split("\r\n\r\n")
                    .nth(1)
                    .and_then(|b| serde_json::from_str(b).ok())
                    .unwrap_or(Value::Null);
                log.lock()
                    .unwrap()
                    .push((method.clone(), target.clone(), body.clone()));
                let path = target.split('?').next().unwrap_or("").to_string();
                let (status, resp): (&str, String) = match (method.as_str(), path.as_str()) {
                    ("GET", p) if p == format!("/api/jobs/{PACKET}") => (
                        "200 OK",
                        json!({
                            "id": PACKET, "kind": "backlog-item", "title": "Shop floor",
                            "status": "open", "priority": "standard", "opened_on": "2026-09-18",
                            "metadata": { "detail": "Decided as proposed." },
                            "steps": [
                                { "id": "s-build", "spec_slug": "build", "status": "ready",
                                  "title": "Build the change",
                                  "metadata": { "authority_role": "platform-admin",
                                    "agent_profile": "builder", "agent_model": "opus-5[1m]",
                                    "agent_budget_usd": 5, "agent_effort": "high" } },
                            ],
                        })
                        .to_string(),
                    ),
                    ("GET", p) if p == format!("/api/jobs/{SESSION}") => (
                        "200 OK",
                        json!({ "id": SESSION, "kind": "work-session", "status": "open",
                                "metadata": { "actor": "emp-david", "untracked_runs": 2 } })
                        .to_string(),
                    ),
                    ("POST", p) if p.ends_with("/claim") => {
                        ("200 OK", json!({ "status": "active" }).to_string())
                    }
                    ("POST", "/api/jobs") => {
                        let mut filed = body.clone();
                        filed["id"] = json!(RUN);
                        filed["steps"] = json!([
                            { "id": "run-claimed", "spec_slug": "claimed", "status": "completed", "metadata": {} },
                            { "id": "run-briefed", "spec_slug": "briefed", "status": "ready", "metadata": {} },
                            { "id": "run-building", "spec_slug": "building", "status": "pending", "metadata": {} },
                        ]);
                        *run.lock().unwrap() = Some(filed);
                        ("201 Created", json!({ "id": RUN }).to_string())
                    }
                    ("GET", p) if p == format!("/api/jobs/{RUN}") => {
                        match run.lock().unwrap().clone() {
                            Some(r) => ("200 OK", r.to_string()),
                            None => ("404 Not Found", "no such job".into()),
                        }
                    }
                    ("PUT", _) | ("PATCH", _) => ("204 No Content", String::new()),
                    _ => ("404 Not Found", format!("unstubbed {method} {target}")),
                };
                let out = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{resp}",
                    resp.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), calls)
    }

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is above this crate")
    }

    fn call(prompt: &str) -> Value {
        json!({
            "session_id": "s-1", "cwd": "/work/boss/.claude/worktrees/agent-x",
            "hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_use_id": "toolu_01",
            "tool_input": { "prompt": prompt, "description": "build" },
        })
    }

    async fn go(base: &str, input: &Value, session: Option<&str>) -> Outcome {
        go_with(base, input, session, None).await
    }

    async fn go_with(
        base: &str,
        input: &Value,
        session: Option<&str>,
        definitions: Option<&std::path::Path>,
    ) -> Outcome {
        from_hook_at(
            &reqwest::Client::new(),
            base,
            &repo(),
            input,
            session,
            "emp-david",
            "emp-david",
            "/work/boss/.claude/worktrees/agent-x",
            "boss-dev-0",
            definitions,
        )
        .await
        .expect("the door answers")
    }

    /// A prompt naming a packet is dispatched as the hand verb does it —
    /// claim, file, brief — with the prompt as the brief, the session on
    /// the run, and the run section handed back for the tool's input.
    #[tokio::test]
    async fn a_packet_in_the_prompt_is_dispatched_with_the_prompt_as_its_brief() {
        let (base, calls) = stub().await;
        let prompt = format!("You are a builder.\nPacket: {PACKET}\nStop and report per rule 8.");
        let out = go(&base, &call(&prompt), Some(SESSION)).await;
        let Outcome::Dispatched(d) = out else {
            panic!("{out:?}");
        };
        assert_eq!(d.run_id, RUN);
        assert!(d.prompt.starts_with(&prompt), "the brief IS the prompt");
        assert!(
            d.prompt.contains(&format!("export BOSS_AGENT_RUN={RUN}")),
            "and the run section follows it"
        );
        // End to end (e720dd00): the block declares effort high, so the
        // definition this checkout holds for `high` is what the call is
        // rewritten to name — the declaration selecting the CPU.
        assert_eq!(
            d.subagent_type.as_deref(),
            Some("effort-high"),
            "the declared effort selects the definition"
        );
        assert!(
            d.prompt.contains("subagent_type effort-high"),
            "{}",
            d.prompt
        );
        // The step declares `builder`, whose document's lane is `car`:
        // the run is worktree-isolated (65cea113).
        assert_eq!(d.isolation, Some(crate::dispatch::Isolation::Worktree));
        let calls = calls.lock().unwrap().clone();
        let filed = calls
            .iter()
            .find(|(m, p, _)| m == "POST" && p == "/api/jobs")
            .map(|(_, _, b)| b.clone())
            .expect("a run was filed");
        assert_eq!(filed["metadata"]["brief"], prompt);
        assert_eq!(filed["metadata"]["session"], SESSION);
        assert_eq!(filed["metadata"]["packet"], PACKET);
        assert_eq!(filed["metadata"]["model"], "opus-5[1m]");
        assert!(
            calls
                .iter()
                .any(|(m, p, _)| m == "POST"
                    && *p == format!("/api/jobs/{PACKET}/steps/s-build/claim")),
            "the claim came first"
        );
        assert!(
            calls.iter().any(|(m, p, b)| m == "PUT"
                && *p == format!("/api/jobs/{RUN}/steps/run-briefed")
                && b["status"] == "completed"),
            "briefed is completed"
        );
    }

    /// A session that never loaded the definition is refused HERE,
    /// before anything is written (backlog e1c4dc93). Claude Code
    /// reads `.claude/agents/*.md` once, at session start; a car that
    /// adds one lands under a session already running, and the hook
    /// would then name a `subagent_type` that session cannot load.
    /// Measured live 2026-09-22: the harness answers `Agent type
    /// 'effort-ultra-nonexistent' not found. Available agents: …` —
    /// loud, immediate, never a silent fallback — so what the raw
    /// failure costs is not the diagnosis but the RESIDUE, a step
    /// claimed and a run filed for an agent that never started. The
    /// door refuses first and says what repairs it.
    #[tokio::test]
    async fn a_session_that_never_loaded_the_definition_is_refused_before_the_claim() {
        let dir = boss_testing::scratch_dir("dispatch-hook-definitions");
        let snapshot = dir.join("definitions");
        // This session loaded the general agents and none of the
        // effort definitions — it started before the car that added
        // them, exactly the packet's case.
        std::fs::write(&snapshot, "claude\ngeneral-purpose\n").expect("write the snapshot");

        let (base, calls) = stub().await;
        let out = go_with(
            &base,
            &call(&format!("Packet: {PACKET}")),
            Some(SESSION),
            Some(snapshot.as_path()),
        )
        .await;
        let Outcome::Refused { missing, .. } = &out else {
            panic!("{out:?}");
        };
        assert!(missing.iter().any(|m| m == "effort-high"), "{missing:?}");
        assert!(
            calls.lock().unwrap().is_empty(),
            "nothing is claimed and no run is filed: {:?}",
            calls.lock().unwrap()
        );

        // The same session once it has been restarted: the snapshot
        // holds the definitions, and the dispatch proceeds.
        std::fs::write(
            &snapshot,
            "claude\neffort-low\neffort-medium\neffort-high\n",
        )
        .expect("rewrite the snapshot");
        let (base, _calls) = stub().await;
        let out = go_with(
            &base,
            &call(&format!("Packet: {PACKET}")),
            Some(SESSION),
            Some(snapshot.as_path()),
        )
        .await;
        assert!(matches!(out, Outcome::Dispatched(_)), "{out:?}");

        // No snapshot at all — a hand-run verb, or a session older
        // than the hook that writes one — is never refused.
        let (base, _calls) = stub().await;
        let out = go_with(
            &base,
            &call(&format!("Packet: {PACKET}")),
            Some(SESSION),
            None,
        )
        .await;
        assert!(matches!(out, Outcome::Dispatched(_)), "{out:?}");
    }

    /// A pasted `boss dispatch` prompt links the run it names to the
    /// session and files nothing.
    #[tokio::test]
    async fn a_prompt_carrying_a_run_is_linked_not_refiled() {
        let (base, calls) = stub().await;
        let prompt = format!("== THE RUN ==\nYour run is agent-run {RUN} (profile `builder`).");
        let out = go(&base, &call(&prompt), Some(SESSION)).await;
        assert_eq!(out, Outcome::Linked { run: RUN.into() });
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].0, "PATCH");
        assert_eq!(calls[0].1, format!("/api/jobs/{RUN}/metadata"));
        assert_eq!(calls[0].2, json!({ "session": SESSION }));
    }

    /// No packet: counted on the session, nothing filed — and with no
    /// session, nothing written at all. Never a refusal.
    #[tokio::test]
    async fn an_untracked_run_is_counted_on_the_session_and_never_refused() {
        let (base, calls) = stub().await;
        let out = go(
            &base,
            &call("Find where the yard reads gate-runs."),
            Some(SESSION),
        )
        .await;
        assert_eq!(
            out,
            Outcome::Untracked {
                session: Some(SESSION.into()),
                count: Some(3)
            }
        );
        let calls = calls.lock().unwrap().clone();
        let patch = calls
            .iter()
            .find(|(m, _, _)| m == "PATCH")
            .expect("the count was written");
        assert_eq!(patch.1, format!("/api/jobs/{SESSION}/metadata"));
        assert_eq!(patch.2, json!({ "untracked_runs": 3 }));
        assert!(
            !calls
                .iter()
                .any(|(m, p, _)| m == "POST" && p == "/api/jobs")
        );

        let (base, calls) = stub().await;
        let out = go(&base, &call("Find where the yard reads gate-runs."), None).await;
        assert_eq!(
            out,
            Outcome::Untracked {
                session: None,
                count: None
            }
        );
        assert!(calls.lock().unwrap().is_empty(), "no session, no write");
        let out = go(
            &base,
            &json!({ "tool_name": "Bash", "tool_input": {} }),
            Some(SESSION),
        )
        .await;
        assert_eq!(out, Outcome::Nothing);
        assert!(calls.lock().unwrap().is_empty());
    }
}
