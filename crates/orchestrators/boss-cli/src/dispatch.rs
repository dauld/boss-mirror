//! `boss dispatch <packet> [--step <slug>] [--model M --budget B
//! --effort E]` — hand a protocol step to an agent, as a packet.
//!
//! `boss dispatch --next --station <name>` is the same verb taking its
//! packet from a QUEUE instead of from the caller (design 8382bbb2,
//! backlog 923b6571) — the durable inbox. See the section above
//! [`next_at`] for why that is the whole difference that matters.
//!
//! WHY THIS IS A VERB (design c87fb59b, decided 2026-09-18; car 2,
//! backlog 39d0b528). Until this landed, dispatching a builder was
//! five hand acts in the operator's terminal: read the packet, write a
//! brief, paste a rules file from /tmp, pick a model and a budget from
//! memory, and paste the lot into the Agent tool — and the run itself
//! existed nowhere in the record. Car 1 put the model, the budget and
//! the effort on the step's own Workflow row as its `agent` block; this
//! verb is the door that READS it, so a run takes the protocol's
//! settings and the record says so.
//!
//! WHAT IT DOES, in order — and each write is confirmed by reading it
//! back, the rule every filing verb here follows:
//!
//! 1. Reads the packet and picks the step: `--step <slug>`, or the one
//!    step the packet is AT when there is exactly one (two open steps
//!    is a refusal that lists them).
//! 2. Resolves the step's agent block. The packet first — car 1
//!    projects the block onto the materialised step as `agent_profile`
//!    / `agent_model` / `agent_budget_usd` / `agent_effort`, and "the
//!    claim door reads the packet, never the spec" is that car's own
//!    rule — then the ACTIVE Workflow row's step, for a packet
//!    materialised before the row declared one (this packet's own
//!    `build` was: opened 17:33, the block landed 18:11). Neither is a
//!    REFUSAL that names the fix: add `agent = { profile = …, model =
//!    …, budget_usd = …, effort = … }` to the step in its Workflow row.
//!    `--model`, `--budget`, `--effort` OVERRIDE only what they name;
//!    a model the rate card cannot price and an effort outside
//!    low|medium|high are refused the way the publish lint refuses
//!    them, so a run is never priced against nothing.
//!    THE HOSTING DOOR sits between 1 and 2 (a479faf7; design 01c3cc3f
//!    reader 3): a packet that declares the paths its change will
//!    touch (`metadata.paths`) is judged against the instance's edit
//!    level (`GET /api/tenant/edit-level`, the tenant manifest's
//!    `[meta] edit_level`) with the one predicate the gate's lint also
//!    reads, `boss_core::tiers::TierMap::first_above` — refused before
//!    an agent spends, naming the first path, its tier and the level.
//!    A packet declaring no paths is admitted here; the gate decides
//!    on the diff. An instance declaring no level enforces nothing.
//! 3. Claims the step as the actor running the verb (`BOSS_ACTOR`;
//!    unnamed, the claim is refused before anything is filed) through
//!    the claim door — a Ready→Active compare-and-set that answers 409
//!    with the holder, so two operators dispatching one step is a
//!    refusal naming the other, not two runs.
//! 4. Files the `agent-run` packet with the settings, the actor, the
//!    worktree and the host on it, and the rendered brief as `brief`
//!    — the record of what the agent was told.
//! 5. Prints the EXACT prompt: `boss brief`'s rendering (the packet,
//!    the invariants, the rules document for the profile) and the run
//!    id — what the operator pastes into the Agent tool today and what
//!    a runner reads later — and completes the run's `briefed` step
//!    with the prompt's size, so the run is at `building` the moment
//!    the prompt exists.
//!
//! THE EFFORT SELECTS THE CPU (backlog e720dd00, 2026-09-19). The
//! block's `effort` was resolved, validated, written onto the run
//! packet and printed in the prompt — and applied by nothing: the
//! harness takes reasoning effort from an agent DEFINITION, and there
//! were none, so every builder recorded `effort = high` and ran at the
//! session default. A packet asserting a property of a run that is not
//! true of that run is the mostly-sure shape the correctness protocol
//! refuses, and the honest repair is to make the declaration select
//! something. It now names a definition — `.claude/agents/effort-<v>.md`,
//! one per value [`boss_jobs::agent_spec::Effort`] admits, identical
//! but for the `effort:` line so a series across them varies reasoning
//! budget and nothing else — in the prompt ([`effort_line`]) and on the
//! Agent call itself, where the hook sets `subagent_type`
//! ([`crate::dispatch_hook::updated_input`]). What that buys is
//! "declared and NAMED", not "provably applied": that the harness
//! honours the key is attested by its documentation (sub-agents,
//! Advanced Fields, v2.1.278), and a model cannot introspect its own
//! thinking budget, so no probe available to us measures the effort
//! took. The tests pin what is checkable — a definition for every
//! declarable effort, its effort matching its name, its model fixed,
//! and the prompt naming the right one.
//!
//! The prompt goes to stdout and every status line to stderr, so
//! `boss dispatch <packet> > prompt.txt` is the prompt and nothing else.
//!
//! `boss dispatch --report <run> --summary S [--spend-usd D] [--tokens
//! N | IN,OUT]` is the OTHER end of the run (car 3, backlog cb78818d —
//! car 2's second loose end): the builder's handback, recorded by the
//! operator who read it today and by the runner later. It writes the
//! report onto the run packet (`report`, `spend_usd`, `tokens` — the
//! keys the agent-run schema names for a later hand), completes the
//! run's `reported` step when the green has opened it (the run lands
//! on that), and records the finish in `agent_runs` — the row the claim
//! door reads an actor's hour-window spend from, priced by the rate
//! card at its two rates when the tokens are a split and at the model's
//! declared blend when they are a total, with the basis named either
//! way (`PricingBasis`; design 91a9bfe7). Idempotent end to end: the packet PATCH
//! merges, a completed `reported` is left as it is, and the run row is
//! keyed on the run's id. That last key is INSERT-ONCE, so a second
//! report changes no row: an identical retry says the record already
//! there stands, and one carrying a different count is refused naming
//! both figures, rather than printing the held row's price as though
//! it were this command's answer ([`record_line`], backlog b4fd594e).

use anyhow::{Context, Result, bail};
use boss_jobs::agent_runs::{PricingBasis, TokenUsage};
use serde_json::{Value, json};
use std::path::Path;

/// The kind this verb files.
pub(crate) const RUN_KIND: &str = "agent-run";

/// The step `boss dispatch` completes once the prompt is printed.
pub(crate) const BRIEFED_SLUG: &str = "briefed";

/// What a run is launched with, resolved from the block and the
/// overrides.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settings {
    pub profile: String,
    pub model: String,
    pub budget_usd: f64,
    pub effort: String,
}

/// The overrides the flags name. Each is applied over the block's
/// value and nothing else; an absent flag changes nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Overrides {
    pub model: Option<String>,
    pub budget_usd: Option<f64>,
    pub effort: Option<String>,
}

/// The block as car 1 projects it onto a materialised step: four
/// plain keys, all present or the step declares none.
pub(crate) fn block_on_step(step: &Value) -> Option<Settings> {
    use boss_jobs::agent_spec::{BUDGET_KEY, EFFORT_KEY, MODEL_KEY, PROFILE_KEY};
    let md = step.get("metadata")?;
    let text = |k: &str| md.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Settings {
        profile: text(PROFILE_KEY)?,
        model: text(MODEL_KEY)?,
        budget_usd: md.get(BUDGET_KEY).and_then(Value::as_f64)?,
        effort: text(EFFORT_KEY)?,
    })
}

/// The block on a Workflow row's step, as `GET /api/workflows/{kind}`
/// serves the spec (`steps[].agent`).
pub(crate) fn block_in_row(row: &Value, slug: &str) -> Option<Settings> {
    let step = row
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(slug))?;
    let a: boss_jobs::agent_spec::AgentSpec =
        serde_json::from_value(step.get("agent")?.clone()).ok()?;
    Some(Settings {
        profile: a.profile,
        model: a.model,
        budget_usd: a.budget_usd,
        effort: a.effort.as_str().to_string(),
    })
}

/// THE reader of a step's agent settings, and the only one (backlog
/// dacee8cc): the projection the packet carries, else the block the
/// active Workflow row declares for that step. `row` is what the
/// caller has in hand — `None` asks whether the step can answer alone,
/// which is how a caller that reads the row lazily stays lazy.
///
/// It exists because the fallback arrived twice by repair rather than
/// once by design, and the two copies did not agree. `boss dispatch`
/// required all four projected keys (half a projection is none, since
/// they are written together); `boss brief` read `agent_profile` alone,
/// so a half-projected step briefed a human in one lane and dispatched
/// an agent in the other. CLAUDE.md 9a: a fact that lives twice gets
/// collapsed if it can be, and this one could.
///
/// The fallback is needed because the projection is copied at OPEN
/// time, like the procedure, so every packet admitted before its kind
/// declared an agent block has none. Measured 2026-09-22 against the
/// live system of record: 195 open steps whose active row declares a
/// block carry no `agent_` key — 186 page-audit, 8 backlog-item, 1
/// user-feedback. Back-filling them is NOT the answer: an in-flight
/// packet is pinned to the version it was admitted under, and writing
/// v3's block onto a v1 packet would make the record state a
/// declaration that version never made.
pub(crate) fn settings_for(step: &Value, row: Option<&Value>) -> Option<Settings> {
    block_on_step(step).or_else(|| {
        let slug = step.get("spec_slug").and_then(Value::as_str)?;
        block_in_row(row?, slug)
    })
}

/// The refusal a step with no block gets: it names the fix, in the
/// row's own syntax.
pub(crate) fn no_block_refusal(kind: &str, slug: &str) -> String {
    format!(
        "step `{slug}` of {kind} declares no agent block, so nothing says which model, at \
         what effort, under what spend — add `agent = {{ profile = \"builder\", model = \
         \"opus-5[1m]\", budget_usd = 5, effort = \"high\" }}` to that step in its Workflow \
         row (infra/platform/workflows/{kind}.toml, then `boss workflow publish {kind}`)"
    )
}

/// The settings a run launches with: the block, with each override
/// laid over the one value it names, and the result checked the way
/// the publish lint checks a block — a model the rate card cannot
/// price, a budget that is not positive, an effort outside the set.
pub(crate) fn resolve(block: Settings, over: &Overrides) -> std::result::Result<Settings, String> {
    let settings = Settings {
        profile: block.profile,
        model: over.model.clone().unwrap_or(block.model),
        budget_usd: over.budget_usd.unwrap_or(block.budget_usd),
        effort: over.effort.clone().unwrap_or(block.effort),
    };
    let effort: boss_jobs::agent_spec::Effort = serde_json::from_value(json!(settings.effort))
        .map_err(|_| {
            format!(
                "effort `{}` is not one of low, medium, high",
                settings.effort
            )
        })?;
    let spec = boss_jobs::agent_spec::AgentSpec {
        profile: settings.profile.clone(),
        model: settings.model.clone(),
        budget_usd: settings.budget_usd,
        effort,
    };
    match boss_jobs::agent_spec::refusal(&spec) {
        Some(why) => Err(why),
        None => Ok(settings),
    }
}

/// The step to dispatch: the named one, which must be open; or the
/// one open step when nothing is named. Two open steps is a refusal
/// that lists them, never a guess.
pub(crate) fn choose_step<'a>(
    job: &'a Value,
    slug: Option<&str>,
) -> std::result::Result<&'a Value, String> {
    let steps = crate::envelope::steps(job);
    let short = crate::envelope::job_id(job)
        .map(|id| &id[..8.min(id.len())])
        .unwrap_or("?");
    let open: Vec<&Value> = steps
        .iter()
        .copied()
        .filter(|s| crate::envelope::step_line(s).now)
        .collect();
    match slug {
        Some(slug) => {
            let step = steps
                .iter()
                .copied()
                .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
                .ok_or_else(|| format!("packet {short} has no step `{slug}`"))?;
            let line = crate::envelope::step_line(step);
            if !line.now {
                return Err(format!(
                    "packet {short}'s `{slug}` is {} — only a ready or active step can be \
                     dispatched",
                    line.status
                ));
            }
            Ok(step)
        }
        None => match open.as_slice() {
            [one] => Ok(one),
            [] => Err(format!(
                "packet {short} has no ready or active step — nothing to dispatch"
            )),
            many => Err(format!(
                "packet {short} is at {} steps — say which with --step: {}",
                many.len(),
                many.iter()
                    .map(|s| crate::envelope::step_line(s).slug)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    }
}

/// The paths a packet declares its change will touch — `metadata.paths`,
/// an array of tree-relative path strings. The hosting door's input
/// (a479faf7): a packet that declares none is admitted here and the
/// gate decides on the diff.
pub(crate) fn declared_paths(job: &Value) -> Vec<String> {
    job.get("metadata")
        .and_then(|m| m.get(PATHS_KEY))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The packet metadata key the door reads.
pub(crate) const PATHS_KEY: &str = "paths";

/// The jobs API path the instance answers its edit level on.
pub(crate) const EDIT_LEVEL_PATH: &str = "/api/tenant/edit-level";

/// The instance's declared edit level: `Some(tier)` when it declares
/// one, `None` when it declares none (`null`) — or when the instance
/// has no level door at all (404, a server from before a479faf7):
/// no level is no door, never a level that admits nothing. Anything
/// else is an error, because a door that cannot read the level must
/// not open a run.
pub(crate) async fn edit_level_at(http: &reqwest::Client, base: &str) -> Result<Option<String>> {
    let url = format!("{base}{EDIT_LEVEL_PATH}");
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("reading the instance's edit level at {url}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        eprintln!(
            "boss dispatch: {url} answered 404 — this instance has no edit-level door, so no \
             level is enforced"
        );
        return Ok(None);
    }
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "reading the instance's edit level: {url} -> {status}: {}",
            body.trim()
        );
    }
    let v: Value = serde_json::from_str(&body).with_context(|| {
        format!("the edit-level door at {url} answered something other than JSON")
    })?;
    Ok(v.get("edit_level")
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// THE HOSTING DOOR, the dispatch half (a479faf7; design 01c3cc3f
/// reader 3; the gate half is infra/lint/a-car-stays-under-the-edit-level.sh
/// and both read the one predicate, `boss_core::tiers`). The refusal
/// for a declared scope above the level, or `None` when it is
/// admitted. A level the map does not name is a refusal too: the
/// manifest is wrong, and a run must not open on a guess.
pub(crate) fn level_refusal(
    short: &str,
    slug: &str,
    level: &str,
    paths: &[String],
) -> Option<String> {
    let map = match boss_core::tiers::tier_map() {
        Ok(m) => m,
        Err(e) => {
            return Some(format!(
                "the tier map this binary carries does not parse: {e}"
            ));
        }
    };
    let above = match map.first_above(level, paths.iter().map(String::as_str)) {
        Ok(a) => a?,
        Err(e) => {
            return Some(format!(
                "this instance's manifest declares an edit level the tier map does not know — {e}"
            ));
        }
    };
    Some(format!(
        "packet {short}'s `{slug}` declares a change this instance's edit level does not admit: \
         {} — nothing claimed, nothing filed. Raise the level in the tenant's manifest \
         ([meta] edit_level) or narrow the packet's metadata.{PATHS_KEY}",
        map.level_refusal(level, &above)
    ))
}

/// The run packet's body, through the one envelope every filing verb
/// uses. The brief rides as `brief` — the record of what the agent was
/// told, copied from the prompt and never retyped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_body(
    packet_id: &str,
    packet_title: &str,
    step_slug: &str,
    agent: &str,
    settings: &Settings,
    worktree: &str,
    host: &str,
    brief: &str,
    owner: &str,
) -> Value {
    crate::job::envelope(
        RUN_KIND,
        &format!("{} run: {packet_title}", settings.profile),
        None,
        None,
        owner,
        Some(json!({
            "packet": packet_id,
            "step": step_slug,
            "agent": agent,
            "model": settings.model,
            "budget_usd": settings.budget_usd,
            "effort": settings.effort,
            "worktree": worktree,
            "host": host,
            "brief": brief,
        })),
    )
}

/// The `briefed` completion: `prompt_bytes` over the step's own
/// metadata (PATCH-on-PUT replaces `metadata` wholesale).
pub(crate) fn briefed_body(existing: &Value, prompt_bytes: usize) -> Value {
    let mut md = match existing.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    md.insert("prompt_bytes".into(), json!(prompt_bytes.to_string()));
    json!({ "status": "completed", "metadata": Value::Object(md) })
}

/// The agent definition a run's declared effort selects — the one
/// mapping, in `boss_jobs::agent_spec`, never a name spelled here.
pub(crate) fn subagent_type(settings: &Settings) -> String {
    boss_jobs::agent_spec::definition_name(&settings.effort)
}

/// The definition in THIS checkout, or `None` when the file is not
/// there. Named after the file it reads: a definition the harness
/// cannot load is a `subagent_type` that fails the call, so the hook
/// names one only when the tree holds it.
pub(crate) fn definition_in(repo: &Path, settings: &Settings) -> Option<String> {
    let name = subagent_type(settings);
    let path = repo
        .join(boss_jobs::agent_spec::DEFINITIONS_DIR)
        .join(format!("{name}.md"));
    path.is_file().then_some(name)
}

/// Where `session-start.sh` left the list of definitions THIS session
/// loaded — the one moment Claude Code reads the directory.
pub(crate) const SESSION_DEFINITIONS_ENV: &str = "BOSS_SESSION_AGENT_DEFINITIONS";

/// The declarable definitions this checkout holds that the session did
/// not load (backlog e1c4dc93). Claude Code reads
/// [`boss_jobs::agent_spec::DEFINITIONS_DIR`] ONCE, at session start;
/// one that arrives after — the car that added them, an hourly
/// fast-forward of the pod's checkout — is on disk and unknown to the
/// running session, so naming it is an immediate `Agent type
/// 'effort-high' not found`. Pure: both lists are the caller's.
pub(crate) fn unloaded(on_disk: &[String], loaded: &[String]) -> Vec<String> {
    boss_jobs::agent_spec::Effort::ALL
        .iter()
        .map(|e| boss_jobs::agent_spec::definition_name(e.as_str()))
        .filter(|name| on_disk.iter().any(|d| d == name) && !loaded.iter().any(|l| l == name))
        .collect()
}

/// [`unloaded`] against this checkout and the session's snapshot. No
/// snapshot — a session that started before the hook wrote one, or a
/// `boss dispatch` run by hand — means nothing is KNOWN about what the
/// session loaded, and an unknown is never a refusal.
pub(crate) fn unloaded_for_session(repo: &Path, snapshot: Option<&Path>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(snapshot) else {
        return Vec::new();
    };
    let loaded: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    let dir = repo.join(boss_jobs::agent_spec::DEFINITIONS_DIR);
    let on_disk: Vec<String> = boss_jobs::agent_spec::Effort::ALL
        .iter()
        .map(|e| boss_jobs::agent_spec::definition_name(e.as_str()))
        .filter(|name| dir.join(format!("{name}.md")).is_file())
        .collect();
    unloaded(&on_disk, &loaded)
}

/// The last part of the prompt: the run's id, and the one export that
/// ties the gate the builder launches back to it.
pub(crate) fn run_section(run_id: &str, settings: &Settings) -> String {
    format!(
        "== THE RUN ==\n\n\
         Your run is agent-run {run_id} (profile `{}`, model {}, budget ${}, effort {}).\n\
         Before `boss gate`, in the shell you gate from: export {}={run_id}\n\
         The gate-run then records this run, and a green lands it by itself.\n\
         {}\n{}\n",
        settings.profile,
        settings.model,
        settings.budget_usd,
        settings.effort,
        crate::gate::AGENT_RUN_ENV,
        budget_line(settings.budget_usd),
        effort_line(settings),
    )
}

/// The effort as a CONTROL rather than a noun (backlog e720dd00,
/// 2026-09-19). Until this landed the block's `effort` was validated,
/// written onto the run packet and printed in the sentence above —
/// and nothing applied it, so every builder recorded `effort=high`
/// and ran at the session default. The harness takes reasoning effort
/// from an agent DEFINITION, so the declaration selects one: the hook
/// puts this name on the Agent call's `subagent_type`, and a prompt
/// pasted by hand names it here for the operator who pastes it.
pub(crate) fn effort_line(settings: &Settings) -> String {
    let name = subagent_type(settings);
    format!(
        "Launched with subagent_type {name} ({}/{name}.md) — the definition whose `effort` line \
         is the effort this block declares. The PreToolUse hook sets it on the Agent call; a \
         prompt pasted by hand must name it, or the run takes the session's own effort and the \
         record says something untrue of it.",
        boss_jobs::agent_spec::DEFINITIONS_DIR
    )
}

/// The spend cap as the runner takes it (car 3, backlog cb78818d): the
/// block's `budget_usd` rendered as the `--max-budget-usd` flag a
/// runner passes to its session, and as a sentence for the operator
/// who pastes the prompt by hand today. The claim door admitted this
/// run against the agent's hourly budget with exactly this number, so
/// the runner's cap and the door's reservation are one value.
pub(crate) fn budget_line(budget_usd: f64) -> String {
    format!(
        "Spend cap: --max-budget-usd {budget_usd} — the claim door reserved this much of the \
         agent's hourly budget for the run; stop and report before you pass it."
    )
}

/// Where the brief comes from (design 511fa7d4 car 2b, backlog
/// da925366). `Rendered` is the hand-run verb: `boss brief`'s
/// rendering, printed to stdout. `Handed` is the hook's door
/// (`--from-hook`, `dispatch_hook.rs`): the prompt the Agent tool was
/// already given IS the brief — recorded verbatim, never printed, since
/// the operator wrote it and the agent already has it — and `session`
/// is the work-session packet the run belongs to, written as
/// `metadata.session` so the crew board can fold runs under their
/// crew.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BriefSource<'a> {
    Rendered,
    Handed {
        prompt: &'a str,
        session: Option<&'a str>,
    },
}

