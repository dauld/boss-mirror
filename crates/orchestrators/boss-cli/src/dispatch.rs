//! `boss dispatch <packet> [--step <slug>] [--model M --budget B
//! --effort E]` — hand a protocol step to an agent, as a packet.
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
//! The prompt goes to stdout and every status line to stderr, so
//! `boss dispatch <packet> > prompt.txt` is the prompt and nothing else.

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

/// The last part of the prompt: the run's id, and the one export that
/// ties the gate the builder launches back to it.
pub(crate) fn run_section(run_id: &str, settings: &Settings) -> String {
    format!(
        "== THE RUN ==\n\n\
         Your run is agent-run {run_id} (profile `{}`, model {}, budget ${}, effort {}).\n\
         Before `boss gate`, in the shell you gate from: export {}={run_id}\n\
         The gate-run then records this run, and a green lands it by itself.\n",
        settings.profile,
        settings.model,
        settings.budget_usd,
        settings.effort,
        crate::gate::AGENT_RUN_ENV,
    )
}

/// The whole verb against an explicit base — the seam the wire tests
/// go through. Returns the prompt it printed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_at(
    http: &reqwest::Client,
    base: &str,
    repo: &Path,
    packet_ref: &str,
    step_slug: Option<&str>,
    over: &Overrides,
    actor: &str,
    owner: &str,
    worktree: &str,
    host: &str,
) -> Result<String> {
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

    // The brief, rendered ONCE: it is the prompt and the record.
    let brief = crate::brief::render(repo, Some(&job), &settings.profile)?;

    // CLAIM FIRST. A step someone else holds is a refusal naming the
    // holder, and it must come before anything is filed.
    api_at(
        Method::POST,
        format!("/api/jobs/{id}/steps/{step_id}/claim"),
        None,
    )
    .await
    .with_context(|| format!("claiming `{slug}` on {} as {actor}", &id[..8]))?;

    let created = api_at(
        Method::POST,
        "/api/jobs".to_string(),
        Some(run_body(
            &id, &title, &slug, actor, &settings, worktree, host, &brief, owner,
        )),
    )
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
    print!("{prompt}");
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
    Ok(prompt)
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
        &over,
        &actor,
        &owner,
        &worktree,
        &host,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The stub: a packet at `build`, the workflow row, and the run it
    /// files. The claim answers 409 when `claim_conflict` is set.
    async fn stub(packet: Value, row: Value, claim_conflict: bool) -> (String, Log) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Log {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let l = log.clone();
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
                l.calls
                    .lock()
                    .unwrap()
                    .push((method.clone(), target.clone(), body.clone()));
                let path = target.split('?').next().unwrap_or("").to_string();
                let (status, resp): (&str, String) = match (method.as_str(), path.as_str()) {
                    ("GET", p) if p == format!("/api/jobs/{PACKET}") => {
                        ("200 OK", packet.to_string())
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
        (format!("http://{addr}"), log)
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
            &over,
            "claude@algedonic.dev",
            "emp-david",
            "/work/boss/.claude/worktrees/agent-x",
            "boss-dev-0",
        )
        .await
        .expect("dispatches");

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
            &Overrides::default(),
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
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
            &Overrides::default(),
            "claude@algedonic.dev",
            "emp-david",
            "/wt",
            "h",
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
}
