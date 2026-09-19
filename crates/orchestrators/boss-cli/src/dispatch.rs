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
//! card when the tokens are a split, unpriced when they are a total
//! (`TokenUsage`'s own rule). Idempotent end to end: the packet PATCH
//! merges, a completed `reported` is left as it is, and the run row is
//! keyed on the run's id.

use anyhow::{Context, Result, bail};
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

    // The block: the packet's projection, else the active row's step.
    let block = match block_on_step(step) {
        Some(b) => b,
        None => {
            let row = api_at(Method::GET, format!("/api/workflows/{kind}"), None)
                .await
                .with_context(|| format!("reading the {kind} Workflow row for its agent block"))?;
            row.as_ref()
                .and_then(|r| block_in_row(r, &slug))
                .ok_or_else(|| anyhow::anyhow!("{}", no_block_refusal(&kind, &slug)))?
        }
    };
    let settings = resolve(block, over).map_err(|e| anyhow::anyhow!("{e}"))?;

    // The brief, rendered ONCE: it is the prompt and the record. The
    // hook hands the prompt the agent was already given, which is the
    // same fact from the other side.
    let brief = match source {
        BriefSource::Rendered => crate::brief::render(repo, Some(&job), &settings.profile)?,
        BriefSource::Handed { prompt, .. } => prompt.to_string(),
    };

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
             usage line shows (--tokens 740000,21000 as input,output) — only a split is \
             priced by the rate card"
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
/// enum-checked by the Workflow row, so the three values below are the
/// whole fork: `gated` is the gate's green, `refused` is a builder
/// that stopped without building, `died` is the silence rule's verdict
/// — and silence is refused exactly like failure.
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
        "gated" => Some("success"),
        "refused" => Some("cancelled"),
        "died" => Some("failed"),
        _ => None,
    }
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
/// `--tokens` is recorded in full and priced by nothing; a split is
/// priced by the rate card. The reporter's own dollar figure rides
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
        eprintln!(
            "boss dispatch: run {short} has no terminal yet — `{BUILDING_SLUG}` carries no \
             `result`, so agent_runs would have to assert an outcome the packet does not hold. \
             The report is on the packet; run --report again once the run has landed, refused \
             or died, and the cost is recorded with the outcome it reached"
        );
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
    let priced = out
        .as_ref()
        .and_then(|o| o.pointer("/run/usd_micros"))
        .and_then(Value::as_u64);
    match priced {
        Some(micros) => eprintln!(
            "boss dispatch: agent_runs holds run {short} for {actor_id} at ${:.4} (rate card)",
            micros as f64 / 1_000_000.0
        ),
        None => eprintln!(
            "boss dispatch: agent_runs holds run {short} for {actor_id}, unpriced — a total-only \
             token count is recorded in full and priced by nothing; give --tokens IN,OUT to price it"
        ),
    }
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
        // says about the run: `gated` is the green, `refused` is a
        // builder that stopped without building, `died` is the silence
        // rule's verdict — and silence is refused exactly like failure.
        for (result, outcome) in [
            ("gated", "success"),
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
                ("GET".to_string(), "/api/workflows/backlog-item".to_string()),
                (
                    "POST".to_string(),
                    format!("/api/jobs/{PACKET}/steps/s-build/claim")
                ),
                ("POST".to_string(), "/api/jobs".to_string()),
                ("GET".to_string(), format!("/api/jobs/{RUN}")),
                (
                    "PUT".to_string(),
                    format!("/api/jobs/{RUN}/steps/run-briefed")
                ),
            ],
            "the claim precedes the filing, and the brief precedes the completion"
        );

        let filed = &calls[3].2;
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

        let briefed = &calls[5].2;
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