/// The merging PATCH that makes the executing run a FACT on the step
/// it was claimed for (backlog dd6d44b7), under the same key
/// `boss gate` stamps on a gate-run — one spelling in this crate,
/// because it is the `link` the delivery rule follows
/// (`agent-run-delivers-when-its-step-is-done`, pinned below).
pub(crate) fn executing_run_patch(run_id: &str) -> Value {
    json!({ crate::gate::AGENT_RUN_KEY: run_id })
}

/// What a dispatch produced: the run's id and the exact prompt — the
/// brief plus the run section — whether it was printed (`Rendered`) or
/// handed back to the hook to put on the tool's input (`Handed`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dispatched {
    pub run_id: String,
    pub prompt: String,
    /// The agent definition the declared effort selects, when this
    /// checkout HAS it (backlog e720dd00). `None` in a tree without
    /// the definitions — the hook then leaves the call's own type
    /// rather than naming a CPU that does not exist here.
    pub subagent_type: Option<String>,
}

/// The cars that name this packet as the item they build — narrowed at
/// the server by the containment door (`metadata=`, url-encoded through
/// `job::QUERY_VALUE`, the one encoder every other caller uses).
///
/// Exact id, because that is what a car carries: all 189 `backlog_item`
/// values on the instance were full 36-char ids when this was measured
/// (2026-09-19). A car holding an 8-char PREFIX would not be found, and
/// that miss is today's behaviour — no refusal — never a wrong one.
pub(crate) fn cars_for_item_query(item_id: &str) -> String {
    let doc = json!({ boss_jobs::car::BACKLOG_ITEM: item_id }).to_string();
    format!(
        "/api/jobs?kind=ship-a-change&metadata={}&limit=50",
        percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
    )
}

/// THE LANDED-WORK REFUSAL (a7837d81). A packet whose fix is already on
/// main is refused BEFORE the claim, and the refusal names the car that
/// carried it.
///
/// WHAT WAS MEASURED, and why this is not a heuristic. On 2026-09-19
/// two packets were dispatched to builders at high effort after their
/// fixes had landed: d7fef617 (train #466, ~21 h earlier) and f47861a5
/// (train #471, ~19 h earlier). The first run was spent entirely
/// re-deriving a landed fix. The cause was NOT a missing link — of the
/// 188 merged cars on the instance, 175 named a closing `backlog_item`,
/// 6 named a `partial_item` and 7 named a `no_item_reason`, so every
/// one of them answered the question `require_item_answer` asks, and
/// not one of the 175 left its item open once the CAR closed. Both of
/// these cars named their packet correctly.
///
/// What they had in common is that they were still `open` at `Proven in
/// prod`: `ship-a-change` reaches `merged` off `steps.proven.done`, so
/// an unproven car never closes, `jobs.complete_linked_step` never
/// fires, and the item stays open for as long as the proof takes.
/// Twelve such cars were standing that morning, the oldest landed 58 h
/// earlier, holding ten open items between them — a population, not two
/// accidents, and the queue an automated runner drains.
///
/// So the question here is answered from the RECORD, never from the
/// tree: does a car naming this packet already satisfy
/// `boss_jobs::car::is_landed`? That is the same predicate `boss gate`
/// and the auto-park handler ask before filing a twin, and it is the
/// only "is this already fixed" question that can be answered without
/// reading code and guessing at it.
///
/// THE PROMPT IS THE DELIVERABLE, SO A DISCARDED ONE IS A REFUSAL
/// (backlog d268b260). `boss dispatch <packet> > prompt.txt` is what
/// you paste; the prompt prints ONCE, to stdout, after the step is
/// claimed and the run is filed. Send stdout to /dev/null and the claim
/// and the run survive while the only copy of the thing they exist to
/// produce does not.
///
/// Measured three times, every one by the session that keeps filing
/// about it: five runs orphaned on 2026-09-19 (`c8703bee`, gate-runs
/// carrying `agent_run: None`, all five recovered by hand), and twice
/// more on 2026-09-20 — the second while testing which packets were
/// already landed, an hour after writing the packet describing the
/// habit. Recovery is possible (the run id is on stderr) and costs a
/// re-render plus a hand substitution into the `BOSS_AGENT_RUN=<run id>`
/// placeholder, per run.
///
/// WHY THIS AND NOT "IS STDOUT A TTY". Piping the prompt into another
/// program is legitimate and a shell with no terminal is legitimate;
/// the only thing refused is output that is PROVABLY discarded. On
/// Linux `/proc/self/fd/1` resolves to `/dev/null` exactly then — a
/// file gets its path, a pipe `pipe:[…]`, a terminal `/dev/pts/N`.
/// Where /proc is absent (macOS, the workstation) the read fails, this
/// answers `None`, and nothing changes: a check that cannot see must
/// not refuse.
pub(crate) fn discarded_stdout_refusal() -> Option<String> {
    let target = std::fs::read_link("/proc/self/fd/1").ok()?;
    stdout_goes_nowhere(&target.to_string_lossy()).then(|| {
        "stdout is /dev/null, so this dispatch would CLAIM the step, FILE the run, and throw the \
         prompt away — the prompt is the deliverable, printed once and only here. Refused before \
         the claim, so nothing is left half-done.\n  \
         Send it somewhere: `boss dispatch <packet> > prompt.txt`, or drop the redirect to read \
         it. Five runs were orphaned this way on 2026-09-19 and each needed the brief re-rendered \
         and its run id substituted by hand (c8703bee)."
            .to_string()
    })
}

/// Is this stdout provably discarded? Split out from the read so the
/// judgement is testable without a process whose fd 1 is /dev/null.
pub(crate) fn stdout_goes_nowhere(target: &str) -> bool {
    target == "/dev/null"
}

/// The claim is re-read off each row rather than trusted from the
/// query: a server that ignored `metadata=` would answer the
/// unfiltered page, and an unnarrowed page must narrow nothing
/// (CLAUDE.md §Doors — a wrong target answers instead of erroring).
pub(crate) fn landed_work_refusal(item_id: &str, cars: &[Value]) -> Option<String> {
    let landed = cars.iter().find(|c| {
        c.pointer("/metadata/backlog_item").and_then(Value::as_str) == Some(item_id)
            && boss_jobs::car::is_landed(c)
    })?;
    let branch = landed
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let full = landed.get("id").and_then(Value::as_str).unwrap_or("?");
    let car = &full[..full.len().min(8)];
    let at = landed
        .pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .unwrap_or("main");
    let item = &item_id[..item_id.len().min(8)];
    Some(format!(
        "this packet's fix is ALREADY ON MAIN: car {car} ({branch}) merged at {at}.\n  \
         Dispatching it spends an agent run re-deriving landed work — measured twice on \
         2026-09-19 (a7837d81), 21 h and 19 h after the merge.\n  \
         That car is still open because its proof has not completed, which is the only \
         reason its item did not close by itself. Two repairs, in order:\n    \
         boss prove {branch}\n      \
         runs the car's recorded probe; a pass closes the car AND this packet\n    \
         boss dispatch {item} --force\n      \
         if the packet asks for MORE than that car carried, say so and build the rest"
    ))
}

/// The whole verb against an explicit base — the seam the wire tests
/// go through. Returns the run and the prompt.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_at(
    http: &reqwest::Client,
    base: &str,
    repo: &Path,
    packet_ref: &str,
    step_slug: Option<&str>,
    // The station the claim PULLS FROM, when this dispatch came out
    // of a queue (`--next`). Named on the claim, so the door checks
    // membership and capability against the queue the work was taken
    // from — a dispatch that names no station keeps today's claim.
    station: Option<&str>,
    over: &Overrides,
    // Dispatch this packet even though a car carrying its fix has
    // already landed — the operator saying "the packet asks for more
    // than that car carried". `--next` never sets it: the queue is
    // exactly where nobody is reading each choice.
    force: bool,
    actor: &str,
    owner: &str,
    worktree: &str,
    host: &str,
    source: BriefSource<'_>,
) -> Result<Dispatched> {
    use reqwest::Method;

    // Every call signed as the actor the verb resolved: the run is
    // theirs, and so is the claim.
    let api_at = |method: Method, path: String, body: Option<Value>| {
        let signature = crate::identity::Signature::As(actor.to_string());
        async move { crate::gate::api_at_signed(http, base, method, &path, body, signature).await }
    };

    let id = crate::job::fetch_and_resolve(http, packet_ref).await?;
    let job = api_at(Method::GET, format!("/api/jobs/{id}"), None)
        .await?
        .context("the packet read returned no body")?;
    let kind = job
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let title = crate::envelope::job_title(&job).unwrap_or("?").to_string();
    let step = choose_step(&job, step_slug).map_err(|e| anyhow::anyhow!("{e}"))?;
    let slug = step
        .get("spec_slug")
        .and_then(Value::as_str)
        .context("the step has no spec_slug")?
        .to_string();
    let step_id = step
        .get("id")
        .and_then(Value::as_str)
        .context("the step has no id")?
        .to_string();

    // THE HOSTING DOOR (a479faf7): a packet that declares the paths its
    // change will touch is judged against the instance's edit level
    // BEFORE anything is read further or claimed — dispatch refuses
    // before an agent spends. A packet declaring none is admitted; the
    // gate judges the diff with the same predicate.
    let paths = declared_paths(&job);
    if !paths.is_empty()
        && let Some(level) = edit_level_at(http, base).await?
        && let Some(why) = level_refusal(&id[..8], &slug, &level, &paths)
    {
        bail!("{why}");
    }

    // THE DISCARDED-PROMPT DOOR (d268b260): before anything is claimed
    // or filed, refuse a dispatch whose prompt has nowhere to go. It is
    // first among the pre-claim doors because it costs no read at all —
    // and because the failure it prevents is the quietest of the three:
    // the hosting and landed-work doors stop work that should not
    // happen, this one stops work that WOULD happen and then vanish.
    if let Some(why) = discarded_stdout_refusal() {
        bail!("{why}");
    }

    // THE LANDED-WORK DOOR (a7837d81): a packet a merged car already
    // names is refused BEFORE the claim, for the same reason the
    // hosting door above is — an agent that has claimed a step has
    // already started spending. Best-effort on the READ only: a jobs
    // API that cannot answer this one query must not stop a dispatch
    // it would otherwise admit, so an unreachable read is silence and
    // the dispatch proceeds. What is never best-effort is the verdict:
    // a page that DID come back and names a landed car refuses.
    if !force
        && let Ok(body) = api_at(Method::GET, cars_for_item_query(&id), None).await
        && let Some(why) = landed_work_refusal(&id, &crate::gate::rows(body))
    {
        bail!("{why}");
    }

    // The block: the packet's projection, else the active row's step,
    // both through the ONE reader `boss brief` also asks (dacee8cc).
    // Asked first with no row in hand, so the row is read only when
    // the step cannot answer alone. The row is KEPT when it is read,
    // because the brief dates the step's procedure against the same
    // row (794e8d61) and one dispatch should read it at most once.
    let (block, row_already_read) = match settings_for(step, None) {
        Some(b) => (b, None),
        None => {
            let row = api_at(Method::GET, format!("/api/workflows/{kind}"), None)
                .await
                .with_context(|| format!("reading the {kind} Workflow row for its agent block"))?;
            let block = settings_for(step, row.as_ref())
                .ok_or_else(|| anyhow::anyhow!("{}", no_block_refusal(&kind, &slug)))?;
            (block, row)
        }
    };
    let settings = resolve(block, over).map_err(|e| anyhow::anyhow!("{e}"))?;

    // CLAIM FIRST. A step someone else holds is a refusal naming the
    // holder, and it must come before anything is filed. The ONE door:
    // it runs the station's capability gate when a station is named
    // and the budget reservation always (`boss_jobs::agent_budget` —
    // the block's budget against the actor's hour, before the CAS), so
    // a queued dispatch and a hand dispatch are gated by one rule.
    let claim_path = match station {
        Some(s) => format!("/api/jobs/{id}/steps/{step_id}/claim?station={s}"),
        None => format!("/api/jobs/{id}/steps/{step_id}/claim"),
    };
    api_at(Method::POST, claim_path, None)
        .await
        .with_context(|| format!("claiming `{slug}` on {} as {actor}", &id[..8]))?;

    // The brief, rendered ONCE: it is the prompt and the record. The
    // hook hands the prompt the agent was already given, which is the
    // same fact from the other side.
    //
    // RENDERED AFTER THE CLAIM, FROM A RE-READ (backlog c8faa7f3). It
    // used to be rendered before, so its `now at` line said `ready`
    // while the agent reading it held a step the claim door had
    // already made `active` — a status one write out of date on every
    // dispatch there has ever been. The claim is the door's effect, so
    // the brief reads it back rather than assuming it.
    let brief = match source {
        BriefSource::Rendered => {
            let claimed = api_at(Method::GET, format!("/api/jobs/{id}"), None)
                .await?
                .with_context(|| {
                    format!(
                        "claimed `{slug}` on {} and then could not read the packet back to \
                         brief from — the step is held by {actor} and needs releasing",
                        &id[..8]
                    )
                })?;
            // The active Workflow row, so the brief can date the
            // procedure it is about to hand the agent (794e8d61) —
            // the copy the block already read when there is one, else
            // a best-effort read of its own. An unreachable read says
            // so in the brief; it never stops a dispatch the claim has
            // already made.
            let active = match row_already_read {
                Some(row) => Some(row),
                None => api_at(Method::GET, format!("/api/workflows/{kind}"), None)
                    .await
                    .ok()
                    .flatten(),
            };
            crate::brief::render(repo, Some(&claimed), &settings.profile, active.as_ref())?
        }
        BriefSource::Handed { prompt, .. } => prompt.to_string(),
    };

    let mut body = run_body(
        &id, &title, &slug, actor, &settings, worktree, host, &brief, owner,
    );
    if let BriefSource::Handed {
        session: Some(session),
        ..
    } = source
    {
        body["metadata"]["session"] = json!(session);
    }
    let created = api_at(Method::POST, "/api/jobs".to_string(), Some(body))
        .await?
        .context("the run's create returned no body")?;
    let run_id = created
        .get("data")
        .unwrap_or(&created)
        .get("id")
        .and_then(Value::as_str)
        .context("the run's create returned no id — refusing to call that dispatched")?
        .to_string();

    // THE EDGE (backlog dd6d44b7). The claimed step now SAYS which run
    // is executing it. Nothing on a packet said so before: the run
    // names its packet and step (`metadata.packet` / `metadata.step`)
    // and the packet named nothing back, so the crew board's BUILDING
    // lane had to INFER the run from a branch with no gate-run behind
    // it, and no rule could reach from a step completing to the run
    // that did the work. It rides the step rather than the job because
    // one packet hosts a run PER STEP — the page march dispatches
    // `measure` and `file` on the same page-audit — so one job-level
    // key could not name which.
    //
    // Written before the prompt is printed, and fatal if it fails: a
    // run whose step carries no edge is one that can never be
    // delivered, and handing over the prompt anyway would leave the
    // operator holding a run the silence clock will age out as died.
    api_at(
        Method::PATCH,
        format!("/api/jobs/{id}/steps/{step_id}/metadata"),
        Some(executing_run_patch(&run_id)),
    )
    .await
    .with_context(|| {
        format!(
            "writing the run edge onto `{slug}` on {} — run {} is filed and the step is claimed",
            &id[..8],
            &run_id[..8.min(run_id.len())]
        )
    })?;

    // A 201 is a claim; the read-back is the fact — and the step ids
    // only exist once the packet does.
    let run = api_at(Method::GET, format!("/api/jobs/{run_id}"), None)
        .await?
        .context("filed a run the API will not read back")?;
    let briefed = crate::envelope::steps(&run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(BRIEFED_SLUG))
        .cloned()
        .with_context(|| format!("run {run_id} has no `{BRIEFED_SLUG}` step"))?;
    let briefed_id = briefed
        .get("id")
        .and_then(Value::as_str)
        .context("the briefed step has no id")?
        .to_string();

    let prompt = format!("{brief}\n{}", run_section(&run_id, &settings));
    if matches!(source, BriefSource::Rendered) {
        print!("{prompt}");
    }
    api_at(
        Method::PUT,
        format!("/api/jobs/{run_id}/steps/{briefed_id}"),
        Some(briefed_body(&briefed, prompt.len())),
    )
    .await
    .context("completing the run's briefed step")?;
    eprintln!(
        "boss dispatch: run {} open for `{slug}` on {} — {} bytes of prompt printed, \
         building as {actor} on {host}",
        &run_id[..8.min(run_id.len())],
        &id[..8],
        prompt.len()
    );
    Ok(Dispatched {
        run_id,
        prompt,
        subagent_type: definition_in(repo, &settings),
    })
}

// ---------------------------------------------------------------------------
// The durable inbox — `boss dispatch --next --station <name>`
// ---------------------------------------------------------------------------
//
// WHY (design 8382bbb2, backlog 923b6571, decided 2026-09-19). Until
// this, a dispatch lived in the session that made it: `boss dispatch
// <packet>` printed a prompt and the session held the thread, so when
// the session died at ~15:39Z on 2026-09-19 fifteen `agent-run`
// packets were orphaned — twelve green but never handed back, three
// with a prompt printed and no executor ever started — and recovery
// was by hand.
//
// The fix is not a coordinator. The QUEUE already exists as data: a
// step declaring an `agent` block under a role projects an
// `a.<role>.<model-slug>` station (`station_projection::agent_stations`),
// and the packet sits in it while the step is ready. What was missing
// was the door OUT of it — this verb, which asks the record what is
// waiting and takes the first piece of it. Nothing is handed from the
// process that filed the work to the one that runs it; the record is
// the whole hand-off, which is what makes the work outlive either.
//
// The station is NAMED, never inferred. An agents row's `role` is a
// Class code (`engineering-agent` on this instance) and a station's
// role is the step's `authority_role` (`platform-admin`); nothing
// reconciles the two, so guessing an actor's inbox from its row would
// resolve a queue that does not exist and answer "nothing waiting"
// instead of erroring — the wrong-target shape CLAUDE.md warns about.

/// The step an inbox hands out: ready, nobody's, and carrying the
/// agent model — which is exactly what made the packet a member of an
/// `a.<role>.<model>` station, so the verb selects on the same fact
/// the queue did rather than a second opinion about it.
///
/// ACTIVE steps are deliberately not candidates although the station
/// holds them (a queue shows what is being worked as well as what is
/// waiting): they are somebody's, and the claim would 409.
pub(crate) fn waiting_step(job: &Value) -> Option<&Value> {
    crate::envelope::steps(job).into_iter().find(|s| {
        s.get("status").and_then(Value::as_str) == Some("ready")
            && s.get("assignee_id").and_then(Value::as_str).is_none()
            && s.get("metadata")
                .and_then(|m| m.get(boss_jobs::agent_spec::MODEL_KEY))
                .is_some()
    })
}

/// Take the first piece of waiting work at `station` and dispatch it.
/// `Ok(None)` is an empty inbox — a runner asking again in a minute is
/// the normal case, not an error.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn next_at(
    http: &reqwest::Client,
    base: &str,
    repo: &Path,
    station: &str,
    over: &Overrides,
    actor: &str,
    owner: &str,
    worktree: &str,
    host: &str,
) -> Result<Option<Dispatched>> {
    use reqwest::Method;

    let api_at = |method: Method, path: String| {
        let signature = crate::identity::Signature::As(actor.to_string());
        async move { crate::gate::api_at_signed(http, base, method, &path, None, signature).await }
    };

    // The queue read is the ONLY input: no packet is passed in, and a
    // station that does not exist is a refusal, not an empty answer.
    let queue = api_at(Method::GET, format!("/api/stations/{station}/queue"))
        .await
        .with_context(|| format!("reading the {station} queue as {actor}"))?
        .with_context(|| format!("the {station} queue returned no body"))?;
    let members: Vec<String> = queue
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|j| crate::envelope::job_id(j).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    for id in &members {
        let job = api_at(Method::GET, format!("/api/jobs/{id}"))
            .await?
            .with_context(|| format!("packet {} read returned no body", &id[..8.min(id.len())]))?;
        let Some(step) = waiting_step(&job) else {
            continue;
        };
        let slug = step
            .get("spec_slug")
            .and_then(Value::as_str)
            .context("a waiting step with no spec_slug")?
            .to_string();
        eprintln!(
            "boss dispatch --next: {station} holds {} packet(s); taking {} `{slug}`",
            members.len(),
            &id[..8.min(id.len())]
        );
        return dispatch_at(
            http,
            base,
            repo,
            id,
            Some(&slug),
            Some(station),
            over,
            // Never forced from the queue: the landed-work refusal is
            // most valuable exactly where nobody reads each choice.
            false,
            actor,
            owner,
            worktree,
            host,
            BriefSource::Rendered,
        )
        .await
        .map(Some);
    }

    eprintln!(
        "boss dispatch --next: {station} holds {} packet(s), none of them waiting for an \
         executor — nothing claimed, nothing filed",
        members.len()
    );
    Ok(None)
}

/// `boss dispatch --next --station <name>` at the resolved base.
pub async fn next(
    station: String,
    model: Option<String>,
    budget: Option<f64>,
    effort: Option<String>,
) -> Result<()> {
    let base = crate::gate::resolve_jobs_base(None)?;
    let repo = crate::brief::repo_root()?;
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    let owner = crate::owner::for_filing_at(&base).await;
    let host = crate::prove::host();
    let worktree = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let over = Overrides {
        model,
        budget_usd: budget,
        effort,
    };
    if let Some(b) = over.budget_usd
        && !(b.is_finite() && b > 0.0)
    {
        bail!("--budget must be a positive number of dollars, got {b}");
    }
    next_at(
        &reqwest::Client::new(),
        &base,
        &repo,
        &station,
        &over,
        &actor,
        &owner,
        &worktree,
        &host,
    )
    .await?;
    Ok(())
}

/// The step `--report` completes.
pub(crate) const REPORTED_SLUG: &str = "reported";

/// What a run spent, in the two shapes a reporter can be in — the
/// same two `boss_jobs::agent_runs::TokenUsage` records, parsed from
/// `--tokens N` (a total) or `--tokens IN,OUT` (a split, priceable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tokens {
    Total(u64),
    Split { input: u64, output: u64 },
}

impl Tokens {
    pub(crate) fn total(self) -> u64 {
        match self {
            Tokens::Total(t) => t,
            Tokens::Split { input, output } => input.saturating_add(output),
        }
    }
}

/// `--tokens` as typed: digits, or two digit runs joined by a comma.
pub(crate) fn parse_tokens(s: &str) -> std::result::Result<Tokens, String> {
    let bad = || {
        format!(
            "--tokens {s:?} is not a count: give a total (--tokens 761000) or the split the \
             usage line shows (--tokens 740000,21000 as input,output) — a split is priced at \
             the card's two rates, a total at the model's declared blend"
        )
    };
    let n = |t: &str| t.trim().replace('_', "").parse::<u64>().map_err(|_| bad());
    match s.split_once(',') {
        Some((i, o)) => Ok(Tokens::Split {
            input: n(i)?,
            output: n(o)?,
        }),
        None => Ok(Tokens::Total(n(s)?)),
    }
}

/// The builder's handback as `--report` takes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Report {
    pub summary: String,
    pub spend_usd: Option<f64>,
    pub tokens: Option<Tokens>,
}

/// The merging PATCH the report writes onto the run packet: the keys
/// the agent-run schema names for the report (`spend_usd`, `tokens`,
/// numbers) and the summary as `report` — so the handback is on the
/// record the moment it arrives, whether or not `reported` has opened.
pub(crate) fn report_patch(r: &Report) -> Value {
    let mut md = serde_json::Map::new();
    md.insert("report".into(), json!(r.summary));
    if let Some(d) = r.spend_usd {
        md.insert("spend_usd".into(), json!(d));
    }
    if let Some(t) = r.tokens {
        md.insert("tokens".into(), json!(t.total()));
    }
    Value::Object(md)
}

/// The `reported` completion: the step's three declared fields
/// (`summary` required; `spend_usd` and `tokens` are string fields on
/// the row) over the step's own metadata, since PATCH-on-PUT replaces
/// `metadata` wholesale.
pub(crate) fn reported_body(existing: &Value, r: &Report) -> Value {
    let mut md = match existing.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    md.insert("summary".into(), json!(r.summary));
    if let Some(d) = r.spend_usd {
        md.insert("spend_usd".into(), json!(d.to_string()));
    }
    if let Some(t) = r.tokens {
        md.insert("tokens".into(), json!(t.total().to_string()));
    }
    json!({ "status": "completed", "metadata": Value::Object(md) })
}

/// The registered id the run's `agent` signs as, off `GET /api/agents`:
/// the id itself when it already is one, else the row whose aliases
/// hold the login. `None` is "no row names it" — the record cannot
/// name its CPU, and the refusal says how to register one.
pub(crate) fn resolve_agent(agents: &[Value], login: &str) -> Option<String> {
    let id_of = |a: &Value| a.get("id").and_then(Value::as_str).map(str::to_string);
    agents
        .iter()
        .find(|a| {
            id_of(a).as_deref() == Some(login)
                || a.get("aliases")
                    .and_then(Value::as_array)
                    .is_some_and(|al| al.iter().any(|x| x.as_str() == Some(login)))
        })
        .and_then(id_of)
}

/// The step whose `result` says which terminal the run reached.
pub(crate) const BUILDING_SLUG: &str = "building";

/// The run's outcome, READ from the terminal it reached rather than
/// asserted (backlog 8f1de7bf, 2026-09-19: `run_record` hardcoded
/// `"success"`, so a run that refused was recorded identically to one
/// that landed green first try — all 24 rows in the live table said
/// success). The `result` field on `building` is required and
/// enum-checked by the Workflow row, so the four values below are the
/// whole fork: `gated` is the gate's green, `delivered` is a run that
/// ships no car finishing its work (backlog a9c6ed5b — an analyst has
/// no gate to go green, and a success recorded as anything else is a
/// packet asserting something untrue of the run), `refused` is an
/// agent that stopped without doing it, `died` is the silence rule's
/// verdict — and silence is refused exactly like failure. A value the
/// protocol admits and this match does not reads as "no terminal
/// reached", which would drop the run's spend from `agent_runs`
/// entirely; `every_value_the_protocol_admits_is_an_outcome_this_records`
/// holds the two equal (CLAUDE.md 9a).
///
/// `None` is "the run has reached no terminal yet", which is not an
/// outcome: the report can arrive before the green, and the row is
/// insert-once, so a guess made here would be the permanent record.
pub(crate) fn run_outcome(run: &Value) -> Option<&'static str> {
    let result = crate::envelope::steps(run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(BUILDING_SLUG))
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
        .and_then(|s| s.pointer("/metadata/result").and_then(Value::as_str))?;
    match result {
        "gated" | "delivered" => Some("success"),
        "refused" => Some("cancelled"),
        "died" => Some("failed"),
        _ => None,
    }
}

/// What `--report` tells a run that has reached no terminal — and the
/// command that ends it (backlog 2e4d7624, 2026-09-22).
///
/// The refusal was already correct about the CONDITION and said
/// nothing about what to do: "once the run has landed, refused or
/// died" is a state with no verb attached, and the verb was not
/// discoverable — the builder who hit it on run 93bcd09c found it by
/// reading `infra/platform/workflows/agent-run.toml`. The cost of not
/// naming it is silent and shared: an un-terminated run keeps its slot
/// against the agent's concurrent-run cap, and the only symptom is the
/// NEXT dispatch refusing with 409, paid by whoever comes next rather
/// than by the run that refused (open runs reached 7 against a cap of
/// 6 on the day this was filed).
///
/// Refusing to build is a GOOD outcome — builder rule 15 — and it is
/// the harder ending to record: a green gate writes `gated` by itself
/// through the landing rule, while a refusal has no gate and no rule
/// and must be written by a hand. So this hands over the door, the way
/// the step API's 409 names `PATCH /api/jobs/{id}/metadata` as the way
/// to annotate instead. It stays a two-step form on purpose: the
/// terminal is written by `boss step complete`, the same verb that
/// completes every other step in its row's declared shape, and a
/// `--report --refused` spelling would be a second way to write it
/// and a second thing to keep true.
pub(crate) fn no_terminal_line(short: &str) -> String {
    format!(
        "boss dispatch: run {short} has no terminal yet — `{BUILDING_SLUG}` carries no \
         `result`, so agent_runs would have to assert an outcome the packet does not hold. \
         The report is on the packet. End the run with the outcome it reached — `boss step \
         complete {short} --step {BUILDING_SLUG} --field result=refused` for an agent that \
         stopped without building, or `result=delivered` for work that ships no car; a green \
         gate writes `gated` by itself and the hourly clock writes `died`. Then run --report \
         again and the cost is recorded with the outcome it reached"
    )
}

/// The car the run produced, off the evidence the landing rule stamped
/// onto `building` (backlog 65c9c05a). `jobs.complete_linked_step`
/// writes the closing packet's own `metadata.branch` under the rule's
/// `evidence_key`: `gate_run` for the gate's green
/// (`agent-run-lands-on-gate-green`), `car` for the arrival one hop
/// later (`agent-run-lands-on-car-merged`). Both name the same branch;
/// the gate one is read first because it fires first, and on
/// 2026-09-19 every one of the 25 landed runs in the system of record
/// carried it while none carried `car` — the second rule is idempotent
/// and writes nothing when the first already completed the step.
///
/// `None` is honest and common: a run that refused or died reached its
/// terminal by hand or by the clock and produced no car, so there is
/// no branch to name. Until this read existed the column was filled by
/// nobody at all — 0 of 24 rows, while `job_id` was 24 of 24 — so
/// answering "what did this car cost" meant going through the run
/// packet on every read.
pub(crate) fn run_branch(run: &Value) -> Option<String> {
    let building = crate::envelope::steps(run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(BUILDING_SLUG))?;
    ["gate_run", "car"].into_iter().find_map(|key| {
        building
            .pointer(&format!("/metadata/{key}/branch"))
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .map(str::to_string)
    })
}

/// The `agent_runs` record for a run packet: keyed on the run's own id
/// (idempotent), the CPU as its registered id, the model and packet
/// off the run's metadata, started when `briefed` completed (the
/// instant the build began — the same stamp the silence rule reads)
/// or when the packet opened, finished at the report. A total-only
/// `--tokens` is recorded in full and priced at the model's declared
/// blend; a split is priced at the card's two rates, and the record
/// says which basis produced the figure. The reporter's own dollar figure rides
/// `detail` beside it, for the comparison, never as the price. The
/// `outcome` is what [`run_outcome`] read off the run's terminal, and
/// the effort it was dispatched at rides `detail` beside the spend,
/// and the `branch` is the car it produced, off the terminal's own
/// evidence ([`run_branch`]) —
/// the two together are what makes reliability-vs-cost a query rather
/// than a guess (backlog 8f1de7bf).
pub(crate) fn run_record(
    run: &Value,
    actor_id: &str,
    r: &Report,
    finished_at: chrono::DateTime<chrono::Utc>,
    outcome: &str,
) -> Result<Value> {
    let run_id = crate::envelope::job_id(run).context("the run has no id")?;
    let md = run.get("metadata").cloned().unwrap_or(Value::Null);
    let text = |k: &str| md.get(k).and_then(Value::as_str).map(str::to_string);
    let started_at = crate::envelope::steps(run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(BRIEFED_SLUG))
        .and_then(|s| s.get("completed_at").and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| text("opened_at"))
        .with_context(|| format!("run {run_id} has neither a briefed stamp nor opened_at"))?;
    let tokens = match r.tokens {
        Some(Tokens::Split { input, output }) => {
            json!({ "input_tokens": input, "output_tokens": output })
        }
        Some(Tokens::Total(t)) => json!({ "total_tokens": t }),
        // A report with no count is still a record of the run, and it
        // says so: an EXPLICIT null, which the API reads as
        // `TokenUsage::Unreported` and stores as a NULL column —
        // unknown, not zero, the same distinction `usd_micros` draws
        // for an unpriced run. Until 2026-09-19 this sent `0` with a
        // `detail.tokens_reported: false` beside it, and 13 of the 42
        // rows then live read as a measurement of zero to any query
        // that did not know to check the flag (backlog 65c9c05a).
        // Omitting the key instead would be refused, which is the
        // point: silence and a stated null are different facts.
        None => json!({ "total_tokens": null }),
    };
    let mut body = json!({
        "run_id": run_id,
        "actor_id": actor_id,
        "model": text("model"),
        "started_at": started_at,
        "finished_at": finished_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "outcome": outcome,
        "job_id": text("packet"),
        "branch": run_branch(run),
        "detail": {
            "agent_run": run_id,
            "step": text("step"),
            // The thinking budget the run was dispatched at, off the
            // packet `resolve` wrote it onto. `detail` rather than a
            // column: it is free-form on purpose, and a field can
            // graduate once the comparison says what it needs.
            "effort": text("effort"),
            "reported_spend_usd": r.spend_usd,
            // No `tokens_reported` flag: the column says it now. The
            // flag existed because a zero could not, and keeping both
            // would be the same fact in two places with nothing
            // holding them equal (CLAUDE.md §9a). Rows written before
            // 2026-09-19 still carry it, and for that window it is the
            // only way to read a 0 correctly — see backlog 65c9c05a
            // and fd5ce137, which covers the pre-instrumentation era.
        },
    });
    if let (Some(dst), Some(src)) = (body.as_object_mut(), tokens.as_object()) {
        dst.extend(src.clone());
    }
    Ok(body)
}

/// The three token columns of a recorded row, read back as the shape
/// the reporter was in. One derivation, from the row's own keys — a
/// split when both halves are there, the bare total otherwise, and
/// `None` for a row that states it holds no count.
pub(crate) fn row_tokens(row: &Value) -> Option<Tokens> {
    let n = |k: &str| row.get(k).and_then(Value::as_u64);
    match (n("input_tokens"), n("output_tokens")) {
        (Some(input), Some(output)) => Some(Tokens::Split { input, output }),
        _ => n("total_tokens").map(Tokens::Total),
    }
}

/// A count in the words the caller gave it, so two of them can be set
/// side by side and the difference read without arithmetic.
pub(crate) fn tokens_phrase(t: Option<Tokens>) -> String {
    match t {
        Some(Tokens::Split { input, output }) => format!("{input} in / {output} out"),
        Some(Tokens::Total(total)) => format!("{total} total, unsplit"),
        None => "no count at all".to_string(),
    }
}

/// What a row's figure is and what it rests on — through the record's
/// own rule ([`boss_jobs::agent_runs::pricing_basis`]), never a second
/// copy of it here, so a blended figure is never printed as a measured
/// one (design 91a9bfe7).
fn price_phrase(priced: Option<u64>, basis: Option<PricingBasis>) -> String {
    let dollars = |m: u64| format!("${:.4}", m as f64 / 1_000_000.0);
    match (priced, basis) {
        (Some(m), Some(PricingBasis::Blended)) => format!(
            "at {} — BLENDED, not measured: a bare total priced at the model's declared \
             input/output ratio (rate card)",
            dollars(m)
        ),
        (Some(m), Some(PricingBasis::Split)) => {
            format!("at {} (rate card, measured split)", dollars(m))
        }
        // Priced, but the row carries no count this build can read, so
        // the figure is stated and the claim about it is not: naming a
        // basis here would be guessing one.
        (Some(m), None) => format!(
            "at {} (rate card; basis unread — GET /api/agent-runs says which)",
            dollars(m)
        ),
        (None, _) => "unpriced — nothing on the rate card could price it: no row for the model, \
             no declared blend for a total-only count, or no count at all"
            .to_string(),
    }
}

/// What `POST /api/agent-runs` answered, said back in the caller's own
/// terms. `Ok` is a line to print; `Err` is a refusal.
///
/// The row is insert-once on `run_id` (`ON CONFLICT (run_id) DO
/// NOTHING`), and the POST says which it did: `recorded: false` is a
/// second report collapsing onto the record already there. Until
/// 2026-09-22 this verb ignored that flag and printed the HELD row's
/// price as though it described the command just run — on run 54f43757
/// a report carrying `--tokens 300000,17000` was answered "unpriced —
/// a total-only token count", which was true of the first report an
/// hour earlier and of nothing the caller had done, and the split went
/// nowhere in silence (backlog b4fd594e).
///
/// Two second reports, two different facts, so two answers:
///   - the SAME count is a retry, which is what the idempotent
///     `run_id` is for — stated, not refused, and carrying no advice,
///     because nothing the caller can send would move the row;
///   - a DIFFERENT count is a correction, and nothing edits an
///     `agent_runs` row today. It is REFUSED, naming both figures and
///     the packet that now disagrees with the row, rather than
///     restating the rule. A silent no-op is the one forbidden failure
///     mode (CLAUDE.md: no evidence is not a pass); a loud refusal is
///     not, and it leaves the operator holding a figure they can see
///     the record does not have. Making the record TAKE the correction
///     is a bigger change than a message — the insert-once contract,
///     its event replay guard and the rebuilder's own `DO NOTHING` all
///     rest on it — and it wants its own decision, not a builder's
///     aside.
pub(crate) fn record_line(
    short: &str,
    actor_id: &str,
    out: Option<&Value>,
    r: &Report,
) -> std::result::Result<String, String> {
    let row = out.and_then(|o| o.get("run"));
    let priced = row
        .and_then(|v| v.get("usd_micros"))
        .and_then(Value::as_u64);
    let held = row.and_then(row_tokens);
    let usage = match held {
        Some(Tokens::Split { input, output }) => TokenUsage::Split { input, output },
        Some(Tokens::Total(total)) => TokenUsage::TotalOnly { total },
        None => TokenUsage::Unreported,
    };
    let basis = boss_jobs::agent_runs::pricing_basis(usage, priced);
    let phrase = price_phrase(priced, basis);

    if out.and_then(|o| o.get("recorded")).and_then(Value::as_bool) != Some(false) {
        let mut line =
            format!("boss dispatch: agent_runs holds run {short} for {actor_id} {phrase}");
        match basis {
            Some(PricingBasis::Blended) => line.push_str(
                ". Give --tokens IN,OUT when a split exists and it is priced at the two rates \
                 instead",
            ),
            _ if priced.is_none() => line.push_str(". The run is recorded in full either way"),
            _ => {}
        }
        return Ok(line);
    }

    let at = row
        .and_then(|v| v.get("recorded_at"))
        .and_then(Value::as_str)
        .unwrap_or("an earlier report");
    if held == r.tokens {
        return Ok(format!(
            "boss dispatch: agent_runs already held run {short} for {actor_id} {phrase}, \
             recorded {at}. This report carries the same count, so the row is unchanged — it is \
             insert-once on run_id, and a retry collapses onto the record already there"
        ));
    }
    Err(format!(
        "agent_runs already holds run {short} for {actor_id} {phrase}, recorded {at} by an \
         earlier --report, and this report did NOT change it: the row is insert-once on run_id \
         and no verb edits one. The row holds {}; this report gave {} — the figure you just gave \
         is the one the record does not have. The handback IS on the run packet, so the packet \
         and the row now disagree about the same run. Read the row as first reported, not as \
         this command's answer; correcting a recorded cost needs a record that can take a \
         correction (backlog b4fd594e)",
        tokens_phrase(held),
        tokens_phrase(r.tokens),
    ))
}

/// The report, against an explicit base — the seam the wire tests go
/// through.
pub(crate) async fn report_at(
    http: &reqwest::Client,
    base: &str,
    run_ref: &str,
    report: &Report,
    actor: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    use reqwest::Method;
    let api_at = |method: Method, path: String, body: Option<Value>| {
        let signature = crate::identity::Signature::As(actor.to_string());
        async move { crate::gate::api_at_signed(http, base, method, &path, body, signature).await }
    };

    let run_id = crate::job::fetch_and_resolve(http, run_ref).await?;
    let run = api_at(Method::GET, format!("/api/jobs/{run_id}"), None)
        .await?
        .context("the run read returned no body")?;
    let short = &run_id[..8.min(run_id.len())];
    let kind = run.get("kind").and_then(Value::as_str).unwrap_or("?");
    if kind != RUN_KIND {
        bail!(
            "{short} is a {kind}, not an {RUN_KIND} — --report takes the run `boss dispatch` printed"
        );
    }

    // THE RECORD FIRST: the handback rides the packet whether or not
    // the green has opened `reported` yet (rule 8 of the builder
    // rules: the report arrives at gate launch, ten minutes earlier).
    api_at(
        Method::PATCH,
        format!("/api/jobs/{run_id}/metadata"),
        Some(report_patch(report)),
    )
    .await
    .with_context(|| format!("recording the report on run {short}"))?;

    let reported = crate::envelope::steps(&run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(REPORTED_SLUG))
        .cloned()
        .with_context(|| format!("run {short} has no `{REPORTED_SLUG}` step"))?;
    let line = crate::envelope::step_line(&reported);
    if line.now {
        let step_id = reported
            .get("id")
            .and_then(Value::as_str)
            .context("the reported step has no id")?;
        api_at(
            Method::PUT,
            format!("/api/jobs/{run_id}/steps/{step_id}"),
            Some(reported_body(&reported, report)),
        )
        .await
        .with_context(|| format!("completing `{REPORTED_SLUG}` on run {short}"))?;
        eprintln!(
            "boss dispatch: run {short} reported — `{REPORTED_SLUG}` completed, the run lands on it"
        );
    } else if line.status == "completed" {
        eprintln!(
            "boss dispatch: run {short} already reported — `{REPORTED_SLUG}` is completed; the record was refreshed"
        );
    } else {
        eprintln!(
            "boss dispatch: run {short}'s `{REPORTED_SLUG}` is {} — it opens on the gate's green; \
             the report rides the packet, run --report again once the run is at reported",
            line.status
        );
    }

    // THE FINISH RECORD: what the run cost and how it went, where the
    // claim door reads it. The outcome is the terminal the run
    // reached; a run that has reached none is not recorded at all,
    // because the row is insert-once and the guess would stick
    // (backlog 8f1de7bf).
    let Some(outcome) = run_outcome(&run) else {
        eprintln!("{}", no_terminal_line(short));
        return Ok(());
    };
    let login = run
        .pointer("/metadata/agent")
        .and_then(Value::as_str)
        .with_context(|| format!("run {short} names no agent"))?;
    let agents = crate::gate::rows(api_at(Method::GET, "/api/agents".to_string(), None).await?);
    let actor_id = resolve_agent(&agents, login).with_context(|| {
        format!(
            "run {short} signs as {login:?}, which no agents row names — register it (an \
             `[[agent]]` in seeds/agents.toml with that login among its aliases, then `boss \
             tenant publish`) so the run's record can name its CPU; the report itself is on \
             the packet"
        )
    })?;
    let record = run_record(&run, &actor_id, report, now, outcome)?;
    let out = api_at(Method::POST, "/api/agent-runs".to_string(), Some(record))
        .await
        .with_context(|| format!("recording run {short} in agent_runs"))?;
    eprintln!(
        "{}",
        record_line(short, &actor_id, out.as_ref(), report).map_err(|e| anyhow::anyhow!("{e}"))?
    );
    Ok(())
}

/// `boss dispatch --report <run> …` — the handback, by whichever hand
/// read it. `now` is the finish instant the record carries, read once
/// at the CLI boundary like every verb's.
pub async fn report(
    run_ref: String,
    summary: String,
    spend_usd: Option<f64>,
    tokens: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    if summary.trim().is_empty() {
        bail!("--summary is the report; it cannot be blank");
    }
    if let Some(d) = spend_usd
        && !(d.is_finite() && d >= 0.0)
    {
        bail!("--spend-usd must be a non-negative number of dollars, got {d}");
    }
    let tokens = tokens
        .as_deref()
        .map(parse_tokens)
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let base = crate::gate::resolve_jobs_base(None)?;
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    report_at(
        &reqwest::Client::new(),
        &base,
        &run_ref,
        &Report {
            summary,
            spend_usd,
            tokens,
        },
        &actor,
        now,
    )
    .await
}

pub async fn run(
    packet_ref: String,
    step: Option<String>,
    model: Option<String>,
    budget: Option<f64>,
    effort: Option<String>,
    force: bool,
) -> Result<()> {
    let base = crate::gate::resolve_jobs_base(None)?;
    let repo = crate::brief::repo_root()?;
    // Named before any read: a run signs as the actor running the
    // verb, and an unnamed dispatch would file the run under nobody.
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    let owner = crate::owner::for_filing_at(&base).await;
    let host = crate::prove::host();
    let worktree = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let over = Overrides {
        model,
        budget_usd: budget,
        effort,
    };
    if let Some(b) = over.budget_usd
        && !(b.is_finite() && b > 0.0)
    {
        bail!("--budget must be a positive number of dollars, got {b}");
    }
    dispatch_at(
        &reqwest::Client::new(),
        &base,
        &repo,
        &packet_ref,
        step.as_deref(),
        None,
        &over,
        force,
        &actor,
        &owner,
        &worktree,
        &host,
        BriefSource::Rendered,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {

    /// THE DISCARDED-PROMPT DOOR (backlog d268b260). `boss dispatch`
    /// claims a step and files a run, then prints the prompt ONCE.
    /// Sending stdout to /dev/null keeps the claim and the run and
    /// loses the deliverable — three times measured, five runs
    /// orphaned on the worst of them.
    #[test]
    fn only_a_provably_discarded_stdout_is_refused() {
        assert!(stdout_goes_nowhere("/dev/null"));

        // Everything else is a legitimate place for a prompt to go, and
        // refusing any of them would be worse than the defect: a pipe
        // into another program, a redirect to a file, a terminal, and
        // the harness's own capture.
        //
        // shared-tmp-ok: these are readlink TARGETS being judged as
        // strings, not paths this test creates or opens — nothing is
        // written anywhere, so there is no fixture to collide over.
        for ok in [
            "pipe:[8675309]",
            "/tmp/prompt.txt",
            "/dev/pts/3",
            "/dev/stdout",
            "socket:[4242]",
            "/home/david/prompts/prompt.txt",
            // Near misses that are not the device.
            "/dev/null.txt",
            "/dev/nullX",
            "/var/dev/null",
        ] {
            assert!(!stdout_goes_nowhere(ok), "{ok} must not be refused");
        }
    }

    /// AND IT MUST NOT FIRE HERE. The test harness captures stdout, so
    /// a check that misread its own fd would refuse every dispatch on
    /// this machine — the failure mode worse than the one it fixes.
    #[test]
    fn the_door_is_silent_when_stdout_is_not_the_null_device() {
        assert!(
            discarded_stdout_refusal().is_none(),
            "the harness's stdout is captured, never /dev/null"
        );
    }

    /// A platform without /proc answers None rather than guessing.
    /// Pinned as the reason the read is `.ok()?` and not an unwrap:
    /// the workstation is macOS and must keep dispatching.
    #[test]
    fn a_missing_proc_entry_refuses_nothing() {
        assert!(std::fs::read_link("/proc/self/fd/this-does-not-exist").is_err());
        assert!(discarded_stdout_refusal().is_none());
    }
    use super::*;
    // An EXPLICIT null says "nothing measured this"; an absent key is
    // a payload that forgot to say, and the API refuses that one.
    use boss_testing::assert_explicit_null;

    fn block() -> Settings {
        Settings {
            profile: "builder".into(),
            model: "opus-5[1m]".into(),
            budget_usd: 5.0,
            effort: "high".into(),
        }
    }

    fn step(slug: &str, status: &str, metadata: Value) -> Value {
        json!({
            "id": format!("{slug}-id"),
            "spec_slug": slug,
            "status": status,
            "title": slug,
            "metadata": metadata,
        })
    }

    /// THE EDGE AND THE RULE THAT FOLLOWS IT (backlog dd6d44b7). The
    /// key `boss dispatch` writes onto the claimed step and the `link`
    /// the delivery rule reads are one fact in two files — a Rust
    /// const and a TOML rule row, which cannot be collapsed because a
    /// registry row is data — so CLAUDE.md §9a asks for the equality
    /// test. Without it the dispatch could be renamed, every test
    /// here would pass, and every analyst run would sit at `building`
    /// until the silence clock aged it out as died, saying nothing.
    #[test]
    fn the_step_edge_is_the_link_the_delivery_rule_follows() {
        assert_eq!(
            executing_run_patch("5b1d2c3e-0000-4000-8000-000000000001"),
            json!({ crate::gate::AGENT_RUN_KEY: "5b1d2c3e-0000-4000-8000-000000000001" }),
            "the same key the gate stamps on a gate-run"
        );

        let rule = boss_testing::repo_root()
            .join("infra/dispatcher/rules/agent-run-delivers-when-its-step-is-done.toml");
        let text = std::fs::read_to_string(&rule)
            .unwrap_or_else(|e| panic!("read {}: {e}", rule.display()));
        let row: toml::Value = toml::from_str(&text).expect("the rule file parses");
        let args = row["rule"][0]["do"][0]["args"]
            .as_table()
            .expect("the rule's do carries args");
        // The args are expressions, so a string literal is quoted
        // inside the TOML string — `"\"agent_run\""`. Built from the
        // CONST, not spelled out (backlog 1783c6dd): the rule file is
        // DATA and does not move when the const is renamed, so a pin
        // against a literal would keep agreeing with a rule that
        // follows a key nothing writes any more.
        assert_eq!(
            args["link"].as_str(),
            Some(format!("\"{}\"", crate::gate::AGENT_RUN_KEY).as_str()),
            "the rule follows the key dispatch writes"
        );
        assert_eq!(
            args["link_from"].as_str(),
            Some("\"step\""),
            "off the COMPLETING STEP — one packet hosts a run per step"
        );
        assert_eq!(args["steps"].as_str(), Some("\"building\""));
    }
    #[test]
    fn the_block_is_read_off_the_packets_projection_or_the_rows_step() {
        let projected = step(
            "build",
            "ready",
            json!({
                "agent_profile": "builder", "agent_model": "opus-5[1m]",
                "agent_budget_usd": 5, "agent_effort": "high",
            }),
        );
        assert_eq!(block_on_step(&projected), Some(block()));
        // Half a projection is none: the four keys are written together.
        let half = step("build", "ready", json!({ "agent_profile": "builder" }));
        assert_eq!(block_on_step(&half), None);
        assert_eq!(block_on_step(&step("build", "ready", json!({}))), None);

        let row = json!({
            "kind": "backlog-item",
            "steps": [
                { "title": "triage", "kind": "task" },
                { "title": "build", "kind": "task",
                  "agent": { "profile": "builder", "model": "opus-5[1m]", "budget_usd": 5, "effort": "high" } },
            ],
        });
        assert_eq!(block_in_row(&row, "build"), Some(block()));
        assert_eq!(block_in_row(&row, "triage"), None);

        // ONE READER FOR BOTH HALVES (dacee8cc). `boss brief` asks the
        // same function, so the lane a human reads before dispatching
        // cannot differ from the lane the dispatch renders. With no row
        // in hand it answers off the step alone — which is how the
        // dispatch keeps its row read lazy — and a step with no
        // projection and no row answers nothing, which is the refusal.
        assert_eq!(settings_for(&projected, None), Some(block()));
        assert_eq!(settings_for(&projected, Some(&row)), Some(block()));
        assert_eq!(settings_for(&half, None), None);
        assert_eq!(settings_for(&half, Some(&row)), Some(block()));
        assert_eq!(
            settings_for(&step("triage", "ready", json!({})), Some(&row)),
            None
        );
    }

    /// Overrides override only what they name, and the result is held
    /// to the same bar the publish lint holds a block to.
    #[test]
    fn overrides_only_override_and_the_result_is_priced() {
        assert_eq!(resolve(block(), &Overrides::default()), Ok(block()));
        let over = Overrides {
            model: None,
            budget_usd: Some(2.0),
            effort: Some("low".into()),
        };
        let got = resolve(block(), &over).unwrap();
        assert_eq!(got.model, "opus-5[1m]", "untouched");
        assert_eq!((got.budget_usd, got.effort.as_str()), (2.0, "low"));

        let unpriced = Overrides {
            model: Some("claude-opus-5".into()),
            ..Overrides::default()
        };
        let why = resolve(block(), &unpriced).unwrap_err();
        assert!(why.contains("not on the rate card"), "{why}");
        let bad_effort = Overrides {
            effort: Some("max".into()),
            ..Overrides::default()
        };
        assert!(
            resolve(block(), &bad_effort)
                .unwrap_err()
                .contains("low, medium, high")
        );
        let bad_budget = Overrides {
            budget_usd: Some(0.0),
            ..Overrides::default()
        };
        assert!(
            resolve(block(), &bad_budget)
                .unwrap_err()
                .contains("positive")
        );
    }

    /// A packet whose fix is already on main is refused BEFORE the
    /// claim, and the refusal carries the car that carried it.
    ///
    /// Measured 2026-09-19 (a7837d81): d7fef617 and f47861a5 were each
    /// dispatched to a builder at high effort ~21 h and ~19 h after a
    /// car carrying their fix had merged. Both cars named their packet
    /// correctly — the link is not the defect — and both were still
    /// `open` at `Proven in prod`, so the arrival rule that closes the
    /// item had nothing to fire on.
    #[test]
    fn a_packet_whose_car_already_landed_is_refused_with_the_car_named() {
        let item = "f47861a5-2a86-4b8e-bb01-6491377b9499";
        let landed = json!({
            "id": "b8c4267f-0000-0000-0000-000000000000",
            "status": "open",
            "metadata": {
                "backlog_item": item,
                "branch": "fix/an-answered-ops-request-records-its-exit",
                "merged": "true",
                "merge_ref": "60d95aadf8bb",
            },
        });
        let why = landed_work_refusal(item, std::slice::from_ref(&landed)).expect("refused");
        assert!(
            why.contains("fix/an-answered-ops-request-records-its-exit"),
            "{why}"
        );
        assert!(why.contains("60d95aadf8bb"), "{why}");
        assert!(why.contains("b8c4267f"), "{why}");
        // Both repairs named: prove the car, or close the packet.
        assert!(why.contains("boss prove"), "{why}");
        assert!(why.contains("--force"), "{why}");

        // A car still BUILDING the packet is not landed work: that is
        // the ordinary in-flight case and must not refuse.
        let building = json!({
            "id": "c0000000-0000-0000-0000-000000000000",
            "status": "open",
            "metadata": { "backlog_item": item, "branch": "fix/in-flight" },
        });
        assert_eq!(landed_work_refusal(item, &[building]), None);
        assert_eq!(landed_work_refusal(item, &[]), None);

        // THE CONTROL LEG. A server that ignored `metadata=` would
        // answer the unfiltered page, and every dispatch would be
        // refused by the newest landed car of some OTHER packet. The
        // claim is re-read off each row here, so a page that was never
        // narrowed narrows nothing.
        let other = json!({
            "id": "d0000000-0000-0000-0000-000000000000",
            "status": "closed",
            "metadata": {
                "backlog_item": "aaaaaaaa-0000-0000-0000-000000000000",
                "branch": "fix/someone-elses", "outcome": "merged",
            },
        });
        assert_eq!(
            landed_work_refusal(item, std::slice::from_ref(&other)),
            None
        );
        // …and a narrowed page that DOES hold the packet still refuses.
        assert!(landed_work_refusal(item, &[other, landed]).is_some());
    }

    /// The one read behind the refusal: narrowed at the server by the
    /// containment door, with the same encoder every other `metadata=`
    /// caller uses.
    #[test]
    fn the_landed_car_read_is_narrowed_at_the_server() {
        let q = cars_for_item_query("f47861a5-2a86-4b8e-bb01-6491377b9499");
        assert!(
            q.starts_with("/api/jobs?kind=ship-a-change&metadata="),
            "{q}"
        );
        assert!(q.contains("%22backlog_item%22"), "{q}");
        assert!(q.contains("f47861a5-2a86-4b8e-bb01-6491377b9499"), "{q}");
    }

    #[test]
    fn the_refusal_names_the_fix_in_the_rows_own_syntax() {
        let why = no_block_refusal("backlog-item", "build");
        assert!(why.contains("agent = {"), "{why}");
        assert!(
            why.contains("infra/platform/workflows/backlog-item.toml"),
            "{why}"
        );
        assert!(why.contains("boss workflow publish backlog-item"), "{why}");
    }

    #[test]
    fn the_step_is_the_named_one_or_the_only_open_one() {
        let job = json!({
            "id": "39d0b528-ff69-4cb8-ba82-408b641da66c",
            "steps": [
                step("triage", "completed", json!({})),
                step("build", "ready", json!({})),
                step("closed", "pending", json!({})),
            ],
        });
        assert_eq!(choose_step(&job, None).unwrap()["spec_slug"], "build");
        assert_eq!(
            choose_step(&job, Some("build")).unwrap()["spec_slug"],
            "build"
        );
        let err = choose_step(&job, Some("triage")).unwrap_err();
        assert!(err.contains("`triage` is completed"), "{err}");
        assert!(
            choose_step(&job, Some("ghost"))
                .unwrap_err()
                .contains("no step `ghost`")
        );

        let two = json!({
            "id": "39d0b528-ff69-4cb8-ba82-408b641da66c",
            "steps": [step("a", "ready", json!({})), step("b", "active", json!({}))],
        });
        let err = choose_step(&two, None).unwrap_err();
        assert!(err.contains("--step") && err.contains("a, b"), "{err}");
        let none = json!({ "id": "x", "steps": [step("a", "completed", json!({}))] });
        assert!(
            choose_step(&none, None)
                .unwrap_err()
                .contains("nothing to dispatch")
        );
    }

    /// The run's body deserialises into the Job the API parses it as,
    /// carries every key the agent-run schema requires, and the brief
    /// rides it verbatim.
    #[test]
    fn the_run_body_carries_the_schemas_required_keys_and_the_brief() {
        let body = run_body(
            "39d0b528-ff69-4cb8-ba82-408b641da66c",
            "Agent controls car 2",
            "build",
            "claude@algedonic.dev",
            &block(),
            "/work/boss/.claude/worktrees/agent-a5",
            "boss-dev-0",
            "== THE PACKET ==\nverbatim",
            "emp-david",
        );
        assert_eq!(body["kind"], RUN_KIND);
        assert_eq!(body["title"], "builder run: Agent controls car 2");
        assert_eq!(body["owner_id"], "emp-david");
        let md = &body["metadata"];
        for k in ["packet", "step", "agent", "model", "budget_usd", "effort"] {
            assert!(!md[k].is_null(), "{k} is written");
        }
        assert_eq!(md["budget_usd"], 5.0);
        assert_eq!(md["worktree"], "/work/boss/.claude/worktrees/agent-a5");
        assert_eq!(md["host"], "boss-dev-0");
        assert_eq!(md["brief"], "== THE PACKET ==\nverbatim");
        // The API's clock owns the date (no `opened_on` sent, so the
        // filing instant is stamped as `opened_at` — the stamp the died
        // clock rule falls back to); with it, the body is the Job the
        // create handler parses.
        assert!(body.get("opened_on").is_none());
        let mut as_handled = body.clone();
        as_handled["opened_on"] = json!("2026-09-18");
        let _: boss_core::job::Job = serde_json::from_value(as_handled).expect("parses as a Job");
    }

    #[test]
    fn briefed_keeps_the_steps_own_keys_and_the_run_section_names_the_export() {
        let existing = json!({ "metadata": { "authority_role": "platform-admin" } });
        let body = briefed_body(&existing, 4242);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert_eq!(body["metadata"]["prompt_bytes"], "4242");

        let s = run_section("5b1d2c3e-0000-4000-8000-000000000001", &block());
        assert!(s.contains("export BOSS_AGENT_RUN=5b1d2c3e-0000-4000-8000-000000000001"));
        assert!(s.contains("opus-5[1m]") && s.contains("$5") && s.contains("high"));
        // The cap reaches the runner as the flag it passes (car 3).
        assert!(s.contains("--max-budget-usd 5"), "{s}");
    }

    /// The declared effort must reach a CONTROL (backlog e720dd00):
    /// the prompt names the agent definition that runs at it, and the
    /// name is `agent_spec`'s one mapping — not a word retyped here.
    #[test]
    fn the_run_section_names_the_definition_for_the_declared_effort() {
        for effort in boss_jobs::agent_spec::Effort::ALL {
            let settings = Settings {
                effort: effort.as_str().into(),
                ..block()
            };
            let name = boss_jobs::agent_spec::definition_name(effort.as_str());
            let s = run_section("5b1d2c3e-0000-4000-8000-000000000001", &settings);
            assert!(
                s.contains(&format!("subagent_type {name}")),
                "effort {} must name its definition: {s}",
                effort.as_str()
            );
            assert!(
                s.contains(&format!(
                    "{}/{name}.md",
                    boss_jobs::agent_spec::DEFINITIONS_DIR
                )),
                "{s}"
            );
            assert_eq!(subagent_type(&settings), name);
        }
    }

    /// A session loads `.claude/agents/*.md` ONCE, at its start
    /// (backlog e1c4dc93). A definition that arrives after — the car
    /// that added them, an hourly fast-forward of the pod's checkout —
    /// is on disk and unknown to that session, so the hook would name
    /// a `subagent_type` the harness answers `Agent type 'effort-high'
    /// not found` to. Measured live 2026-09-22: that refusal is loud
    /// and immediate, so the cost is not a silent fallback but the
    /// claim and the run this door files BEFORE the Agent call dies.
    #[test]
    fn a_definition_the_session_never_loaded_is_named_before_anything_is_filed() {
        let all: Vec<String> = boss_jobs::agent_spec::Effort::ALL
            .iter()
            .map(|e| boss_jobs::agent_spec::definition_name(e.as_str()))
            .collect();
        // The whole set loaded: nothing to refuse.
        assert!(unloaded(&all, &all).is_empty());
        // A session started before the car that added them.
        assert_eq!(unloaded(&all, &[]), all);
        // One arrived since: only that one is named.
        let older: Vec<String> = all.iter().skip(1).cloned().collect();
        assert_eq!(unloaded(&all, &older), vec![all[0].clone()]);
        // A checkout WITHOUT the definitions refuses nothing — that is
        // `definition_in`'s case, and the caller's own type stands.
        assert!(unloaded(&[], &[]).is_empty());
        assert!(unloaded(&[], &all).is_empty());
        // A name the session loaded that no effort declares is not
        // this door's business.
        assert!(unloaded(&["claude".into()], &[]).is_empty());
    }

    /// `--tokens` is a total or an input,output split, and the split is
    /// the only shape the rate card can price — the record says which.
    #[test]
    fn tokens_parse_as_a_total_or_a_split_and_the_record_carries_the_shape() {
        assert_eq!(parse_tokens("761000"), Ok(Tokens::Total(761_000)));
        assert_eq!(
            parse_tokens("740_000, 21000"),
            Ok(Tokens::Split {
                input: 740_000,
                output: 21_000
            })
        );
        let why = parse_tokens("lots").unwrap_err();
        assert!(why.contains("input,output"), "{why}");
        assert!(parse_tokens("1,2,3").is_err());

        let run = json!({
            "id": "5b1d2c3e-0000-4000-8000-000000000001",
            "kind": "agent-run",
            "metadata": {
                "packet": "39d0b528-ff69-4cb8-ba82-408b641da66c", "step": "build",
                "agent": "claude@algedonic.dev", "model": "opus-5[1m]",
                "opened_at": "2026-09-18T17:00:00Z",
            },
            "steps": [
                { "spec_slug": "briefed", "status": "completed", "completed_at": "2026-09-18T17:05:00Z" },
                { "spec_slug": "reported", "status": "ready" },
            ],
        });
        let at = "2026-09-18T19:00:00Z".parse().unwrap();
        let split = Report {
            summary: "done".into(),
            spend_usd: Some(4.2),
            tokens: Some(Tokens::Split {
                input: 10,
                output: 5,
            }),
        };
        let rec = run_record(&run, "agent-claude", &split, at, "success").unwrap();
        assert_eq!(rec["run_id"], "5b1d2c3e-0000-4000-8000-000000000001");
        assert_eq!(
            rec["actor_id"], "agent-claude",
            "the registered id, never the login"
        );
        assert_eq!(rec["model"], "opus-5[1m]");
        assert_eq!(
            rec["started_at"], "2026-09-18T17:05:00Z",
            "the briefed stamp"
        );
        assert_eq!(rec["finished_at"], "2026-09-18T19:00:00.000Z");
        assert_eq!(rec["job_id"], "39d0b528-ff69-4cb8-ba82-408b641da66c");
        assert_eq!(
            (rec["input_tokens"].as_u64(), rec["output_tokens"].as_u64()),
            (Some(10), Some(5))
        );
        assert!(
            rec.get("total_tokens").is_none(),
            "a split states no total of its own"
        );
        assert_eq!(rec["detail"]["reported_spend_usd"], 4.2);
        // The record deserialises as the run the API parses.
        let parsed: boss_jobs::agent_runs::NewAgentRun = serde_json::from_value(rec).unwrap();
        assert!(parsed.actor_id.is_agent());

        let total = Report {
            summary: "done".into(),
            spend_usd: None,
            tokens: Some(Tokens::Total(761_000)),
        };
        let rec = run_record(&run, "agent-claude", &total, at, "success").unwrap();
        assert_eq!(rec["total_tokens"], 761_000);
        assert!(rec.get("input_tokens").is_none());
        // No stamp on briefed: the packet's opened_at is the start.
        let mut bare = run.clone();
        bare["steps"] = json!([{ "spec_slug": "reported", "status": "ready" }]);
        assert_eq!(
            run_record(&bare, "agent-claude", &total, at, "success").unwrap()["started_at"],
            "2026-09-18T17:00:00Z"
        );
    }

    /// THE CAR THE RUN PRODUCED, AND THE COUNT NOBODY TOOK — the two
    /// ways a row answered where it had nothing (backlog 65c9c05a,
    /// 2026-09-19). Measured against the live table: `branch` on 0 of
    /// 24 rows while `job_id` was on 24 of 24, and `total_tokens` a
    /// flat 0 on 13, meaning "not reported". The branch is on the
    /// evidence the landing rule stamped onto `building`; the absent
    /// count is a stated null, which the API records as NULL.
    #[test]
    fn the_record_names_the_car_and_says_null_where_no_count_was_taken() {
        let run = |building: Value| {
            json!({
                "id": "5b1d2c3e-0000-4000-8000-000000000001",
                "kind": "agent-run",
                "metadata": {
                    "packet": "39d0b528-ff69-4cb8-ba82-408b641da66c", "step": "build",
                    "agent": "claude@algedonic.dev", "model": "opus-5[1m]", "effort": "high",
                    "opened_at": "2026-09-18T17:00:00Z",
                },
                "steps": [
                    { "spec_slug": "briefed", "status": "completed", "completed_at": "2026-09-18T17:05:00Z" },
                    building,
                ],
            })
        };
        // The evidence object `jobs.complete_linked_step` writes: the
        // CLOSING packet's own metadata.branch, under the rule's
        // evidence_key.
        let landed = |key: &str, branch: &str| {
            json!({
                "spec_slug": "building", "status": "completed",
                "metadata": {
                    "result": "gated",
                    key: { "car": "e47f2238-0000-4000-8000-000000000002", "branch": branch },
                },
            })
        };
        let gated = run(landed(
            "gate_run",
            "fix/a-probe-dates-its-cutoff-from-its-own-merge",
        ));
        assert_eq!(
            run_branch(&gated).as_deref(),
            Some("fix/a-probe-dates-its-cutoff-from-its-own-merge"),
            "the gate's green is what lands the run, and it fires first"
        );
        // The arrival rule one hop later stamps the same branch under
        // `car`; a run landed by that path alone must still name it.
        let merged = run(landed(
            "car",
            "feat/the-durable-inbox-claims-from-its-station",
        ));
        assert_eq!(
            run_branch(&merged).as_deref(),
            Some("feat/the-durable-inbox-claims-from-its-station")
        );
        // A run that refused or died produced no car. `None` is the
        // honest answer, not an empty string dressed as one.
        let refused = json!({ "spec_slug": "building", "status": "completed", "metadata": { "result": "refused" } });
        assert_eq!(run_branch(&run(refused)), None);
        let blank = json!({
            "spec_slug": "building", "status": "completed",
            "metadata": { "result": "gated", "gate_run": { "branch": "" } },
        });
        assert_eq!(
            run_branch(&run(blank)),
            None,
            "an empty branch is no branch"
        );

        let at = "2026-09-18T19:00:00Z".parse().unwrap();
        let silent = Report {
            summary: "done".into(),
            spend_usd: None,
            tokens: None,
        };
        let rec = run_record(&gated, "agent-claude", &silent, at, "success").unwrap();
        assert_eq!(
            rec["branch"], "fix/a-probe-dates-its-cutoff-from-its-own-merge",
            "the column nothing used to fill"
        );
        assert_explicit_null!(
            rec,
            "total_tokens",
            "unknown, not zero — and a stated null, not an absent key the API would refuse"
        );
        // The API parses it as the shape that means unknown.
        let parsed: boss_jobs::agent_runs::NewAgentRun = serde_json::from_value(rec).unwrap();
        assert_eq!(parsed.tokens, boss_jobs::agent_runs::TokenUsage::Unreported);
        assert_eq!(parsed.tokens.total(), None);
        assert_eq!(
            parsed.branch.as_deref(),
            Some("fix/a-probe-dates-its-cutoff-from-its-own-merge")
        );
    }

    /// EFFORT AND OUTCOME — the two fields the record dropped (backlog
    /// 8f1de7bf, 2026-09-19). Measured against the live table that
    /// morning: 24 rows, `outcome` the literal `success` on every one
    /// and `effort` on none, so "was high effort more reliable than
    /// low" had no substrate to answer from. The effort is on the run
    /// packet this function already reads; the outcome is the terminal
    /// the run reached, read from `building.result`, never asserted.
    #[test]
    fn the_record_carries_the_effort_and_the_terminal_the_run_reached() {
        let run = |building: Value| {
            json!({
                "id": "5b1d2c3e-0000-4000-8000-000000000001",
                "kind": "agent-run",
                "metadata": {
                    "packet": "39d0b528-ff69-4cb8-ba82-408b641da66c", "step": "build",
                    "agent": "claude@algedonic.dev", "model": "opus-5[1m]", "effort": "high",
                    "opened_at": "2026-09-18T17:00:00Z",
                },
                "steps": [
                    { "spec_slug": "briefed", "status": "completed", "completed_at": "2026-09-18T17:05:00Z" },
                    building,
                ],
            })
        };
        let done = |result: &str| json!({ "spec_slug": "building", "status": "completed", "metadata": { "result": result } });
        // Every terminal the `result` fork can reach, and what each one
        // says about the run: `gated` is the green, `delivered` is a
        // run that ships no car finishing its work, `refused` is an
        // agent that stopped without doing it, `died` is the silence
        // rule's verdict — and silence is refused exactly like failure.
        for (result, outcome) in [
            ("gated", "success"),
            ("delivered", "success"),
            ("refused", "cancelled"),
            ("died", "failed"),
        ] {
            assert_eq!(run_outcome(&run(done(result))), Some(outcome), "{result}");
        }
        // No terminal reached, and a value the fork does not name:
        // neither is an outcome, and a record that cannot read one is
        // not written at all.
        let open = json!({ "spec_slug": "building", "status": "ready", "metadata": {} });
        assert_eq!(run_outcome(&run(open)), None, "building is still open");
        assert_eq!(run_outcome(&run(done("wat"))), None, "not a terminal");

        let at = "2026-09-18T19:00:00Z".parse().unwrap();
        let r = Report {
            summary: "done".into(),
            spend_usd: None,
            tokens: Some(Tokens::Total(761_000)),
        };
        let rec = run_record(&run(done("refused")), "agent-claude", &r, at, "cancelled").unwrap();
        assert_eq!(rec["outcome"], "cancelled", "the terminal, not a literal");
        assert_eq!(
            rec["detail"]["effort"], "high",
            "the effort the run was dispatched at, off the packet"
        );
        // And the API still parses what the record says.
        let parsed: boss_jobs::agent_runs::NewAgentRun = serde_json::from_value(rec).unwrap();
        assert_eq!(parsed.outcome, boss_jobs::agent_runs::RunOutcome::Cancelled);
    }

    /// THE FACT THAT LIVES TWICE (CLAUDE.md 9a). The `result` fork is
    /// authored in `infra/platform/workflows/agent-run.toml` as an
    /// enum; [`run_outcome`] matches its values here. A value added to
    /// the protocol and not to the match falls through to `None`, and
    /// `None` means "no terminal reached" — so `--report` would decline
    /// to record the run in `agent_runs` at all, and the spend of every
    /// run ending that way would be missing from the table the claim
    /// door reads. That is exactly what `delivered` would have done
    /// (backlog a9c6ed5b). Pinned against the bundle rather than a list
    /// spelled twice.
    #[test]
    fn every_value_the_protocol_admits_is_an_outcome_this_records() {
        let run =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .find(|w| w.kind == RUN_KIND)
                .expect("agent-run ships in the platform bundle");
        let field_type = run
            .steps
            .iter()
            .find(|s| s.title == BUILDING_SLUG)
            .expect("agent-run has a `building` step")
            .fields
            .iter()
            .find(|f| f.name == "result")
            .expect("building declares `result`")
            .field_type
            .clone();
        let values: Vec<&str> = field_type.split('|').collect();
        assert!(values.len() >= 4, "the fork is an enum: {field_type}");
        for value in values {
            let packet = json!({
                "id": "5b1d2c3e-0000-4000-8000-000000000002",
                "kind": RUN_KIND,
                "steps": [{
                    "spec_slug": BUILDING_SLUG, "status": "completed",
                    "metadata": { "result": value },
                }],
            });
            assert!(
                run_outcome(&packet).is_some(),
                "the protocol admits `{value}` and this match does not"
            );
        }
    }

    /// The refusal hands over the door (backlog 2e4d7624). A refusal
    /// that names the condition and no command leaves the run holding
    /// its slot against the concurrent-run cap, and the next dispatch
    /// pays for it with a 409. Every value it names is held to the
    /// protocol's own enum, so the door cannot name a result the
    /// Workflow row stopped admitting (CLAUDE.md 9a).
    #[test]
    fn the_no_terminal_refusal_names_the_command_that_ends_the_run() {
        let line = no_terminal_line("5b1d2c3e");
        assert!(
            line.contains("boss step complete 5b1d2c3e --step building --field result=refused"),
            "the refusal must name the command that ends a refused run: {line}"
        );

        let run =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .find(|w| w.kind == RUN_KIND)
                .expect("agent-run ships in the platform bundle");
        let field_type = run
            .steps
            .iter()
            .find(|s| s.title == BUILDING_SLUG)
            .expect("agent-run has a `building` step")
            .fields
            .iter()
            .find(|f| f.name == "result")
            .expect("building declares `result`")
            .field_type
            .clone();
        for value in ["refused", "delivered", "gated", "died"] {
            assert!(
                field_type.split('|').any(|v| v == value),
                "the refusal names `{value}` and the row declares {field_type}"
            );
            assert!(
                line.contains(value),
                "the fork has a `{value}` branch the refusal does not name: {line}"
            );
        }
    }

    #[test]
    fn the_report_rides_the_packet_and_the_step_as_their_own_fields() {
        let r = Report {
            summary: "packet x, branch y, sha z".into(),
            spend_usd: Some(3.5),
            tokens: Some(Tokens::Total(1000)),
        };
        let patch = report_patch(&r);
        assert_eq!(patch["report"], "packet x, branch y, sha z");
        assert_eq!(
            patch["spend_usd"], 3.5,
            "a number on the packet, as the schema says"
        );
        assert_eq!(patch["tokens"], 1000);
        let existing = json!({ "metadata": { "authority_role": "platform-admin" } });
        let body = reported_body(&existing, &r);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert_eq!(body["metadata"]["summary"], "packet x, branch y, sha z");
        assert_eq!(
            body["metadata"]["spend_usd"], "3.5",
            "string fields on the row"
        );
        assert_eq!(body["metadata"]["tokens"], "1000");
        // Nothing is written for what was not given.
        let bare = Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: None,
        };
        assert!(report_patch(&bare).get("spend_usd").is_none());
        assert!(
            reported_body(&existing, &bare)["metadata"]
                .get("tokens")
                .is_none()
        );
    }

    /// A SECOND `--report` on a run already in `agent_runs` writes
    /// nothing — the row is insert-once on `run_id` — and until
    /// 2026-09-22 the verb never said so: it printed the price of the
    /// row already held as though it described the command just run.
    /// Measured on run 54f43757 (backlog b4fd594e): a report carrying
    /// `--tokens 300000,17000` was answered "unpriced — a total-only
    /// token count", which was true of the FIRST report an hour
    /// earlier and of nothing the caller had done. A differing count
    /// is a correction the record cannot take, so it is REFUSED, and
    /// the refusal names both figures rather than the rule.
    #[test]
    fn a_second_report_with_a_different_count_is_refused_naming_the_row_it_did_not_change() {
        let out = json!({
            "recorded": false,
            "run": {
                "run_id": "54f43757", "usd_micros": 1_575_000,
                "total_tokens": 210_000, "recorded_at": "2026-09-19T05:40:33Z",
            },
        });
        let r = Report {
            summary: "handback".into(),
            spend_usd: None,
            tokens: Some(Tokens::Split {
                input: 300_000,
                output: 17_000,
            }),
        };
        let why = record_line("54f43757", "agent-claude", Some(&out), &r)
            .expect_err("a differing second report is refused");
        assert!(why.contains("210000 total"), "the row it holds: {why}");
        assert!(
            why.contains("300000 in / 17000 out"),
            "the figure that is missing: {why}"
        );
        assert!(
            why.contains("2026-09-19T05:40:33Z"),
            "when it was held: {why}"
        );
        assert!(why.contains("insert-once"), "why nothing changed: {why}");
        assert!(
            !why.contains("total-only count"),
            "the old message blamed a shape this report did not send: {why}"
        );
    }

    /// The benign half of the same answer. A retry after a failed
    /// report carries the SAME count — that is what the idempotent
    /// `run_id` is for — so it is not a refusal; it is a statement
    /// that the row already there is the record. It must not advise
    /// `--tokens IN,OUT`: nothing this caller can send would change
    /// the row, and advice that cannot be acted on is the restated
    /// rule the packet named.
    #[test]
    fn a_retried_report_with_the_same_count_says_the_row_is_unchanged_and_advises_nothing() {
        let out = json!({
            "recorded": false,
            "run": {
                "run_id": "54f43757", "usd_micros": 1_575_000,
                "total_tokens": 210_000, "recorded_at": "2026-09-19T05:40:33Z",
            },
        });
        let r = Report {
            summary: "handback".into(),
            spend_usd: None,
            tokens: Some(Tokens::Total(210_000)),
        };
        let line = record_line("54f43757", "agent-claude", Some(&out), &r)
            .expect("an identical retry is not a refusal");
        assert!(line.contains("already held"), "{line}");
        assert!(line.contains("unchanged"), "{line}");
        assert!(
            !line.contains("Give --tokens"),
            "advice that cannot be acted on: {line}"
        );
    }

    /// The first record still says what its figure rests on, and a
    /// blended one still asks for the split — that advice IS
    /// actionable here, because the next report is the first one.
    #[test]
    fn a_first_record_names_its_basis_beside_the_figure() {
        let split = json!({
            "recorded": true,
            "run": { "usd_micros": 4_200_000, "input_tokens": 740_000, "output_tokens": 21_000 },
        });
        let r = Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: Some(Tokens::Split {
                input: 740_000,
                output: 21_000,
            }),
        };
        let line = record_line("54f43757", "agent-claude", Some(&split), &r).expect("recorded");
        assert!(line.contains("$4.2000"), "{line}");
        assert!(line.contains("measured split"), "{line}");

        let blended = json!({
            "recorded": true,
            "run": { "usd_micros": 1_575_000, "total_tokens": 210_000 },
        });
        let r = Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: Some(Tokens::Total(210_000)),
        };
        let line = record_line("54f43757", "agent-claude", Some(&blended), &r).expect("recorded");
        assert!(line.contains("BLENDED, not measured"), "{line}");
        assert!(line.contains("Give --tokens IN,OUT"), "{line}");

        let unpriced = json!({ "recorded": true, "run": { "total_tokens": null } });
        let r = Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: None,
        };
        let line = record_line("54f43757", "agent-claude", Some(&unpriced), &r).expect("recorded");
        assert!(line.contains("unpriced"), "{line}");
        assert!(line.contains("recorded in full either way"), "{line}");
    }

    #[test]
    fn the_runs_cpu_is_resolved_to_its_registered_id() {
        let agents = vec![json!({ "id": "agent-claude", "aliases": ["claude@algedonic.dev"] })];
        assert_eq!(
            resolve_agent(&agents, "claude@algedonic.dev").as_deref(),
            Some("agent-claude")
        );
        assert_eq!(
            resolve_agent(&agents, "agent-claude").as_deref(),
            Some("agent-claude")
        );
        assert_eq!(resolve_agent(&agents, "nobody@example.test"), None);
    }
}

// ---------------------------------------------------------------------
// The wire, against an in-memory jobs API: a stub that serves the
// packet and the workflow row, answers the claim, files the run with
// its steps, and applies the step PUT — so the verb's whole sequence
// is observed, not assumed.
// ---------------------------------------------------------------------
#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const PACKET: &str = "39d0b528-ff69-4cb8-ba82-408b641da66c";
    const RUN: &str = "5b1d2c3e-0000-4000-8000-000000000001";

    #[derive(Clone)]
    struct Log {
        /// Every request: (method, path, body).
        calls: Arc<Mutex<Vec<(String, String, Value)>>>,
    }

    /// One request as the stub reads it: method, path (query stripped),
    /// the full target, and the JSON body.
    type Answer = (&'static str, String);

    /// The HTTP loop every stub here shares: reads one request per
    /// connection, logs it, and answers with what `route` says.
    async fn serve<F>(route: F) -> (String, Log)
    where
        F: Fn(&str, &str, &str, &Value) -> Answer + Send + Sync + 'static,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Log {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let l = log.clone();
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
                l.calls
                    .lock()
                    .unwrap()
                    .push((method.clone(), target.clone(), body.clone()));
                let path = target.split('?').next().unwrap_or("").to_string();
                let (status, resp) = route(&method, &path, &target, &body);
                let out = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{resp}",
                    resp.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), log)
    }

    /// The stub: a packet at `build`, the workflow row, and the run it
    /// files. The claim answers 409 when `claim_conflict` is set. The
    /// instance declares no edit level (the converged, undeclared case
    /// every instance is in today).
    async fn stub(packet: Value, row: Value, claim_conflict: bool) -> (String, Log) {
        stub_at_level(packet, row, claim_conflict, Some(None)).await
    }

    /// `level`: `None` = the instance has no level door at all (404, a
    /// server from before a479faf7); `Some(None)` = the door answers
    /// `null`; `Some(Some(tier))` = a declared level.
    async fn stub_at_level(
        packet: Value,
        row: Value,
        claim_conflict: bool,
        level: Option<Option<&'static str>>,
    ) -> (String, Log) {
        let run: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        serve(move |method, path, target, body| {
            let (status, resp): (&str, String) = match (method, path) {
                    ("GET", p) if p == format!("/api/jobs/{PACKET}") => {
                        ("200 OK", packet.to_string())
                    }
                    ("GET", "/api/tenant/edit-level") => match level {
                        None => ("404 Not Found", "no such route".into()),
                        Some(l) => (
                            "200 OK",
                            json!({ "edit_level": l, "manifest": "/opt/boss/tenant/seeds/tenant.toml" }).to_string(),
                        ),
                    },
                    // The landed-work read (a7837d81): no car names
                    // this packet, so nothing is refused here.
                    ("GET", "/api/jobs") => {
                        ("200 OK", json!({ "data": [], "total": 0 }).to_string())
                    }
                    ("GET", "/api/workflows/backlog-item") => ("200 OK", row.to_string()),
                    ("POST", p) if p.ends_with("/claim") => {
                        if claim_conflict {
                            (
                                "409 Conflict",
                                json!({ "error": "step already claimed or not claimable", "holder": "emp-someone", "status": "active" }).to_string(),
                            )
                        } else {
                            ("200 OK", json!({ "status": "active" }).to_string())
                        }
                    }
                    ("POST", "/api/jobs") => {
                        let mut filed = body.clone();
                        filed["id"] = json!(RUN);
                        filed["steps"] = json!([
                            { "id": "run-claimed", "spec_slug": "claimed", "status": "completed", "metadata": {} },
                            { "id": "run-briefed", "spec_slug": "briefed", "status": "ready",
                              "metadata": { "authority_role": "platform-admin" } },
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
                    ("PUT", p) if p.starts_with(&format!("/api/jobs/{RUN}/steps/")) => {
                        ("204 No Content", String::new())
                    }
                    // The edge onto the CLAIMED step (dd6d44b7) — on
                    // the packet, not the run.
                    ("PATCH", p) if p.starts_with(&format!("/api/jobs/{PACKET}/steps/")) => {
                        ("204 No Content", String::new())
                    }
                    _ => ("404 Not Found", format!("unstubbed {method} {target}")),
                };
            (status, resp)
        })
        .await
    }

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is above this crate")
    }

    /// A packet materialised BEFORE its row declared the block — no
    /// projection on the step — the case this very packet was.
    fn packet_without_projection() -> Value {
        json!({
            "id": PACKET,
            "kind": "backlog-item",
            "title": "Agent controls car 2",
            "status": "open",
            "priority": "standard",
            "opened_on": "2026-09-18",
            "metadata": { "detail": "Decided as proposed, after car 1." },
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "completed", "metadata": {} },
                { "id": "s-build", "spec_slug": "build", "status": "ready", "title": "Build the change",
                  "metadata": { "authority_role": "platform-admin" } },
            ],
        })
    }

    fn row_with_block() -> Value {
        json!({
            "kind": "backlog-item",
            "version": 7,
            "steps": [
                { "title": "triage", "kind": "task" },
                { "title": "build", "kind": "task",
                  "agent": { "profile": "builder", "model": "opus-5[1m]", "budget_usd": 5, "effort": "high" } },
            ],
        })
    }

    /// The whole sequence: read, resolve the block off the row, claim,
    /// file, read back, print, complete `briefed` — and the prompt is
    /// the brief plus the run section, byte for byte what the packet
    /// records.
    #[tokio::test]
    async fn a_dispatch_claims_files_prints_and_briefs() {
        let (base, log) = stub(packet_without_projection(), row_with_block(), false).await;
        let over = Overrides {
            budget_usd: Some(3.0),
            ..Overrides::default()
        };
        let prompt = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            None,
            None,
            &over,
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/work/boss/.claude/worktrees/agent-x",
            "boss-dev-0",
            BriefSource::Rendered,
        )
        .await
        .expect("dispatches")
        .prompt;

        let calls = log.calls.lock().unwrap().clone();
        let seq: Vec<(String, String)> = calls
            .iter()
            .map(|(m, p, _)| (m.clone(), p.split('?').next().unwrap().to_string()))
            .collect();
        assert_eq!(
            seq,
            vec![
                ("GET".to_string(), format!("/api/jobs/{PACKET}")),
                // The landed-work read (a7837d81), BEFORE the claim:
                // a packet a merged car already names is refused while
                // nothing has been claimed and nothing filed.
                ("GET".to_string(), "/api/jobs".to_string()),
                ("GET".to_string(), "/api/workflows/backlog-item".to_string()),
                (
                    "POST".to_string(),
                    format!("/api/jobs/{PACKET}/steps/s-build/claim")
                ),
                // THE BRIEF IS RENDERED FROM A RE-READ, AFTER THE
                // CLAIM (c8faa7f3): the claim door moves the step to
                // `active`, and the brief used to be rendered before
                // it, so every dispatched agent read `status ready`
                // about a step it already held.
                ("GET".to_string(), format!("/api/jobs/{PACKET}")),
                ("POST".to_string(), "/api/jobs".to_string()),
                (
                    "PATCH".to_string(),
                    format!("/api/jobs/{PACKET}/steps/s-build/metadata")
                ),
                ("GET".to_string(), format!("/api/jobs/{RUN}")),
                (
                    "PUT".to_string(),
                    format!("/api/jobs/{RUN}/steps/run-briefed")
                ),
            ],
            "the claim precedes the filing, the edge follows the run that exists, \
             and the brief precedes the completion"
        );

        // THE EDGE (dd6d44b7): the claimed step says which run is
        // executing it, so nothing downstream has to infer it — the
        // crew board's BUILDING lane, and the rule that delivers an
        // analyst run when this step completes. Found by WHAT it is
        // rather than by index, for the same reason the filing below
        // is: the landed-work read (a7837d81) shifted every position.
        let edge = &calls
            .iter()
            .find(|(m, p, _)| m == "PATCH" && p.ends_with("/steps/s-build/metadata"))
            .expect("the claimed step names the run executing it")
            .2;
        assert_eq!(
            *edge,
            json!({ "agent_run": RUN }),
            "a merging PATCH naming the run, and nothing else"
        );

        // The filing, found by WHAT it is rather than by where it sits
        // in the sequence: the index moved when the landed-work read
        // was added in front of the claim (a7837d81), and a positional
        // read of a call log re-breaks on every such addition.
        let filed = &calls
            .iter()
            .find(|(m, p, _)| m == "POST" && p == "/api/jobs")
            .expect("the run is filed")
            .2;
        assert_eq!(filed["kind"], "agent-run");
        assert_eq!(filed["metadata"]["packet"], PACKET);
        assert_eq!(filed["metadata"]["step"], "build");
        assert_eq!(filed["metadata"]["agent"], "claude@algedonic.dev");
        assert_eq!(
            filed["metadata"]["model"], "opus-5[1m]",
            "from the row's block"
        );
        assert_eq!(
            filed["metadata"]["budget_usd"], 3.0,
            "the override, and only it"
        );
        assert_eq!(filed["metadata"]["effort"], "high");
        assert_eq!(
            filed["metadata"]["worktree"],
            "/work/boss/.claude/worktrees/agent-x"
        );
        assert_eq!(filed["metadata"]["host"], "boss-dev-0");
        let brief = filed["metadata"]["brief"].as_str().unwrap();
        assert!(
            prompt.starts_with(brief),
            "the prompt IS the recorded brief, then the run"
        );
        assert!(
            brief.contains("Decided as proposed, after car 1."),
            "the packet, verbatim"
        );
        assert!(brief.contains("== THE INVARIANTS"));
        assert!(brief.contains("# Builder rules"), "the profile's document");
        assert!(prompt.contains(&format!("export BOSS_AGENT_RUN={RUN}")));

        let briefed = &calls
            .iter()
            .find(|(m, p, _)| m == "PUT" && p.ends_with("/steps/run-briefed"))
            .expect("the briefed step is completed")
            .2;
        assert_eq!(briefed["status"], "completed");
        assert_eq!(
            briefed["metadata"]["prompt_bytes"],
            prompt.len().to_string()
        );
        assert_eq!(briefed["metadata"]["authority_role"], "platform-admin");
    }

    /// A step nobody declared a block for is refused BEFORE the claim —
    /// nothing is claimed, nothing is filed — and the refusal names the
    /// fix.
    #[tokio::test]
    async fn a_step_with_no_block_is_refused_before_anything_is_claimed() {
        let row_without = json!({
            "kind": "backlog-item",
            "steps": [{ "title": "build", "kind": "task" }],
        });
        let (base, log) = stub(packet_without_projection(), row_without, false).await;
        let err = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            None,
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Rendered,
        )
        .await
        .expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains("declares no agent block"), "{text}");
        assert!(text.contains("agent = {"), "{text}");
        let writes = log
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .count();
        assert_eq!(writes, 0, "nothing claimed, nothing filed");
    }

    /// The packet, declaring the paths its change will touch.
    fn packet_declaring(paths: &[&str]) -> Value {
        let mut p = packet_without_projection();
        p["metadata"]["paths"] = json!(paths);
        p
    }

    fn writes_of(log: &Log) -> usize {
        log.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .count()
    }

    fn asked_the_level(log: &Log) -> bool {
        log.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, p, _)| m == "GET" && p.starts_with("/api/tenant/edit-level"))
    }

    /// THE HOSTING DOOR (a479faf7; design 01c3cc3f reader 3). A packet
    /// declaring `metadata.paths` above the instance's edit level is
    /// refused BEFORE the claim — nothing claimed, nothing filed — and
    /// the refusal names the first offending path, its tier and the
    /// level, in the same words the gate's lint prints.
    #[tokio::test]
    async fn a_packet_declaring_paths_above_the_edit_level_is_refused_before_the_claim() {
        let (base, log) = stub_at_level(
            packet_declaring(&["docs/a.md", "crates/core/boss-a/src/lib.rs"]),
            row_with_block(),
            false,
            Some(Some("tenants")),
        )
        .await;
        let err = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            None,
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Rendered,
        )
        .await
        .expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains("crates/core/boss-a/src/lib.rs"), "{text}");
        assert!(text.contains("edit level `tenants` (rank 3)"), "{text}");
        assert!(text.contains("`core` (rank 1)"), "{text}");
        assert!(text.contains("[meta] edit_level"), "names the fix: {text}");
        assert_eq!(writes_of(&log), 0, "nothing claimed, nothing filed");
    }

    /// The same declaration under a level that admits it is dispatched
    /// as any other packet.
    #[tokio::test]
    async fn a_packet_declaring_paths_within_the_edit_level_is_dispatched() {
        let (base, log) = stub_at_level(
            packet_declaring(&["docs/a.md", "crates/tenants/boss-acme-engine/src/main.rs"]),
            row_with_block(),
            false,
            Some(Some("tenants")),
        )
        .await;
        let d = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            None,
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Handed {
                prompt: "p",
                session: None,
            },
        )
        .await
        .expect("dispatched");
        assert_eq!(d.run_id, RUN);
        assert!(asked_the_level(&log));
    }

    /// A packet that declares no paths is admitted WITHOUT reading the
    /// level — the gate decides on the diff — and an instance that
    /// declares no level (null) or has no level door (404, a server
    /// from before this car) admits a declared scope: no level is no
    /// door, never a level that admits nothing.
    #[tokio::test]
    async fn no_declared_paths_or_no_declared_level_admits_the_dispatch() {
        let (base, log) = stub_at_level(
            packet_without_projection(),
            row_with_block(),
            false,
            Some(Some("data")),
        )
        .await;
        let handed = BriefSource::Handed {
            prompt: "p",
            session: None,
        };
        dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            None,
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            handed,
        )
        .await
        .expect("no paths declared: the gate decides");
        assert!(!asked_the_level(&log), "no paths, no level read");

        for level in [Some(None), None] {
            let (base, log) = stub_at_level(
                packet_declaring(&["crates/core/boss-a/src/lib.rs"]),
                row_with_block(),
                false,
                level,
            )
            .await;
            dispatch_at(
                &reqwest::Client::new(),
                &base,
                &repo(),
                PACKET,
                None,
                None,
                &Overrides::default(),
                false,
                "claude@algedonic.dev",
                "emp-david",
                "/wt",
                "h",
                handed,
            )
            .await
            .unwrap_or_else(|e| panic!("level {level:?} enforces nothing: {e:#}"));
            assert!(asked_the_level(&log));
        }
    }

    /// A step someone else holds is a refusal naming the holder, and
    /// no run is filed for it.
    #[tokio::test]
    async fn a_held_step_is_refused_and_no_run_is_filed() {
        let (base, log) = stub(packet_without_projection(), row_with_block(), true).await;
        let err = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            Some("build"),
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Rendered,
        )
        .await
        .expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains("emp-someone"), "{text}");
        assert!(text.contains("claiming `build`"), "{text}");
        let filed = log
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/api/jobs");
        assert!(!filed, "no run for a step this actor does not hold");
    }

    /// A packet whose fix a MERGED car already carries is refused
    /// before anything is claimed or filed (a7837d81).
    ///
    /// Measured 2026-09-19: d7fef617 and f47861a5 were each dispatched
    /// ~21 h and ~19 h after a car carrying their fix had merged, and
    /// the first run was spent entirely re-deriving landed work. Both
    /// cars named their packet correctly; both were still open at
    /// `Proven in prod`, so `ship-a-change` never reached `merged` and
    /// the rule that closes the item never fired. The refusal is read
    /// off that car, never off the tree.
    #[tokio::test]
    async fn a_packet_a_merged_car_already_names_is_refused_before_the_claim() {
        let landed = json!({
            "data": [{
                "id": "b8c4267f-0000-0000-0000-000000000000",
                "status": "open",
                "metadata": {
                    "backlog_item": PACKET,
                    "branch": "fix/an-answered-ops-request-records-its-exit",
                    "merged": "true",
                    "merge_ref": "60d95aadf8bb",
                },
            }],
            "total": 1,
        });
        let packet = packet_without_projection();
        let (base, log) = serve(move |method, path, target, _body| match (method, path) {
            ("GET", p) if p == format!("/api/jobs/{PACKET}") => ("200 OK", packet.to_string()),
            ("GET", "/api/tenant/edit-level") => (
                "200 OK",
                json!({ "edit_level": Value::Null, "manifest": "t.toml" }).to_string(),
            ),
            ("GET", "/api/jobs") => ("200 OK", landed.to_string()),
            _ => ("404 Not Found", format!("unstubbed {method} {target}")),
        })
        .await;
        let err = dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            Some("build"),
            None,
            &Overrides::default(),
            false,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Rendered,
        )
        .await
        .expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains("ALREADY ON MAIN"), "{text}");
        assert!(text.contains("60d95aadf8bb"), "{text}");
        assert!(text.contains("boss prove"), "{text}");
        let calls = log.calls.lock().unwrap().clone();
        assert!(
            !calls
                .iter()
                .any(|(m, p, _)| p.ends_with("/claim") || (m == "POST" && p == "/api/jobs")),
            "nothing claimed and nothing filed: {calls:?}"
        );
    }

    /// …and `--force` is the operator saying the packet asks for more
    /// than that car carried: the same packet dispatches, and the run
    /// is filed. Without this the refusal would be a wall rather than
    /// a door, and the next builder would work around it.
    #[tokio::test]
    async fn force_dispatches_the_same_packet_and_files_the_run() {
        let landed = json!({
            "data": [{
                "id": "b8c4267f-0000-0000-0000-000000000000",
                "status": "open",
                "metadata": { "backlog_item": PACKET, "branch": "fix/x", "merged": "true" },
            }],
            "total": 1,
        });
        let (base, log) = {
            let row = row_with_block();
            let packet = packet_without_projection();
            let run: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
            serve(move |method, path, target, body| match (method, path) {
                ("GET", p) if p == format!("/api/jobs/{PACKET}") => ("200 OK", packet.to_string()),
                ("GET", "/api/tenant/edit-level") => (
                    "200 OK",
                    json!({ "edit_level": Value::Null, "manifest": "t.toml" }).to_string(),
                ),
                ("GET", "/api/jobs") => ("200 OK", landed.to_string()),
                ("GET", "/api/workflows/backlog-item") => ("200 OK", row.to_string()),
                ("POST", p) if p.ends_with("/claim") => {
                    ("200 OK", json!({ "status": "active" }).to_string())
                }
                ("POST", "/api/jobs") => {
                    let mut filed = body.clone();
                    filed["id"] = json!(RUN);
                    filed["steps"] = json!([
                        { "id": "run-briefed", "spec_slug": "briefed", "status": "ready",
                          "metadata": { "authority_role": "platform-admin" } },
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
                ("PUT", p) if p.starts_with(&format!("/api/jobs/{RUN}/steps/")) => {
                    ("204 No Content", String::new())
                }
                // The run edge (dd6d44b7): dispatch writes `agent_run`
                // onto the step it claimed, on every path including
                // this one, so a forced dispatch stubs it too.
                ("PATCH", p) if p.ends_with("/steps/s-build/metadata") => {
                    ("204 No Content", String::new())
                }
                _ => ("404 Not Found", format!("unstubbed {method} {target}")),
            })
            .await
        };
        dispatch_at(
            &reqwest::Client::new(),
            &base,
            &repo(),
            PACKET,
            Some("build"),
            None,
            &Overrides::default(),
            true,
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
            BriefSource::Rendered,
        )
        .await
        .expect("forced past the landed car");
        let calls = log.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|(m, p, _)| m == "POST" && p == "/api/jobs"),
            "the run is filed: {calls:?}"
        );
        assert!(
            !calls.iter().any(|(m, p, _)| m == "GET" && p == "/api/jobs"),
            "and --force does not spend the read it would ignore: {calls:?}"
        );
    }

    /// The run packet as `--report` reads it, with `reported` in the
    /// given state.
    fn run_packet(reported_status: &str) -> Value {
        json!({
            "id": RUN,
            "kind": "agent-run",
            "title": "builder run: Agent controls car 3",
            "status": "open",
            "metadata": {
                "packet": PACKET, "step": "build", "agent": "claude@algedonic.dev",
                "model": "opus-5[1m]", "budget_usd": 5, "effort": "high",
                "opened_at": "2026-09-18T17:00:00Z",
            },
            "steps": [
                { "id": "run-claimed", "spec_slug": "claimed", "status": "completed", "metadata": {} },
                { "id": "run-briefed", "spec_slug": "briefed", "status": "completed",
                  "completed_at": "2026-09-18T17:05:00Z", "metadata": { "prompt_bytes": "4242" } },
                { "id": "run-building", "spec_slug": "building", "status": "completed",
                  "metadata": { "result": "gated" } },
                { "id": "run-reported", "spec_slug": "reported", "status": reported_status,
                  "metadata": { "authority_role": "platform-admin" } },
            ],
        })
    }

    /// The report's stub: the run, the agents registry, and the three
    /// writes answered.
    async fn report_stub(run: Value) -> (String, Log) {
        serve(move |method, path, target, _body| match (method, path) {
            ("GET", p) if p == format!("/api/jobs/{RUN}") => ("200 OK", run.to_string()),
            ("PATCH", p) if p == format!("/api/jobs/{RUN}/metadata") => {
                ("204 No Content", String::new())
            }
            ("PUT", p) if p.starts_with(&format!("/api/jobs/{RUN}/steps/")) => {
                ("204 No Content", String::new())
            }
            ("GET", "/api/agents") => (
                "200 OK",
                json!({ "data": [{ "id": "agent-claude", "aliases": ["claude@algedonic.dev"],
                                   "default_model": "opus-5[1m]" }], "total": 1 })
                .to_string(),
            ),
            ("POST", "/api/agent-runs") => (
                "200 OK",
                json!({ "recorded": true, "run": { "run_id": RUN, "usd_micros": 4_200_000 } })
                    .to_string(),
            ),
            _ => ("404 Not Found", format!("unstubbed {method} {target}")),
        })
        .await
    }

    /// The handback, end to end: the packet holds the report, `reported`
    /// is completed with the row's own fields, and agent_runs holds the
    /// finish under the run's id with the CPU as its registered id.
    #[tokio::test]
    async fn a_report_lands_on_the_packet_the_step_and_the_run_record() {
        let (base, log) = report_stub(run_packet("ready")).await;
        let report = Report {
            summary: "packet cb78818d, branch feat/x, sha abc1234, gate e47f2238".into(),
            spend_usd: Some(4.2),
            tokens: Some(Tokens::Split {
                input: 740_000,
                output: 21_000,
            }),
        };
        report_at(
            &reqwest::Client::new(),
            &base,
            RUN,
            &report,
            "claude@algedonic.dev",
            "2026-09-18T19:00:00Z".parse().unwrap(),
        )
        .await
        .expect("reports");

        let calls = log.calls.lock().unwrap().clone();
        let seq: Vec<(String, String)> = calls
            .iter()
            .map(|(m, p, _)| (m.clone(), p.split('?').next().unwrap().to_string()))
            .collect();
        assert_eq!(
            seq,
            vec![
                ("GET".to_string(), format!("/api/jobs/{RUN}")),
                ("PATCH".to_string(), format!("/api/jobs/{RUN}/metadata")),
                (
                    "PUT".to_string(),
                    format!("/api/jobs/{RUN}/steps/run-reported")
                ),
                ("GET".to_string(), "/api/agents".to_string()),
                ("POST".to_string(), "/api/agent-runs".to_string()),
            ],
            "the packet record first, then the step, then the finish record"
        );
        let patch = &calls[1].2;
        assert_eq!(
            patch["report"],
            "packet cb78818d, branch feat/x, sha abc1234, gate e47f2238"
        );
        assert_eq!(patch["spend_usd"], 4.2);
        assert_eq!(patch["tokens"], 761_000);
        let put = &calls[2].2;
        assert_eq!(put["status"], "completed");
        assert_eq!(put["metadata"]["summary"], patch["report"]);
        assert_eq!(put["metadata"]["spend_usd"], "4.2");
        assert_eq!(put["metadata"]["tokens"], "761000");
        assert_eq!(put["metadata"]["authority_role"], "platform-admin");
        let rec = &calls[4].2;
        assert_eq!(rec["run_id"], RUN);
        assert_eq!(rec["actor_id"], "agent-claude");
        assert_eq!(rec["job_id"], PACKET);
        assert_eq!(rec["started_at"], "2026-09-18T17:05:00Z");
        assert_eq!(rec["input_tokens"], 740_000);
        assert_eq!(rec["detail"]["reported_spend_usd"], 4.2);
        // What the run was dispatched at, and how it went — the two
        // halves of reliability-vs-cost (backlog 8f1de7bf).
        assert_eq!(rec["detail"]["effort"], "high");
        assert_eq!(rec["outcome"], "success", "`building` reached gated");
    }

    /// A run that has reached no terminal records no cost row: the
    /// `outcome` column would have to assert something the packet does
    /// not say, and the row is insert-once, so the assertion would
    /// stick. The report still rides the packet, and a later
    /// `--report` — after the green, the refusal, or the silence
    /// rule — writes the row with the outcome it can then read.
    #[tokio::test]
    async fn a_report_before_any_terminal_records_no_outcome_it_cannot_read() {
        let mut run = run_packet("pending");
        run["steps"][2] = json!({ "id": "run-building", "spec_slug": "building",
                                  "status": "ready", "metadata": {} });
        let (base, log) = report_stub(run).await;
        report_at(
            &reqwest::Client::new(),
            &base,
            RUN,
            &Report {
                summary: "handback".into(),
                spend_usd: None,
                tokens: Some(Tokens::Total(1000)),
            },
            "claude@algedonic.dev",
            "2026-09-18T19:00:00Z".parse().unwrap(),
        )
        .await
        .expect("reports");
        let calls = log.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|(m, p, _)| m == "PATCH" && p.ends_with("/metadata")),
            "the handback is still on the packet: {calls:?}"
        );
        assert!(
            !calls.iter().any(|(m, _, _)| m == "POST"),
            "no row asserting an outcome the run has not reached: {calls:?}"
        );
    }

    /// Before the green, `reported` is pending: the report rides the
    /// packet and the finish is recorded; the step is left for the
    /// green to open. Nothing is written to a step that is not open.
    #[tokio::test]
    async fn a_report_before_the_green_rides_the_packet_and_touches_no_step() {
        let (base, log) = report_stub(run_packet("pending")).await;
        let report = Report {
            summary: "handback".into(),
            spend_usd: None,
            tokens: Some(Tokens::Total(1000)),
        };
        report_at(
            &reqwest::Client::new(),
            &base,
            RUN,
            &report,
            "claude@algedonic.dev",
            "2026-09-18T19:00:00Z".parse().unwrap(),
        )
        .await
        .expect("reports");
        let calls = log.calls.lock().unwrap().clone();
        assert!(
            !calls.iter().any(|(m, _, _)| m == "PUT"),
            "no step write: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|(m, p, _)| m == "PATCH" && p.ends_with("/metadata"))
        );
        let rec = &calls
            .iter()
            .find(|(m, _, _)| m == "POST")
            .expect("recorded")
            .2;
        assert_eq!(rec["total_tokens"], 1000);
        assert!(rec.get("input_tokens").is_none());
    }

    /// A run whose login no agents row names: the report is on the
    /// packet and the step, and the refusal names the registration —
    /// the record cannot name a CPU the registry does not hold.
    #[tokio::test]
    async fn a_report_for_an_unregistered_login_is_refused_at_the_record_naming_the_fix() {
        let mut run = run_packet("ready");
        run["metadata"]["agent"] = json!("stranger@example.test");
        let (base, log) = report_stub(run).await;
        let err = report_at(
            &reqwest::Client::new(),
            &base,
            RUN,
            &Report {
                summary: "s".into(),
                spend_usd: None,
                tokens: None,
            },
            "claude@algedonic.dev",
            "2026-09-18T19:00:00Z".parse().unwrap(),
        )
        .await
        .expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains("stranger@example.test"), "{text}");
        assert!(text.contains("seeds/agents.toml"), "{text}");
        let calls = log.calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|(m, _, _)| m == "PUT"),
            "the step was still completed"
        );
        assert!(
            !calls.iter().any(|(m, _, _)| m == "POST"),
            "nothing recorded under a CPU nobody registered"
        );
    }

    /// End to end, the operator's case on 2026-09-19: the run was
    /// already recorded from a bare total, and a later report gave the
    /// split. The verb exits NONZERO and says the row is unchanged —
    /// it used to exit 0 and print the held row's price as its own
    /// answer, so the split was dropped in silence (backlog b4fd594e).
    #[tokio::test]
    async fn a_second_report_carrying_a_different_count_exits_nonzero_saying_the_row_stands() {
        let run = run_packet("completed");
        let (base, _log) = serve(move |method, path, target, _body| match (method, path) {
            ("GET", p) if p == format!("/api/jobs/{RUN}") => ("200 OK", run.to_string()),
            ("PATCH", p) if p == format!("/api/jobs/{RUN}/metadata") => {
                ("204 No Content", String::new())
            }
            ("GET", "/api/agents") => (
                "200 OK",
                json!({ "data": [{ "id": "agent-claude", "aliases": ["claude@algedonic.dev"],
                                   "default_model": "opus-5[1m]" }], "total": 1 })
                .to_string(),
            ),
            ("POST", "/api/agent-runs") => (
                "200 OK",
                json!({ "recorded": false, "run": { "run_id": RUN, "usd_micros": 1_575_000,
                                                    "total_tokens": 210_000,
                                                    "recorded_at": "2026-09-19T05:40:33Z" } })
                .to_string(),
            ),
            _ => ("404 Not Found", format!("unstubbed {method} {target}")),
        })
        .await;
        let err = report_at(
            &reqwest::Client::new(),
            &base,
            RUN,
            &Report {
                summary: "handback".into(),
                spend_usd: None,
                tokens: Some(Tokens::Split {
                    input: 300_000,
                    output: 17_000,
                }),
            },
            "claude@algedonic.dev",
            "2026-09-19T07:05:00Z".parse().unwrap(),
        )
        .await
        .expect_err("a second report with a different count is refused");
        let text = format!("{err:#}");
        assert!(text.contains("210000 total"), "{text}");
        assert!(text.contains("300000 in / 17000 out"), "{text}");
    }

    // -----------------------------------------------------------------
    // The durable inbox (design 8382bbb2, backlog 923b6571): the verb is
    // handed NO packet, so everything it dispatches it learned from the
    // record.
    // -----------------------------------------------------------------
    mod inbox {
        use super::*;

        const STATION: &str = "a.platform-admin.opus-5-1m";
        /// Head of the queue, and not takeable: its step is somebody's
        /// already. A station holds what is being worked as well as
        /// what is waiting, so the inbox has to tell them apart.
        const HELD: &str = "11111111-0000-4000-8000-000000000001";
        /// The waiting one, behind it.
        const WAITING: &str = "22222222-0000-4000-8000-000000000002";
        const INBOX_RUN: &str = "33333333-0000-4000-8000-000000000003";

        fn agent_metadata() -> Value {
            json!({
                "authority_role": "platform-admin",
                "agent_profile": "builder",
                "agent_model": "opus-5[1m]",
                "agent_budget_usd": 5,
                "agent_effort": "high",
            })
        }

        fn held_packet() -> Value {
            json!({
                "id": HELD, "kind": "backlog-item", "title": "Already being built",
                "status": "open", "priority": "standard", "opened_on": "2026-09-19",
                "metadata": {},
                "steps": [
                    { "id": "h-build", "spec_slug": "build", "status": "active", "title": "Build",
                      "assignee_id": "agent-someone", "metadata": agent_metadata() },
                ],
            })
        }

        fn waiting_packet() -> Value {
            json!({
                "id": WAITING, "kind": "backlog-item", "title": "Waiting for an executor",
                "status": "open", "priority": "standard", "opened_on": "2026-09-19",
                "metadata": { "detail": "The queue is the record." },
                "steps": [
                    { "id": "w-triage", "spec_slug": "triage", "status": "completed",
                      "metadata": {} },
                    { "id": "w-build", "spec_slug": "build", "status": "ready", "title": "Build",
                      "metadata": agent_metadata() },
                ],
            })
        }

        /// The station queue, the packets it names, and the usual
        /// dispatch wire behind them.
        async fn inbox_stub(members: Vec<Value>) -> (String, Log) {
            let queue = json!({
                "station": STATION, "kind": "constraint", "total": members.len(),
                "discipline": ["priority", "age"], "data": members,
            });
            let row = row_with_block();
            let run: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
            serve(move |method, path, target, body| {
                let (status, resp): (&str, String) = match (method, path) {
                    ("GET", p) if p == format!("/api/stations/{STATION}/queue") => {
                        ("200 OK", queue.to_string())
                    }
                    ("GET", p) if p == format!("/api/jobs/{HELD}") => {
                        ("200 OK", held_packet().to_string())
                    }
                    ("GET", p) if p == format!("/api/jobs/{WAITING}") => {
                        ("200 OK", waiting_packet().to_string())
                    }
                    ("GET", "/api/tenant/edit-level") => (
                        "200 OK",
                        json!({ "edit_level": Value::Null, "manifest": "t.toml" }).to_string(),
                    ),
                    // The landed-work read (a7837d81): no car names
                    // this packet, so nothing is refused here.
                    ("GET", "/api/jobs") => {
                        ("200 OK", json!({ "data": [], "total": 0 }).to_string())
                    }
                    ("GET", "/api/workflows/backlog-item") => ("200 OK", row.to_string()),
                    ("POST", p) if p.ends_with("/claim") => {
                        ("200 OK", json!({ "status": "active" }).to_string())
                    }
                    ("POST", "/api/jobs") => {
                        let mut filed = body.clone();
                        filed["id"] = json!(INBOX_RUN);
                        filed["steps"] = json!([
                            { "id": "run-briefed", "spec_slug": "briefed", "status": "ready",
                              "metadata": {} },
                        ]);
                        *run.lock().unwrap() = Some(filed);
                        ("201 Created", json!({ "id": INBOX_RUN }).to_string())
                    }
                    ("GET", p) if p == format!("/api/jobs/{INBOX_RUN}") => {
                        match run.lock().unwrap().clone() {
                            Some(r) => ("200 OK", r.to_string()),
                            None => ("404 Not Found", "no such job".into()),
                        }
                    }
                    ("PUT", p) if p.starts_with(&format!("/api/jobs/{INBOX_RUN}/steps/")) => {
                        ("204 No Content", String::new())
                    }
                    // The edge onto the claimed step (dd6d44b7): a
                    // queued dispatch writes it exactly as a hand one
                    // does — one code path, one door.
                    ("PATCH", p) if p.starts_with(&format!("/api/jobs/{WAITING}/steps/")) => {
                        ("204 No Content", String::new())
                    }
                    _ => ("404 Not Found", format!("unstubbed {method} {target}")),
                };
                (status, resp)
            })
            .await
        }

        async fn take_next(base: &str) -> Result<Option<Dispatched>> {
            next_at(
                &reqwest::Client::new(),
                base,
                &repo(),
                STATION,
                &Overrides::default(),
                "agent-claude",
                "emp-david",
                "/work/boss/.claude/worktrees/agent-x",
                "boss-dev-0",
            )
            .await
        }

        /// THE HAND-OFF IS THE RECORD. The verb is given a station name
        /// and nothing else — no packet, no step, no prompt — and what
        /// it dispatches it learned from the queue. That is the
        /// property the inbox exists for: on 2026-09-19 fifteen runs
        /// were orphaned because the hand-off lived in a session.
        #[tokio::test]
        async fn the_inbox_takes_waiting_work_the_caller_never_named() {
            let (base, log) =
                inbox_stub(vec![json!({ "id": HELD }), json!({ "id": WAITING })]).await;
            let dispatched = take_next(&base)
                .await
                .expect("dispatches")
                .expect("took one");

            let calls = log.calls.lock().unwrap().clone();
            assert_eq!(
                calls[0].1,
                format!("/api/stations/{STATION}/queue"),
                "the queue read is the first thing it does, because it is the only input"
            );
            // The head of the queue is somebody else's work: held, not
            // taken, and not raced for.
            let claims: Vec<String> = calls
                .iter()
                .filter(|(m, p, _)| m == "POST" && p.contains("/claim"))
                .map(|(_, p, _)| p.clone())
                .collect();
            assert_eq!(claims.len(), 1, "one claim, on one packet: {claims:?}");
            assert_eq!(
                claims[0],
                format!("/api/jobs/{WAITING}/steps/w-build/claim?station={STATION}"),
                "the claim names the station it pulled from, so the door gates on that queue"
            );

            let filed = calls
                .iter()
                .find(|(m, p, _)| m == "POST" && p == "/api/jobs")
                .map(|(_, _, b)| b.clone())
                .expect("a run is filed");
            assert_eq!(filed["kind"], "agent-run");
            assert_eq!(filed["metadata"]["packet"], WAITING);
            assert_eq!(filed["metadata"]["step"], "build");
            assert_eq!(dispatched.run_id, INBOX_RUN);
            assert!(
                dispatched.prompt.contains(&INBOX_RUN[..8]),
                "the prompt carries the run the record now holds"
            );
        }

        /// An empty inbox is a normal answer, not an error — a runner
        /// asks again — and it files nothing.
        #[tokio::test]
        async fn an_empty_inbox_claims_nothing_and_files_nothing() {
            let (base, log) = inbox_stub(vec![]).await;
            assert!(take_next(&base).await.expect("reads the queue").is_none());
            let calls = log.calls.lock().unwrap().clone();
            assert_eq!(
                calls.len(),
                1,
                "the queue read, and nothing else: {calls:?}"
            );
        }

        /// A queue of work that is all somebody's: nothing to take, and
        /// still nothing filed.
        #[tokio::test]
        async fn a_queue_of_held_work_takes_none_of_it() {
            let (base, log) = inbox_stub(vec![json!({ "id": HELD })]).await;
            assert!(take_next(&base).await.expect("reads the queue").is_none());
            let calls = log.calls.lock().unwrap().clone();
            assert!(
                !calls.iter().any(|(m, _, _)| m == "POST"),
                "an active step is not raced for: {calls:?}"
            );
        }

        /// What the inbox will hand out, as a pure question about a
        /// packet.
        #[test]
        fn a_waiting_step_is_ready_nobodys_and_carries_the_model() {
            assert_eq!(
                waiting_step(&waiting_packet()).and_then(|s| s["spec_slug"].as_str()),
                Some("build")
            );
            assert!(
                waiting_step(&held_packet()).is_none(),
                "active is not waiting"
            );

            let mut assigned = waiting_packet();
            assigned["steps"][1]["assignee_id"] = json!("agent-someone");
            assert!(
                waiting_step(&assigned).is_none(),
                "ready but held is not waiting — the claim would 409"
            );

            let mut no_block = waiting_packet();
            no_block["steps"][1]["metadata"] = json!({ "authority_role": "platform-admin" });
            assert!(
                waiting_step(&no_block).is_none(),
                "no agent model is not agent work, which is what the station matched on"
            );
        }
    }
}
