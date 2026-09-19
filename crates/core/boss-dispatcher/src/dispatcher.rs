//! Core dispatcher logic. Consumes step events, picks role-matched
//! Employees, PUTs assignments.

use crate::config::AssignmentStrategy;
use anyhow::{Context, Result};
use boss_core::partition::Partition;
use boss_jobs::step_registry::{Completion, StepRegistry};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct DispatcherCtx {
    pub jobs_api_url: String,
    pub people_api_url: String,
    pub client: reqwest::Client,
    /// StepType registry — used to look up `required_roles` for the
    /// step's kind, the fallback role source when a step carries no
    /// per-step `authority_role` in its metadata. Without any role match
    /// every assignment would default to the lowest-id employee
    /// (emp-aa-001).
    pub registry: Arc<StepRegistry>,
    /// Cached active roster, refreshed on a short TTL. Pre-fix the
    /// dispatcher fetched the full `/api/people` roster (≈700 employees)
    /// on EVERY assignment, serializing the run_loop on a ~150ms HTTP
    /// round-trip per assign — ~6 assigns/sec, so each newly-ready step
    /// waited seconds in the queue and every pipeline tier inherited that
    /// lag. The cache turns the steady-state assign into a local pick.
    /// Since backlog ab192a9f the roster is the UNION of the people
    /// roster and the registered agents that hold a role
    /// (`roster_union`): a role audience resolves to its holders, and an
    /// agent holding the role is one.
    roster: Arc<tokio::sync::Mutex<RosterCache>>,
    /// Step-assignment distribution strategy, selected by config/data
    /// (`BOSS_DISPATCH_STRATEGY`, default `Spread`). Read by
    /// `pick_employee` to gate which index it takes into the id-sorted
    /// candidate list. Plumbing only — it changes *which* eligible holder
    /// is picked, never the eligibility filter or the determinism.
    strategy: AssignmentStrategy,
}

/// The dispatcher's TTL-cached view of the active roster.
#[derive(Default)]
struct RosterCache {
    fetched_at: Option<std::time::Instant>,
    employees: Vec<Employee>,
}

impl DispatcherCtx {
    pub fn new(jobs_api_url: String, people_api_url: String, strategy: AssignmentStrategy) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            // Dispatcher acts as a system-tier actor; its x-boss-user
            // names that identity so audit_log entries from its PUTs
            // stamp _actor=automation:dispatcher rather than landing
            // anonymously.
            .default_headers({
                let mut h = reqwest::header::HeaderMap::new();
                let actor = serde_json::json!({
                    "id": "automation:dispatcher",
                    "role": "platform-admin",
                    "access_tier": "operator",
                    "territory_account_ids": [],
                    "direct_report_ids": [],
                    "department": "platform",
                })
                .to_string();
                if let Ok(v) = reqwest::header::HeaderValue::from_str(&actor) {
                    h.insert("x-boss-user", v);
                }
                boss_core::machine_token::attach(&mut h);
                h
            })
            .build()
            .expect("reqwest client always builds");
        Self {
            jobs_api_url,
            people_api_url,
            client,
            registry: Arc::new(StepRegistry::v1()),
            roster: Arc::new(tokio::sync::Mutex::new(RosterCache::default())),
            strategy,
        }
    }
}

#[derive(Debug, Deserialize)]
struct StepEventPayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    assignee_id: Option<String>,
    /// Step kind (`task`, `procurement`, `bill-approval`, ...).
    /// Used by the dispatcher to look up `required_roles` from the
    /// StepType registry as the fallback when the step carries no
    /// per-step `authority_role`.
    #[serde(default)]
    kind: Option<String>,
    /// Full step metadata. The per-step `authority_role` the dispatcher
    /// routes on lives HERE — the published Step row nests it under
    /// `metadata`, it is not a top-level event field. (Pre-fix this was
    /// read as a top-level `authority_role`, which is always absent, so
    /// every per-step-role step — e.g. a `task` with
    /// `authority_role = "brewer"` — fell through to the StepType
    /// `required_roles` fallback and, for kinds with none, went
    /// unassigned.)
    #[serde(default)]
    metadata: Option<Value>,
}

/// Max assignments processed in parallel. A strictly-serial loop PUTs one
/// assignment at a time (~one HTTP round-trip each); at warp that trails
/// newly-ready steps and lets them pile up unassigned over a long regen —
/// which starves the workforce of assigned steps to pull. Raised 6 → 12 to
/// keep assignment throughput ahead of the (now 16-wide) workforce at high
/// regen warp; the cluster runs max_connections=400 (was 100), so this +
/// the rules-runner + the workforce stay well under the ceiling, and the
/// JetStream layer redelivers any transient blip rather than dropping it.
const MAX_CONCURRENT_ASSIGNMENTS: usize = 12;

/// Bind a durable JetStream consumer on `jobs.step.>` and dispatch
/// assignments. Runs until the message stream ends.
///
/// Each step event is ACK'd only after its assignment succeeds; a failure
/// NAKs it for redelivery, so a transient people-api/policy hiccup retries
/// instead of leaving the step unassigned forever.
pub async fn run_loop(
    ctx: Arc<DispatcherCtx>,
    js: async_nats::jetstream::Context,
    live: Arc<crate::liveness::DispatcherLiveness>,
) -> Result<()> {
    let messages = boss_nats::durable::open_durable(
        &js,
        boss_nats::durable::STREAM_NAME,
        "dispatcher-steps",
        vec!["jobs.step.>".to_string()],
    )
    .await
    .context("opening durable steps consumer")?;
    // Consumer is bound — readiness probes (and the brewery sim's pre-Go gate)
    // can now see assignment is live, not just that the process answers /health.
    live.mark_assigning();
    info!("dispatcher loop started; durable consumer 'dispatcher-steps' on jobs.step.>");
    // Process step events with bounded concurrency (see
    // MAX_CONCURRENT_ASSIGNMENTS). handle_event reads the roster cache
    // (behind its own mutex) + PUTs one assignment; assignment is
    // idempotent on the assignee-already-set check, so concurrent events
    // — and redelivered ones — are safe. A single Job's steps still go
    // ready in DAG order, so this doesn't reorder a Job's pipeline.
    messages
        .for_each_concurrent(MAX_CONCURRENT_ASSIGNMENTS, |msg| {
            let ctx = ctx.clone();
            let live = live.clone();
            async move {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "dispatcher: message stream error");
                        return;
                    }
                };
                let envelope: Value = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        debug!(error = %e, "skip event with non-JSON payload (ACK)");
                        let _ = msg.ack().await;
                        return;
                    }
                };
                // The publisher wraps every event as
                // `{id, timestamp, source, kind, payload: {...}}`. The step
                // fields (status / assignee_id / job_id / ...) live INSIDE
                // `payload`; deserializing the outer envelope as a step
                // silently yields all-None fields, so unwrap `payload` first.
                let inner = envelope.get("payload").cloned().unwrap_or(envelope);
                let subject = msg.subject.to_string();
                // Inherit the triggering event's partition, same as the
                // rules dispatcher. The assignment path is a SEPARATE
                // consumer, and it was the last hop losing the marker:
                // its writes landed `_simulated: false` on simulated
                // Jobs because nothing here ever set the task-local.
                // The SAME fact also gates who may be assigned (see
                // `partition_permits`), so it is threaded into
                // handle_event explicitly rather than re-read there.
                // The chain is "not real" (packet 508cc38c): a shadow
                // packet's writes fail closed exactly as a simulated
                // one's do.
                let partition = event_partition(&inner);
                let outcome = boss_core::sim_origin::with_sim_chain(
                    partition.fails_closed(),
                    handle_event(&ctx, &subject, &inner, partition),
                )
                .await;
                // ACK on success; NAK (→ redeliver) on failure; dead-letter
                // once the redelivery budget is spent. `settle` logs the
                // failure (a transient NAK is not phrased as the health
                // gate's permanent-failure pattern; only a dead-letter is).
                boss_nats::durable::settle(&msg, outcome).await;
                live.record_assignment();
            }
        })
        .await;
    // Stream ended — the process stays up but is no longer assigning. Flip the
    // flag so /readyz stops reporting ready (the silent-death this guards).
    live.mark_assign_stopped();
    info!("dispatcher loop exited");
    Ok(())
}

/// The assignment-side partition boundary, one checkable question — the
/// exact mirror of the workforce's `row_partition` (boss-sim/workforce.rs)
/// and the one reader every marker consumer shares
/// (`Partition::from_event_payload`): `_partition` when the stamp wrote
/// one, else the older `_simulated` bool; absent, null, or a mis-typed
/// value all read as REAL.
///
/// The mirror direction is load-bearing. "Absent means real" is the
/// partition's one documented posture, and both halves must agree on
/// which side an ambiguous packet falls: the workforce reads ambiguous
/// as real (so the sim never touches it), and if the dispatcher read
/// ambiguous as SIM instead, an ambiguous operator-gated step would be
/// refused by the sim workforce AND kept away from every operator —
/// workable by nobody, a silent conservation leak. Reading it as real
/// keeps the packet routable; a mis-labeled sim packet reaching a human
/// is the filing exercise's defect (9c23395c's prevention finding:
/// exercises must set simulated=true or tear down what they file), and
/// costs the human a glance, not lost work.
fn event_partition(payload: &Value) -> Partition {
    Partition::from_event_payload(payload)
}

/// The role that marks an operator identity — a real login/agent, not a
/// simulated employee. Same literal the sim workforce builds its
/// excluded-assignee set from (`employees_by_role.get("platform-admin")`
/// in boss-brewery-engine/src/lib.rs + the live-roster filter in
/// boss_brewery_sim.rs): the workforce refuses to ACT as these
/// identities; this side refuses to ROUTE sim work to them. One
/// partition, two enforcement points.
const OPERATOR_ROLE: &str = "platform-admin";

/// May a packet from this partition be assigned to an employee with this
/// role? The one rule of the 9c23395c fix: a packet that is NOT REAL
/// must never be assigned to an operator identity — the five `[sim]
/// decision-routing probe` packets landed in David's real queue through
/// the owner-preference path because no assignment route checked the
/// partition. Generalised from "simulated" to `fails_closed()` for the
/// shadow lane (packet 508cc38c): a shadow packet is refused an
/// operator exactly as a simulated one is. Real packets are untouched
/// in every direction.
fn partition_permits(partition: Partition, employee_role: &str) -> bool {
    partition.is_real() || employee_role != OPERATOR_ROLE
}

async fn handle_event(
    ctx: &DispatcherCtx,
    subject: &str,
    payload: &Value,
    partition: Partition,
) -> Result<()> {
    let step: StepEventPayload =
        serde_json::from_value(payload.clone()).context("parsing step payload")?;
    let Some(status) = step.status.as_deref() else {
        return Ok(());
    };
    // Workable statuses are the ones an executor could pick up RIGHT
    // NOW. In the v2 workforce model the jobs-api materializes a Job's
    // step graph server-side and emits `jobs.step.created` /
    // `jobs.step.updated` carrying the step's real status — a tier-0
    // (or any newly-unblocked) step lands here as `ready`. We assign
    // those to a role-matched Employee so the workforce, which only
    // drives ASSIGNED steps, has something to pull. `active` is kept
    // only as a defensive net: a claimed step always carries an
    // assignee, so the assignee_id-already-set check below
    // short-circuits it; were some path ever to emit an unassigned
    // active step we'd still route it. Idempotency is that assignee
    // check — an assigned step never re-routes through the dispatcher,
    // no matter how many status flips fire (the assignment PUT itself
    // emits a `jobs.step.updated`).
    if status != "ready" && status != "active" {
        return Ok(());
    }
    if born_placed(&step) {
        return Ok(());
    }
    let Some(job_id) = step.job_id.as_deref() else {
        debug!(subject = %subject, "step event missing job_id; skipping");
        return Ok(());
    };
    let Some(step_id) = step.id.as_deref() else {
        debug!(subject = %subject, "step event missing id; skipping");
        return Ok(());
    };
    // Agent-executed steps (gates, order-intake, billing) are completed by
    // the dispatcher itself on `step.ready` (gate.resolve / jobs.complete_step)
    // at computer speed — never assigned to a person. Skip assignment so they
    // don't sit unworked in a human's queue and throttle the pipeline at warp.
    if let Some(kind) = step.kind.as_deref()
        && ctx
            .registry
            .get(kind)
            .is_some_and(|st| st.completion == Completion::Agent)
    {
        debug!(
            job_id,
            step_id, kind, "agent step — dispatcher executes, not assigned"
        );
        return Ok(());
    }
    // CLAIMABLE: the protocol asked for a role QUEUE, not a
    // nomination. Leave the step unassigned and let any holder of its
    // `authority_role` take it.
    //
    // The loop below resolves a role-gated step to ONE eligible
    // employee and assigns it. Where a role has a single holder that
    // is not routing, it is a permanent nomination — every
    // `platform-admin` step lands on the same person and nobody else
    // can pick one up even holding the role. Measured 2026-08-18: all
    // six open design reviews assigned to `emp-david`, none claimable.
    //
    // Claimability is not authority. `authority_role` still decides
    // who MAY claim and complete, and policy still runs at the claim;
    // this only decides whether the packet arrives pre-nominated or
    // waits in a queue. It is protocol data, so making a step
    // claimable is a Workflow edit rather than a deploy (§9).
    //
    // HUMAN-ONLY is the same shape with a stronger reason (c17871fe).
    // The protocol asked for a PERSON, and every nomination below —
    // the executor pick, the owner pick, the hash pick — can name an
    // agent: the executor is one by definition, and the roster's
    // `platform-admin` holders have included the agent session. On
    // 2026-09-08 `Kill the old one` (destructive, human_only) reached
    // the executor that way. So a human-only step is never nominated:
    // it waits in its role queue, and the jobs API refuses any
    // non-person who tries to take it. Read off the step's metadata
    // because that is where materialisation put the declaration.
    if let Some(reason) = left_for_role_queue(step.metadata.as_ref()) {
        debug!(
            job_id,
            step_id, reason, "step is left for its role queue, not nominated"
        );
        return Ok(());
    }
    // Resolve required roles: prefer the per-step authority_role
    // if the event carries one (some publishers may add it in
    // the future), then fall back to the StepType registry's
    // required_roles list. Try each role in order — the first
    // one with an eligible employee wins. Empty list = any
    // employee qualifies (today's `task` / `outcome` / `trigger`
    // kinds, which carry no role constraint).
    let mut role_candidates: Vec<&str> = Vec::new();
    if let Some(r) = step
        .metadata
        .as_ref()
        .and_then(|m| m.get("authority_role"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        role_candidates.push(r);
    }
    if let Some(step_kind) = step.kind.as_deref()
        && let Some(step_type) = ctx.registry.get(step_kind)
    {
        for r in step_type.required_roles {
            if !role_candidates.contains(r) {
                role_candidates.push(r);
            }
        }
    }
    // No role constraint at all (StepType has empty
    // required_roles AND the event carries no authority_role) →
    // skip auto-assignment. These are generic "task" / "outcome"
    // / "milestone" kinds that any operator can pick up; auto-
    // assigning them to the lowest-id employee was a footgun (the
    // CEO got 23 generic tasks in early testing). Leaving
    // them unassigned matches reality: an operator picks them off
    // a queue.
    if role_candidates.is_empty() {
        debug!(job_id, step_id, "no role constraint; leaving for operator");
        return Ok(());
    }
    // DECIDES vs EXECUTES (David, 291a73a7, option c). A decision-
    // shaped step — a verdict someone renders — goes to the role's
    // human holder, exactly as before. Executable work whose role the
    // configured executor holds goes to the executor first, with the
    // human always able to claim. Measured before this existed: all
    // 16 open feedback packets' steps parked on the one platform-admin
    // holder, three agent-workable builds among them, blocked for
    // days.
    //
    // The flag is registry data (`StepType::decision_shaped`) — the
    // same fact My Day's "Yours to decide" queue reads, one home. An
    // UNKNOWN kind counts as a decision: erring that way costs the
    // human a glance; erring the other way files a verdict under "an
    // agent will get to it".
    //
    // The executor identity and the roles it may execute for are
    // deployment facts (which agent runs here), not protocol data —
    // same class as BOSS_DISPATCH_STRATEGY. Absent either, behavior
    // is exactly what it was. The station-discipline migration the
    // boundary doc schedules will move this into station data with
    // the rest of matchmaking.
    let decision_shaped = step
        .kind
        .as_deref()
        .and_then(|k| ctx.registry.get(k))
        .map(|t| t.decision_shaped)
        .unwrap_or(true);
    // THE CAPABILITY PICK FIRST (c87fb59b car 3): a step whose block
    // names a model goes to the registered agent whose row holds the
    // role and runs the model. The roster is read only when a block is
    // declared, so a step without one costs nothing here; a roster
    // read that fails falls through to the env executor rather than
    // NAKing — the pick below is the prior behaviour, never worse.
    let step_model = step
        .metadata
        .as_ref()
        .and_then(|m| m.get(boss_jobs::agent_spec::MODEL_KEY))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    if step_model.is_some() {
        match capability_executor_for(
            ctx,
            partition,
            decision_shaped,
            &role_candidates,
            step_model,
        )
        .await
        {
            Ok(Some(agent)) => {
                assign(ctx, job_id, step_id, &agent).await?;
                debug!(
                    job_id,
                    step_id,
                    agent,
                    model = step_model.unwrap_or_default(),
                    "dispatcher assigned the step to the agent serving its (role, model)"
                );
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => {
                debug!(job_id, step_id, error = %e, "roster read failed for the capability pick; using the env executor");
            }
        }
    }
    if let Some(executor) = executor_for(
        partition,
        decision_shaped,
        std::env::var("BOSS_DISPATCH_EXECUTOR_ID").ok().as_deref(),
        std::env::var("BOSS_DISPATCH_EXECUTOR_ROLES")
            .ok()
            .as_deref(),
        &role_candidates,
    ) {
        assign(ctx, job_id, step_id, &executor).await?;
        debug!(
            job_id,
            step_id, executor, "dispatcher assigned executable step to the executor"
        );
        return Ok(());
    }
    // A DECISION-SHAPED step prefers the packet OWNER when the owner
    // holds the authority (be264fa2). The owner is fetched here rather
    // than carried on the event — the step payload has no owner_id — and
    // this is the rare human path, not the hot per-step assign. Every
    // failure to learn the owner or their eligibility falls through to
    // the role pick below: a decision that reaches a role holder is
    // exactly the prior behavior, never worse.
    if decision_shaped {
        let owner = match fetch_job_owner(ctx, job_id).await {
            Ok(o) => o,
            Err(e) => {
                debug!(job_id, step_id, error = %e, "owner lookup failed; using the role pick");
                None
            }
        };
        let owner_holds = match owner.as_deref() {
            Some(o) => owner_is_active_holder(ctx, o, &role_candidates, partition)
                .await
                .unwrap_or(false),
            None => false,
        };
        if let Some(owner_id) = owner_assignee(decision_shaped, owner.as_deref(), owner_holds) {
            assign(ctx, job_id, step_id, &owner_id).await?;
            debug!(
                job_id,
                step_id, owner_id, "decision-shaped step assigned to its packet owner"
            );
            return Ok(());
        }
    }
    let chosen = pick_employee_with_role_fallback(
        ctx,
        &role_candidates,
        step_id,
        partition,
        decision_shaped,
    )
    .await?;
    let Some((emp_id, role_used)) = chosen else {
        // A role IS required (role_candidates is non-empty) but no active
        // holder was found. This is virtually always transient: at sim start
        // the roster cache is cold — the people projection hasn't caught up
        // with the just-seeded employees — or a new hire's row hasn't landed
        // yet. Returning Err makes `settle` NAK, so JetStream redelivers on
        // the backoff schedule (~98s across MAX_DELIVER attempts); once the
        // roster warms the reassignment succeeds, and the assignee-already-set
        // guard above keeps redelivery idempotent. A genuinely unfillable role
        // exhausts the budget and dead-letters loudly — the correct outcome.
        // A NON-REAL step whose only role holders are operator identities
        // is unfillable BY DESIGN (`partition_permits`): it dead-letters
        // loudly instead of polluting a real queue, and the exercise that
        // filed it learns immediately.
        // The old `Ok(())` here dropped the step on the floor: it was never
        // assigned, so its Job never closed, silently losing work (a
        // conservation violation). This was the brewery day-1 ap-payment-run
        // hang — the first AP run opened before the roster was queryable.
        anyhow::bail!(
            "no eligible employee for step {step_id} (job {job_id}); \
             candidates={role_candidates:?} partition={partition} — \
             NAK for redelivery once the roster warms"
        );
    };
    assign(ctx, job_id, step_id, &emp_id).await?;
    // Per-assignment log at DEBUG, not INFO: at warp the loop assigns
    // many steps/sec; an INFO line each contributed to the syslog flood.
    debug!(
        job_id, step_id, emp_id,
        role = role_used,
        candidates = ?role_candidates,
        "dispatcher assigned step"
    );
    Ok(())
}

/// Try each candidate role in order; first one with an active
/// employee wins. Empty candidate list = any active employee
/// qualifies. Returns (employee_id, role_used) — `role_used` is
/// `""` when no role constraint applied. `step_id` is threaded
/// through so the underlying pick spreads load deterministically
/// across the role's holders (see `pick_employee`).
/// The decides-vs-executes pick (291a73a7, option c), pure so the rule
/// is testable without a roster or an env-mutating test. `Some(id)` =
/// assign the executor; `None` = fall through to the human pick.
///
/// Never fires for a step that is not real (the executor is a REAL
/// registered agent — a deployment fact, not a sim identity — and the
/// 9c23395c rule is that sim packets never enter a real actor's queue;
/// a shadow packet is refused the same way, 508cc38c), never fires
/// for a decision-shaped step, never fires without a configured
/// executor, and only fires when one of the step's candidate roles is a
/// role the executor is declared to execute for — a brewery `brewer`
/// step must not land on the platform agent just because the agent
/// exists.
/// Is this step already somebody's? An assigned step never re-routes
/// through the dispatcher — that is the idempotency guard on every
/// status flip (the assignment PUT itself emits a `jobs.step.updated`)
/// — and it is also how a step that is born placed stays out of every
/// pick below. A Workflow step with an `individual` audience
/// materialises with its `assignee_id` set (f5ebd2e1), and since
/// backlog af796788 the pr-train's task steps name the conductor that
/// way: they were arriving with a role and no assignee, so the
/// executes-lane nominated all seven of every train to the agent alias
/// and the conductor completed them over its head. Pure, so the rule
/// is testable over a materialised packet without an event loop.
fn born_placed(step: &StepEventPayload) -> bool {
    step.assignee_id
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Why a ready step is left in its role queue instead of being
/// nominated to one actor, or `None` to nominate as usual. Two
/// declarations on the materialized step say so: `claimable` (the
/// protocol asked for a queue) and `human_only` (the protocol asked
/// for a person — c17871fe; read through the one reader for that key,
/// `boss_jobs::human_only::declared`, in both spellings the live
/// registry carries). Pure so the rule is testable without an event.
fn left_for_role_queue(metadata: Option<&Value>) -> Option<&'static str> {
    let metadata = metadata?;
    if boss_jobs::human_only::declared(metadata) {
        return Some("human-only");
    }
    metadata
        .get("claimable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then_some("claimable")
}

fn executor_for(
    partition: Partition,
    decision_shaped: bool,
    executor_id: Option<&str>,
    executor_roles: Option<&str>,
    role_candidates: &[&str],
) -> Option<String> {
    if partition.fails_closed() || decision_shaped {
        return None;
    }
    let id = executor_id?.trim();
    if id.is_empty() {
        return None;
    }
    let eligible = executor_roles?
        .split(',')
        .map(str::trim)
        .any(|r| !r.is_empty() && role_candidates.contains(&r));
    if eligible { Some(id.to_string()) } else { None }
}

/// THE CAPABILITY PICK (design c87fb59b car 3, backlog cb78818d): the
/// registered agent that serves the `(role, model)` a step's agent
/// block declares — the same pair `station_projection::agent_stations`
/// gates a station on — nominated ahead of the env executor.
///
/// Fires only for a REAL, executable step that carries `agent_model`
/// (car 1's projection of the block onto the packet) and a role among
/// its candidates; the agent must be active, hold that role and run
/// that model. Lowest id wins, so the pick is deterministic. `None` is
/// "this pick says nothing", and the caller falls through to
/// `executor_for` — the env's single executor, which is the FALLBACK
/// now and retires when every deployment's agents row holds the roles
/// its blocks are declared under. Measured 2026-09-18 on the live
/// registry: agent-claude holds `engineering-agent`, every block sits
/// under `platform-admin`, so this pick names nobody and prod keeps
/// nominating claude@algedonic.dev through BOSS_DISPATCH_EXECUTOR_ID.
/// The retirement is one registry edit (the row's `role`) and one
/// manifest edit (drop the two env lines), in that order.
fn capability_executor(
    partition: Partition,
    decision_shaped: bool,
    role_candidates: &[&str],
    step_model: Option<&str>,
    roster: &[Employee],
) -> Option<String> {
    if partition.fails_closed() || decision_shaped {
        return None;
    }
    let model = step_model?.trim();
    if model.is_empty() {
        return None;
    }
    roster
        .iter()
        .filter(|e| e.is_agent && e.status == "active")
        .filter(|e| role_candidates.contains(&e.role.as_str()))
        .filter(|e| e.models.iter().any(|m| m == model))
        .map(|e| e.id.as_str())
        .min()
        .map(str::to_string)
}

/// `capability_executor` over the SAME TTL-cached roster the role pick
/// and the owner check read, so the three picks never see different
/// rosters.
async fn capability_executor_for(
    ctx: &DispatcherCtx,
    partition: Partition,
    decision_shaped: bool,
    role_candidates: &[&str],
    step_model: Option<&str>,
) -> Result<Option<String>> {
    let mut cache = ctx.roster.lock().await;
    if roster_is_stale(&cache) {
        cache.employees = fetch_active_roster(ctx).await?;
        cache.fetched_at = Some(std::time::Instant::now());
    }
    Ok(capability_executor(
        partition,
        decision_shaped,
        role_candidates,
        step_model,
        &cache.employees,
    ))
}

/// A decision-shaped step routes to the packet OWNER when the owner is
/// an active holder of one of its authority roles — the owner is who the
/// work is *for*. Pure so the routing rule is testable without a roster
/// or a live packet, exactly like `executor_for`.
///
/// WHY (be264fa2). A decision step declares an `authority_role` and no
/// assignee, so the dispatcher picked a role holder by
/// `stable_hash(step_id) % holders`. When a role has more than one
/// holder — `platform-admin` is held by both David and the agent — the
/// hash lands on the agent for ~half the steps, and the packet's
/// `owner_id` was never consulted. Nine design decisions filed 08-28/29
/// routed to the agent, who cannot answer them, and never reached the
/// owner's queue. Triaging an item to a decision is an explicit
/// statement that *someone else* must choose; handing it back to a hash
/// pick is exactly backwards. The owner is on every packet and is the
/// right default when they hold the authority.
///
/// `None` = fall through to the role pick (no owner, owner holds no
/// candidate role, or the step is not a decision): today's behavior
/// unchanged. Only fires for a decision-shaped step — the caller gates
/// on that, and passes it here so the rule reads in one place.
fn owner_assignee(
    decision_shaped: bool,
    owner_id: Option<&str>,
    owner_holds_candidate_role: bool,
) -> Option<String> {
    if !decision_shaped || !owner_holds_candidate_role {
        return None;
    }
    let id = owner_id?.trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

async fn pick_employee_with_role_fallback(
    ctx: &DispatcherCtx,
    role_candidates: &[&str],
    step_id: &str,
    partition: Partition,
    decision_shaped: bool,
) -> Result<Option<(String, String)>> {
    if role_candidates.is_empty() {
        let chosen = pick_employee(ctx, None, step_id, partition, decision_shaped).await?;
        return Ok(chosen.map(|id| (id, String::new())));
    }
    for r in role_candidates {
        if let Some(id) = pick_employee(ctx, Some(r), step_id, partition, decision_shaped).await? {
            return Ok(Some((id, (*r).to_string())));
        }
    }
    Ok(None)
}

/// One roster row: a person as `/api/people` answers it, or a
/// registered agent that holds a role, folded in by `roster_union`.
#[derive(Debug, Clone, Deserialize)]
struct Employee {
    id: String,
    role: String,
    status: String,
    /// A registered agent (from `/api/agents`, backlog ab192a9f), not a
    /// person. Never on the wire from `/api/people`, hence the default;
    /// read by `eligible_candidates`, which admits an agent under the
    /// executor's three guards and nowhere else.
    #[serde(default)]
    is_agent: bool,
    /// The models a registered agent runs — its row's `default_model`,
    /// folded in by `roster_union` (design c87fb59b car 3). Empty for a
    /// person, who runs none. Read by `capability_executor`, which
    /// nominates an agent for a step whose block names one of these.
    #[serde(default)]
    models: Vec<String>,
}

/// The one roster every reader shares: the people roster plus each
/// registered agent that HOLDS a role, as a row of that role (backlog
/// ab192a9f). An agent with no role is not on the roster — it is
/// reachable by id only, which is what a null role means. Pure, so the
/// union is testable without the two HTTP reads.
fn roster_union(
    employees: Vec<Employee>,
    agents: Vec<boss_jobs::agents::AgentRow>,
) -> Vec<Employee> {
    employees
        .into_iter()
        .chain(agents.into_iter().filter_map(|a| {
            a.role.map(|role| Employee {
                id: a.id,
                role,
                status: "active".to_string(),
                is_agent: true,
                models: vec![a.default_model],
            })
        }))
        .collect()
}

/// Roster cache TTL: short enough that a new hire becomes assignable
/// within ~one sim-day at warp; long enough to keep the roster off the
/// hot per-assignment path. One definition, shared by every reader.
const ROSTER_TTL: std::time::Duration = std::time::Duration::from_secs(10);

fn roster_is_stale(cache: &RosterCache) -> bool {
    cache
        .fetched_at
        .map(|t| t.elapsed() >= ROSTER_TTL)
        .unwrap_or(true)
}

/// The active roster: the people API's employees and the jobs API's
/// registered agents, unioned by `roster_union`. The one read every
/// roster reader shares; the TTL cache in `ctx.roster` is what keeps
/// it off the hot path. Either read failing fails the roster — the
/// caller NAKs for redelivery exactly as for a cold people projection,
/// and a roster missing its agents would nominate nobody for a role
/// only an agent holds, silently, which is the defect ab192a9f names.
async fn fetch_active_roster(ctx: &DispatcherCtx) -> Result<Vec<Employee>> {
    let people = format!("{}/api/people", ctx.people_api_url.trim_end_matches('/'));
    let agents = format!("{}/api/agents", ctx.jobs_api_url.trim_end_matches('/'));
    let employees: Vec<Employee> = fetch_json(ctx, &people).await?;
    let listing: AgentsListing = fetch_json(ctx, &agents).await?;
    Ok(roster_union(employees, listing.data))
}

/// `GET /api/agents` answers `{data, total}`; the rows are the
/// registry's own shape.
#[derive(Debug, Deserialize)]
struct AgentsListing {
    data: Vec<boss_jobs::agents::AgentRow>,
}

async fn fetch_json<T: serde::de::DeserializeOwned>(ctx: &DispatcherCtx, url: &str) -> Result<T> {
    let resp = ctx
        .client
        .get(url)
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    resp.json()
        .await
        .with_context(|| format!("decode {url} response"))
}

/// Is `owner_id` an active holder of any of `roles`? Reads the SAME
/// TTL-cached roster `pick_employee` uses (same `roster_is_stale` +
/// `fetch_active_roster`), so the owner check and the role pick can
/// never see a different roster.
async fn owner_is_active_holder(
    ctx: &DispatcherCtx,
    owner_id: &str,
    roles: &[&str],
    partition: Partition,
) -> Result<bool> {
    let mut cache = ctx.roster.lock().await;
    if roster_is_stale(&cache) {
        cache.employees = fetch_active_roster(ctx).await?;
        cache.fetched_at = Some(std::time::Instant::now());
    }
    Ok(is_active_holder(
        &cache.employees,
        owner_id,
        roles,
        partition,
    ))
}

/// The membership question, pure: is `owner_id` an ACTIVE employee whose
/// role is one of `roles`, reachable from this partition? An inactive
/// holder, a wrong role, a different id, or an operator identity on a
/// NON-REAL packet all read false — the same eligibility
/// `pick_employee`'s candidate filter uses, so the owner is preferred
/// only when they could have been picked anyway. (The partition leg is
/// the 9c23395c fix: the five `[sim]` probes reached emp-david through
/// exactly this owner-preference route.)
fn is_active_holder(
    employees: &[Employee],
    owner_id: &str,
    roles: &[&str],
    partition: Partition,
) -> bool {
    employees.iter().any(|e| {
        !e.is_agent
            && e.status == "active"
            && e.id == owner_id
            && roles.contains(&e.role.as_str())
            && partition_permits(partition, &e.role)
    })
}

/// The packet's `owner_id`, read from the jobs API. `None` for any shape
/// the owner-routing must not act on — missing job, absent/empty owner —
/// so the decision falls through to a role holder, never to nobody.
async fn fetch_job_owner(ctx: &DispatcherCtx, job_id: &str) -> Result<Option<String>> {
    let url = format!(
        "{}/api/jobs/{}",
        ctx.jobs_api_url.trim_end_matches('/'),
        job_id
    );
    let body: serde_json::Value = ctx
        .client
        .get(&url)
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .context("decode job response")?;
    Ok(owner_id_from_job_body(&body))
}

/// Read `owner_id` out of a job response, pure. Tolerates the two shapes
/// the jobs API returns a job in — bare, or wrapped in `{data: ...}` —
/// and treats an absent or blank owner as `None` so the routing falls
/// through rather than assigning to an empty id.
fn owner_id_from_job_body(body: &serde_json::Value) -> Option<String> {
    body.get("data")
        .unwrap_or(body)
        .get("owner_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// FNV-1a hash of a byte slice (64-bit). A fixed, dependency-free, fully
/// deterministic hash — the same bytes always yield the same value on every
/// host and build, unlike `std::collections::hash_map::DefaultHasher` which
/// is seeded per-process with a random key. We use it to spread step
/// assignments across role-holders (see `pick_employee`); determinism is the
/// requirement (a step's pick must replay identically across a rebuild), so
/// the standard randomized hasher is explicitly NOT usable here.
fn stable_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Deterministically choose an index into a non-empty, stably-ordered
/// candidate list from the step id. This is the `Spread` strategy's impl.
/// Factored out of `pick_employee` so the load-distribution behaviour
/// (determinism + spread) is unit-testable without standing up the roster
/// cache / HTTP. Caller guarantees `len > 0`.
fn pick_index(step_id: &str, len: usize) -> usize {
    debug_assert!(len > 0, "pick_index requires a non-empty candidate list");
    (stable_hash(step_id.as_bytes()) % len as u64) as usize
}

/// Select an index into a non-empty, id-sorted candidate list under the
/// configured `AssignmentStrategy`. The eligibility filter and stable
/// `sort_by(id)` ordering happen in `pick_employee`; this is *only* the
/// data-selected choice of which eligible holder gets the step. Both
/// branches are deterministic — same (strategy, len, step_id) ⇒ same index
/// — so an assignment replays identically across a rebuild. Caller
/// guarantees `len > 0`, so neither branch ever computes `% 0`.
fn pick_index_for(strategy: AssignmentStrategy, step_id: &str, len: usize) -> usize {
    debug_assert!(
        len > 0,
        "pick_index_for requires a non-empty candidate list"
    );
    match strategy {
        // Legacy parity: the lowest-id holder (index 0 of the sorted list).
        AssignmentStrategy::LowestId => 0,
        // Default: spread distinct step ids across all holders by hash.
        AssignmentStrategy::Spread => pick_index(step_id, len),
    }
}

/// Pick an active employee (optionally role-filtered) from the TTL-cached
/// roster, refreshing it from the people-api when stale. Which eligible
/// holder gets the step is chosen by the configured `AssignmentStrategy`
/// (`ctx.strategy`, data-selected via `BOSS_DISPATCH_STRATEGY`): `Spread`
/// (default) fans the pick across all holders by a deterministic hash of
/// the step id, so load distributes instead of piling onto one employee;
/// `LowestId` reproduces the original `candidates.first()` behavior (index 0
/// of the sorted list), kept selectable for parity/debugging.
///
/// Every strategy is deterministic: `sort_by(id)` makes the candidate
/// ordering stable, and each branch is a fixed function of the step id, so
/// the same (strategy, candidate set, step id) triple always selects the
/// same employee (a local, repeatable op, NOT a stateful round-robin
/// counter). `Spread` left unfixed (the original lowest-id-only pick) pinned
/// every step of a role onto a single employee while every other holder sat
/// idle; `Index = stable_hash(step_id) % candidates.len()` distributes
/// distinct step ids roughly evenly across the holders.
///
/// `step_id` is the id of the step being assigned. The candidate set is
/// whatever the roster cache holds at assignment time; as the roster grows
/// (hires land), later steps simply distribute over the larger set — each
/// step is assigned exactly once, so we make no attempt to keep a single
/// step's pick stable across roster-size changes.
async fn pick_employee(
    ctx: &DispatcherCtx,
    role: Option<&str>,
    step_id: &str,
    partition: Partition,
    decision_shaped: bool,
) -> Result<Option<String>> {
    let mut cache = ctx.roster.lock().await;
    if roster_is_stale(&cache) {
        cache.employees = fetch_active_roster(ctx).await?;
        cache.fetched_at = Some(std::time::Instant::now());
    }
    let candidates = eligible_candidates(&cache.employees, role, partition, decision_shaped);
    if candidates.is_empty() {
        // No eligible holder — preserve the None contract; never `% 0`.
        return Ok(None);
    }
    let idx = pick_index_for(ctx.strategy, step_id, candidates.len());
    Ok(candidates.get(idx).map(|e| e.id.clone()))
}

/// The candidate pool, pure: active, role-matched (when a role
/// constrains the step), reachable from the packet's partition
/// (`partition_permits` — a NON-REAL packet's pool never contains an
/// operator identity), in stable id order so the strategy index above is
/// reproducible. Factored out of `pick_employee` so the eligibility
/// rule — the surface the 9c23395c defect lived on — is testable
/// without the roster cache / HTTP.
///
/// A registered agent on the roster (backlog ab192a9f) is admitted
/// under the executor's own three guards (`executor_for`, 291a73a7
/// option c): the packet is REAL, the step is EXECUTABLE (a verdict
/// goes to a person), and a role constrains the step — the
/// unconstrained pool stays people. So a role only an agent holds
/// nominates the agent, and everything that reached a person before
/// still does.
fn eligible_candidates<'a>(
    employees: &'a [Employee],
    role: Option<&str>,
    partition: Partition,
    decision_shaped: bool,
) -> Vec<&'a Employee> {
    let agent_permitted = role.is_some() && partition.is_real() && !decision_shaped;
    let mut candidates: Vec<&Employee> = employees
        .iter()
        .filter(|e| e.status == "active")
        .filter(|e| role.map(|r| e.role == r).unwrap_or(true))
        .filter(|e| partition_permits(partition, &e.role))
        .filter(|e| !e.is_agent || agent_permitted)
        .collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    candidates
}

/// PUT /api/jobs/{job_id}/steps/{step_id} with the assignee.
///
/// Retries briefly on `404 step not found`. A step's `STEP_CREATED` event
/// is published BEFORE its projection row is written (log-first ordering —
/// the audit log is the source of truth), so a dispatcher that reacts to
/// that event fast enough can PUT the assignee into the emit→write window
/// and 404. The retry rides out that window so the assignment isn't lost;
/// a step that is genuinely missing still surfaces after the last attempt.
/// `x-sim-origin` for a downstream call, read from the task-local the
/// event loop set from the triggering event. Mirrors the handlers'
/// helper; kept local because this crate does not depend on them.
pub(crate) fn sim_origin_value() -> &'static str {
    if boss_core::sim_origin::is_in_sim_chain() {
        "true"
    } else {
        "false"
    }
}

async fn assign(ctx: &DispatcherCtx, job_id: &str, step_id: &str, emp_id: &str) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 4;
    let url = format!(
        "{}/api/jobs/{}/steps/{}",
        ctx.jobs_api_url.trim_end_matches('/'),
        job_id,
        step_id
    );
    let body = serde_json::json!({ "assignee_id": emp_id });
    let mut attempt = 0;
    loop {
        attempt += 1;
        let resp = ctx
            .client
            .put(&url)
            // Per-call, not a default header: the identity is constant
            // but sim-ness belongs to the event being handled.
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND && attempt < MAX_ATTEMPTS {
            // Emit→write window: back off briefly and re-PUT.
            tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
            continue;
        }
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("PUT {url} returned {status}: {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Partition, StepEventPayload, born_placed, capability_executor, eligible_candidates,
        event_partition, executor_for, is_active_holder, left_for_role_queue, owner_assignee,
        owner_id_from_job_body, partition_permits, pick_index, pick_index_for, roster_union,
        stable_hash,
    };
    use crate::config::AssignmentStrategy;
    use boss_jobs::step_registry::StepRegistry;
    use std::collections::HashMap;

    /// A human-only step is never nominated — not to the executor, not
    /// by the hash pick — it waits in its role queue for a person to
    /// claim (c17871fe). Read off the materialized step's metadata, the
    /// same way `claimable` is, in both spellings the live registry
    /// carries.
    #[test]
    fn a_human_only_step_is_left_for_its_role_queue() {
        use serde_json::json;
        assert_eq!(
            left_for_role_queue(Some(&json!({ "human_only": "true" }))),
            Some("human-only")
        );
        assert_eq!(
            left_for_role_queue(Some(&json!({ "human_only": true }))),
            Some("human-only")
        );
        assert_eq!(
            left_for_role_queue(Some(&json!({ "claimable": true }))),
            Some("claimable")
        );
        // Neither declared: the step is nominated as before.
        assert_eq!(
            left_for_role_queue(Some(&json!({ "human_only": false }))),
            None
        );
        assert_eq!(
            left_for_role_queue(Some(&json!({ "authority_role": "platform-admin" }))),
            None
        );
        assert_eq!(left_for_role_queue(None), None);
    }

    /// FNV-1a is a fixed function of the input bytes — the SAME bytes hash to
    /// the SAME value on every call, host, and process (no per-process seed).
    #[test]
    fn stable_hash_is_a_pure_function_of_the_bytes() {
        let id = "3f8c5a2e-1b6d-4e9a-8c7f-0a1b2c3d4e5f";
        assert_eq!(stable_hash(id.as_bytes()), stable_hash(id.as_bytes()));
        // A known FNV-1a fixed point: the offset basis maps the empty input.
        assert_eq!(stable_hash(b""), 0xcbf29ce484222325);
        // Distinct inputs (overwhelmingly) hash distinctly.
        assert_ne!(stable_hash(b"step-a"), stable_hash(b"step-b"));
    }

    /// (a) Determinism: the same step id + the same roster size always picks
    /// the same candidate index, across repeated calls. This is what lets a
    /// rebuild replay an assignment identically.
    #[test]
    fn pick_index_is_deterministic_for_a_fixed_roster() {
        let step_id = "3f8c5a2e-1b6d-4e9a-8c7f-0a1b2c3d4e5f";
        let len = 7; // a multi-holder role
        let first = pick_index(step_id, len);
        for _ in 0..1000 {
            assert_eq!(pick_index(step_id, len), first, "pick must be repeatable");
        }
        assert!(first < len, "index stays in bounds");
    }

    /// The decides-vs-executes pick (291a73a7, option c), in every
    /// direction it must NOT fire.
    #[test]
    fn executor_takes_executable_platform_work_and_nothing_else() {
        let platform = ["platform-admin"];
        let brewer = ["brewer"];
        // The case the rule exists for: executable, executor configured,
        // role eligible.
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &platform
            ),
            Some("claude@algedonic.dev".to_string())
        );
        // A DECISION never goes to the executor, whatever the config.
        assert_eq!(
            executor_for(
                Partition::Real,
                true,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &platform
            ),
            None
        );
        // A role the executor is not declared for stays with its people
        // — the brewery's brewer steps must not land on the platform
        // agent just because the agent exists.
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &brewer
            ),
            None
        );
        // Unconfigured deployments behave exactly as before.
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                None,
                Some("platform-admin"),
                &platform
            ),
            None
        );
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some(""),
                Some("platform-admin"),
                &platform
            ),
            None
        );
        assert_eq!(
            executor_for(Partition::Real, false, Some("x"), None, &platform),
            None
        );
    }

    /// THE CAPABILITY PICK (design c87fb59b car 3, backlog cb78818d):
    /// a step whose materialised metadata carries the agent block's
    /// `agent_model` beside its `authority_role` is nominated to the
    /// registered agent whose row holds that role and runs that model —
    /// the same `(role, model)` the station projection gates on — and
    /// to nobody else. In every direction it must NOT fire: a decision,
    /// a non-real packet, a step with no block, a role the agent does not
    /// hold, a model the agent does not run, a person of the right role
    /// (people run no model). The env executor stays the FALLBACK, read
    /// only when this pick names nobody — which on the live registry it
    /// does today: agent-claude's row holds `engineering-agent` while
    /// every agent block sits under `platform-admin`, so prod keeps
    /// nominating claude@algedonic.dev through BOSS_DISPATCH_EXECUTOR_*
    /// until the row holds the role (measured 2026-09-18).
    #[test]
    fn a_step_with_an_agent_block_is_nominated_by_capability_match() {
        let agent = |id: &str, role: &str, model: &str| super::Employee {
            id: id.into(),
            role: role.into(),
            status: "active".into(),
            is_agent: true,
            models: vec![model.into()],
        };
        let mut roster = partition_roster();
        roster.push(agent("agent-zed", "platform-admin", "opus-5[1m]"));
        roster.push(agent("agent-claude", "platform-admin", "opus-5[1m]"));
        roster.push(agent("agent-haiku", "platform-admin", "haiku-4-5"));
        roster.push(agent("agent-eng", "engineering-agent", "opus-5[1m]"));
        let platform = ["platform-admin"];
        let pick = |partition, decision, roles: &[&str], model: Option<&str>| {
            capability_executor(partition, decision, roles, model, &roster)
        };
        // The case the rule exists for — and the lowest id wins, so the
        // pick is deterministic across a roster of equals.
        assert_eq!(
            pick(Partition::Real, false, &platform, Some("opus-5[1m]")),
            Some("agent-claude".to_string())
        );
        assert_eq!(
            pick(Partition::Real, false, &platform, Some("haiku-4-5")),
            Some("agent-haiku".to_string())
        );
        // A model no platform-admin agent runs: nobody, so the env
        // fallback decides.
        assert_eq!(
            pick(Partition::Real, false, &platform, Some("sonnet-4-5")),
            None
        );
        // A role the agents do not hold: nobody, even on the right model.
        assert_eq!(
            pick(Partition::Real, false, &["bookkeeper"], Some("opus-5[1m]")),
            None
        );
        // No block on the step: this pick says nothing.
        assert_eq!(pick(Partition::Real, false, &platform, None), None);
        // A decision, or a packet that is not real: never.
        assert_eq!(
            pick(Partition::Real, true, &platform, Some("opus-5[1m]")),
            None
        );
        assert_eq!(
            pick(Partition::Simulated, false, &platform, Some("opus-5[1m]")),
            None
        );
        // The people of the role are not candidates here: emp-agent and
        // emp-david hold platform-admin and run no model.
        let people_only = partition_roster();
        assert_eq!(
            capability_executor(
                Partition::Real,
                false,
                &platform,
                Some("opus-5[1m]"),
                &people_only
            ),
            None
        );
        // An inactive agent is not nominated.
        let mut retired = roster.clone();
        for e in retired.iter_mut().filter(|e| e.is_agent) {
            e.status = "inactive".into();
        }
        assert_eq!(
            capability_executor(
                Partition::Real,
                false,
                &platform,
                Some("opus-5[1m]"),
                &retired
            ),
            None
        );
    }

    /// The owner-routing pick (be264fa2), in every direction it must and
    /// must not fire.
    #[test]
    fn a_decision_routes_to_its_owner_only_when_the_owner_holds_the_role() {
        // The case the rule exists for: a decision, owner holds the role.
        assert_eq!(
            owner_assignee(true, Some("emp-david"), true),
            Some("emp-david".to_string())
        );
        // The owner does NOT hold a candidate role — they cannot decide
        // it, so fall through to the role pick (a car owned by someone
        // without finance authority must not park a finance verdict on
        // them, unclaimable).
        assert_eq!(owner_assignee(true, Some("emp-david"), false), None);
        // NOT a decision — executable work uses the executor / role pick,
        // never the owner. This is what keeps `build` inheritance intact.
        assert_eq!(owner_assignee(false, Some("emp-david"), true), None);
        // No owner, or an empty owner id: fall through, unchanged.
        assert_eq!(owner_assignee(true, None, true), None);
        assert_eq!(owner_assignee(true, Some(""), true), None);
        assert_eq!(owner_assignee(true, Some("  "), true), None);
    }

    /// The job-body parse, over the two shapes the API returns and the
    /// blanks that must read as no-owner.
    #[test]
    fn owner_id_reads_bare_and_wrapped_job_bodies() {
        use serde_json::json;
        assert_eq!(
            owner_id_from_job_body(&json!({"owner_id": "emp-david"})),
            Some("emp-david".to_string())
        );
        assert_eq!(
            owner_id_from_job_body(&json!({"data": {"owner_id": "emp-david"}})),
            Some("emp-david".to_string())
        );
        assert_eq!(
            owner_id_from_job_body(&json!({"kind": "backlog-item"})),
            None
        );
        assert_eq!(owner_id_from_job_body(&json!({"owner_id": null})), None);
        assert_eq!(owner_id_from_job_body(&json!({"owner_id": "   "})), None);
    }

    /// Owner eligibility is the SAME filter pick_employee uses: active,
    /// id-matched, role-matched.
    #[test]
    fn is_active_holder_matches_id_role_and_active_status() {
        let emp = |id: &str, role: &str, status: &str| super::Employee {
            id: id.into(),
            role: role.into(),
            status: status.into(),
            is_agent: false,
            models: vec![],
        };
        let roster = vec![
            emp("emp-david", "platform-admin", "active"),
            emp("emp-gone", "platform-admin", "inactive"),
            emp("emp-brewer", "brewer", "active"),
        ];
        let admin = ["platform-admin"];
        assert!(is_active_holder(
            &roster,
            "emp-david",
            &admin,
            Partition::Real
        ));
        assert!(!is_active_holder(
            &roster,
            "emp-gone",
            &admin,
            Partition::Real
        )); // inactive
        assert!(!is_active_holder(
            &roster,
            "emp-brewer",
            &admin,
            Partition::Real
        )); // wrong role
        assert!(!is_active_holder(
            &roster,
            "emp-ghost",
            &admin,
            Partition::Real
        )); // not on roster
        assert!(!is_active_holder(
            &roster,
            "emp-david",
            &[],
            Partition::Real
        )); // no candidate roles
    }

    /// The assignment-side partition predicate mirrors the workforce's
    /// `row_partition` EXACTLY: `_partition` when present, else only a
    /// literal `_simulated: true` reads as simulated; absent, null,
    /// false, or a mis-typed value all read as REAL. See
    /// `event_partition` for why the mirror direction is load-bearing
    /// (the two halves must agree on which side an ambiguous packet
    /// falls, or it becomes workable by nobody).
    #[test]
    fn the_assignment_boundary_fails_closed_on_shape() {
        use serde_json::json;
        assert_eq!(
            event_partition(&json!({"_simulated": true})),
            Partition::Simulated
        );
        assert_eq!(
            event_partition(&json!({"_partition": "shadow", "_simulated": true})),
            Partition::Shadow
        );
        assert_eq!(
            event_partition(&json!({"_simulated": false})),
            Partition::Real
        );
        assert_eq!(
            event_partition(&json!({})),
            Partition::Real,
            "absent means real"
        );
        assert_eq!(
            event_partition(&json!({"_simulated": null})),
            Partition::Real
        );
        assert_eq!(
            event_partition(&json!({"_simulated": "true"})),
            Partition::Real,
            "a string is not a claim - fail closed on shape too"
        );
    }

    /// The partition rule itself: only the (not-real, operator-role)
    /// pair is refused. Real packets reach operators; sim AND shadow
    /// packets reach every non-operator role and never an operator —
    /// shadow fails closed exactly as simulated does (508cc38c).
    #[test]
    fn the_partition_refuses_exactly_not_real_cross_operator() {
        assert!(!partition_permits(Partition::Simulated, "platform-admin"));
        assert!(!partition_permits(Partition::Shadow, "platform-admin"));
        assert!(partition_permits(Partition::Real, "platform-admin"));
        assert!(partition_permits(Partition::Simulated, "brewer"));
        assert!(partition_permits(Partition::Shadow, "brewer"));
        assert!(partition_permits(Partition::Real, "brewer"));
    }

    fn partition_roster() -> Vec<super::Employee> {
        let emp = |id: &str, role: &str, status: &str| super::Employee {
            id: id.into(),
            role: role.into(),
            status: status.into(),
            is_agent: false,
            models: vec![],
        };
        vec![
            emp("emp-aa-100", "brewer", "active"),
            emp("emp-agent", "platform-admin", "active"),
            emp("emp-david", "platform-admin", "active"),
        ]
    }

    /// The 9c23395c defect, pinned: a SIMULATED packet's step is never
    /// assigned to an operator identity, even when the operator is the
    /// only holder of the step's authority role. The candidate pool
    /// empties instead (→ the caller's no-eligible-employee path, which
    /// dead-letters loudly) rather than polluting a real queue.
    #[test]
    fn a_sim_packet_never_reaches_an_operator_even_holding_the_role() {
        let roster = partition_roster();
        let ids = |v: Vec<&super::Employee>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        // The exact probe shape: authority_role=platform-admin, sim event.
        assert!(
            eligible_candidates(&roster, Some("platform-admin"), Partition::Simulated, false)
                .is_empty(),
            "sim packets must never route to operator identities"
        );
        // Unconstrained sim steps still reach the sim-workable roster.
        assert_eq!(
            ids(eligible_candidates(
                &roster,
                None,
                Partition::Simulated,
                false
            )),
            vec!["emp-aa-100"]
        );
        // Non-operator roles are untouched by the partition.
        assert_eq!(
            ids(eligible_candidates(
                &roster,
                Some("brewer"),
                Partition::Simulated,
                false
            )),
            vec!["emp-aa-100"]
        );
        // And a sim OWNER who is an operator is no longer an active
        // holder for a sim packet — the owner-preference path (the route
        // the five [sim] probes actually took) closes with the same rule.
        assert!(!is_active_holder(
            &roster,
            "emp-david",
            &["platform-admin"],
            Partition::Simulated
        ));
        // The shadow lane closes the same doors (508cc38c).
        assert!(
            eligible_candidates(&roster, Some("platform-admin"), Partition::Shadow, false)
                .is_empty(),
            "shadow packets must never route to operator identities"
        );
        assert_eq!(
            ids(eligible_candidates(&roster, None, Partition::Shadow, false)),
            vec!["emp-aa-100"]
        );
        assert!(!is_active_holder(
            &roster,
            "emp-david",
            &["platform-admin"],
            Partition::Shadow
        ));
    }

    /// Backlog ab192a9f: a step whose audience is a role resolves to
    /// the HOLDERS of the role, and a registered agent that holds it is
    /// one. Until this the roster was the employees table alone, so a
    /// role only an agent held nominated nobody (NAK, then dead-letter)
    /// and an agent was reachable by id only. The agent joins the pool
    /// under the executor's own three guards (`executor_for`): a REAL
    /// packet, an EXECUTABLE (not decision-shaped) step, and a role it
    /// holds — never the unconstrained pool, never a verdict, never a
    /// sim or shadow packet.
    #[test]
    fn a_role_audience_nominates_an_agent_that_holds_the_role_when_no_employee_does() {
        let agent = |id: &str, role: &str| super::Employee {
            id: id.into(),
            role: role.into(),
            status: "active".into(),
            is_agent: true,
            models: vec![],
        };
        let mut roster = partition_roster();
        roster.push(agent("agent-claude", "engineering-agent"));
        let ids = |v: Vec<&super::Employee>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        // No employee holds engineering-agent: the agent is the holder.
        assert_eq!(
            ids(eligible_candidates(
                &roster,
                Some("engineering-agent"),
                Partition::Real,
                false
            )),
            vec!["agent-claude"]
        );
        // A verdict still goes to a person — and here there is none.
        assert!(
            eligible_candidates(&roster, Some("engineering-agent"), Partition::Real, true)
                .is_empty(),
            "a decision-shaped step is never nominated to an agent"
        );
        // A packet that is not real never enters a real actor's queue.
        for not_real in [Partition::Simulated, Partition::Shadow] {
            assert!(
                eligible_candidates(&roster, Some("engineering-agent"), not_real, false).is_empty(),
                "{not_real} must not reach a registered agent"
            );
        }
        // The unconstrained pool is people, as before.
        assert_eq!(
            ids(eligible_candidates(&roster, None, Partition::Real, false)),
            vec!["emp-aa-100", "emp-agent", "emp-david"]
        );
        // A role both hold: both, in stable id order, so the spread
        // pick stays deterministic over the union.
        roster.push(agent("agent-scout", "platform-admin"));
        assert_eq!(
            ids(eligible_candidates(
                &roster,
                Some("platform-admin"),
                Partition::Real,
                false
            )),
            vec!["agent-scout", "emp-agent", "emp-david"]
        );
        // The owner check reads people only: an agent never owns a
        // packet (owner_resolution refuses automation-shaped owners).
        assert!(!is_active_holder(
            &roster,
            "agent-claude",
            &["engineering-agent"],
            Partition::Real
        ));
    }

    /// The roster union, pure: an agent row with a role becomes a
    /// holder of that role; one without a role is not on the roster at
    /// all (reachable by id only), and every employee rides through
    /// untouched.
    #[test]
    fn the_roster_is_employees_and_the_agents_that_hold_a_role() {
        let agent = |id: &str, role: Option<&str>| boss_jobs::agents::AgentRow {
            id: id.into(),
            display_name: id.into(),
            default_model: "opus-5[1m]".into(),
            role: role.map(str::to_string),
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: vec![],
        };
        let roster = roster_union(
            partition_roster(),
            vec![
                agent("agent-claude", Some("engineering-agent")),
                agent("agent-mute", None),
            ],
        );
        let rows: Vec<(&str, bool)> = roster.iter().map(|e| (e.id.as_str(), e.is_agent)).collect();
        assert_eq!(
            rows,
            [
                ("emp-aa-100", false),
                ("emp-agent", false),
                ("emp-david", false),
                ("agent-claude", true),
            ]
        );
        assert_eq!(roster[3].role, "engineering-agent");
        assert_eq!(roster[3].status, "active");
        assert_eq!(
            roster[3].models,
            vec!["opus-5[1m]".to_string()],
            "the agent's default_model is the capability the executor pick matches"
        );
        assert!(roster[0].models.is_empty(), "a person runs no model");
    }

    /// REAL packets are byte-for-byte unaffected: same candidates, same
    /// stable ordering, operators fully reachable.
    #[test]
    fn a_real_packet_reaches_operators_exactly_as_before() {
        let roster = partition_roster();
        let ids = |v: Vec<&super::Employee>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(eligible_candidates(
                &roster,
                Some("platform-admin"),
                Partition::Real,
                false
            )),
            vec!["emp-agent", "emp-david"]
        );
        assert_eq!(
            ids(eligible_candidates(&roster, None, Partition::Real, false)),
            vec!["emp-aa-100", "emp-agent", "emp-david"]
        );
        assert!(is_active_holder(
            &roster,
            "emp-david",
            &["platform-admin"],
            Partition::Real
        ));
    }

    /// The executor is a REAL registered agent (a deployment fact, not a
    /// sim identity): a simulated packet never routes to it, even when
    /// the step is executable and the role matches — and neither does a
    /// shadow one (508cc38c: every door simulated closes, shadow closes).
    #[test]
    fn a_sim_packet_never_reaches_the_executor() {
        let platform = ["platform-admin"];
        for not_real in [Partition::Simulated, Partition::Shadow] {
            assert_eq!(
                executor_for(
                    not_real,
                    false,
                    Some("claude@algedonic.dev"),
                    Some("platform-admin"),
                    &platform
                ),
                None,
                "{not_real} must not reach the executor"
            );
        }
        // Real packets keep the executor path exactly as before.
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &platform
            ),
            Some("claude@algedonic.dev".to_string())
        );
    }

    /// The `run` step of a maintenance chore, materialised from the
    /// platform bundle exactly as `POST /api/jobs` does and serialised
    /// the way `STEP_CREATED` carries it — so what this reads is what
    /// `handle_event` reads. Named for the chore family on purpose: the
    /// pr-train car (af796788) carries its own helper for the train.
    fn bundled_chore_run_step(kind: &str) -> super::StepEventPayload {
        use boss_core::job::{JobId, StepId, Subject};
        let spec =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .find(|w| w.kind == kind)
                .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"));
        let subject = Subject::new("custom", "maintenance/2026-09-18");
        let run = boss_jobs::registry::materialize_steps(
            &spec,
            &subject,
            JobId::new(),
            &serde_json::Value::Object(Default::default()),
            StepId::new,
        )
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some("run"))
        .unwrap_or_else(|| panic!("{kind} has a `run` step"));
        serde_json::from_value(boss_jobs::events::step_state_payload(&run))
            .expect("a serialised Step is a StepEventPayload")
    }

    /// A chore's `run` step nominates nobody — it is born its
    /// automation's (backlog 4f909642, the pr-train's af796788 applied
    /// to the chores). Measured 2026-09-18 on every closed
    /// `maintenance-*` packet the system of record listed: each `run`
    /// arrived with `authority_role = platform-admin` and no assignee,
    /// so the executes-lane handed it to the agent alias and
    /// `automation:boss-step` completed it over the agent's head —
    /// every five minutes for the estate observer. The bundle now
    /// declares the completing actor as the step's audience, so the
    /// step is born placed and the guard at the top of `handle_event`
    /// (assignee already set) passes it over before any pick. The
    /// executes-lane itself is unchanged: handed the role the step used
    /// to carry, it still names the executor.
    #[test]
    fn a_chores_run_step_nominates_nobody() {
        for (kind, actor) in [
            ("maintenance-backup", "automation:boss-step"),
            ("maintenance-estate-observe-units", "automation:boss-step"),
            (
                "maintenance-dev-scratch-reclaim",
                "automation:dev-scratch-reclaim",
            ),
        ] {
            let run = bundled_chore_run_step(kind);
            assert_eq!(
                run.assignee_id.as_deref(),
                Some(actor),
                "{kind}: `run` is born placed with the actor that completes it, so the \
                 dispatcher never reaches a pick for it"
            );
            assert!(
                run.metadata
                    .as_ref()
                    .and_then(|m| m.get("authority_role"))
                    .is_none(),
                "{kind}: `run` carries no role for the role arm to list"
            );
        }
        // The control: the shape the chores USED to arrive in — the
        // role and no assignee — is exactly what the lane nominates.
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &["platform-admin"]
            )
            .as_deref(),
            Some("claude@algedonic.dev")
        );
    }

    /// An UNKNOWN kind counts as a decision: the registry lookup that
    /// feeds `executor_for` defaults decision_shaped=true when the kind
    /// has no StepType row (correction-verdict rides permissively), so
    /// a verdict on an unregistered kind still reaches a person.
    #[test]
    fn the_registry_flag_marks_exactly_the_verdict_kinds() {
        let reg = StepRegistry::v1();
        for k in ["sign-off", "answer-question", "review-design"] {
            assert!(
                reg.get(k).map(|t| t.decision_shaped) == Some(true),
                "{k} must be decision_shaped"
            );
        }
        for k in ["task", "checklist", "outcome", "scheduling"] {
            assert!(
                reg.get(k).map(|t| t.decision_shaped) == Some(false),
                "{k} must NOT be decision_shaped"
            );
        }
    }

    /// (b) Distribution: a spread of distinct step ids fans out across MORE
    /// THAN ONE holder of a multi-holder role (the bug was that every step
    /// landed on a single employee), and lands roughly evenly.
    #[test]
    fn pick_index_spreads_distinct_steps_across_holders() {
        // Stand in for the stably-ordered candidate list: indices 0..LEN map
        // 1:1 to the sorted role-holders pick_employee would select from.
        const LEN: usize = 5;
        const N: usize = 5000;
        let mut hits: HashMap<usize, usize> = HashMap::new();
        for n in 0..N {
            // UUID-shaped distinct step ids, like real step ids.
            let step_id = format!("00000000-0000-4000-8000-{n:012x}");
            let idx = pick_index(&step_id, LEN);
            assert!(idx < LEN, "index stays in bounds");
            *hits.entry(idx).or_default() += 1;
        }
        // Must use more than one holder (the whole point of the fix).
        assert!(
            hits.len() > 1,
            "load must fan out across >1 holder, got {} distinct",
            hits.len()
        );
        // And reach EVERY holder — with N >> LEN this is overwhelmingly true.
        assert_eq!(hits.len(), LEN, "every holder of the role should be used");
        // Roughly even: no holder should hog the lion's share. Expected per
        // holder is N/LEN = 1000; allow generous slack for hash variance.
        let expected = N / LEN;
        for (idx, &count) in &hits {
            assert!(
                count > expected / 2 && count < expected * 2,
                "holder {idx} got {count}, far from the ~{expected} even share"
            );
        }
    }

    /// The config-selected strategy gates which index `pick_employee` takes:
    /// `LowestId` always lands on candidates[0] (legacy parity), while
    /// `Spread` does NOT — over a spread of distinct step ids it reaches
    /// holders past index 0. This is the data-selectable-behavior contract.
    #[test]
    fn strategy_selects_lowest_id_vs_spread() {
        const LEN: usize = 5;
        const N: usize = 5000;

        let mut lowest_hits: HashMap<usize, usize> = HashMap::new();
        let mut spread_hits: HashMap<usize, usize> = HashMap::new();
        for n in 0..N {
            let step_id = format!("00000000-0000-4000-8000-{n:012x}");
            let lowest = pick_index_for(AssignmentStrategy::LowestId, &step_id, LEN);
            let spread = pick_index_for(AssignmentStrategy::Spread, &step_id, LEN);
            assert!(lowest < LEN && spread < LEN, "index stays in bounds");
            // LowestId is ALWAYS index 0, for every step id.
            assert_eq!(
                lowest, 0,
                "LowestId must pick candidates[0] (the lowest-id holder)"
            );
            *lowest_hits.entry(lowest).or_default() += 1;
            *spread_hits.entry(spread).or_default() += 1;
        }
        // LowestId used ONLY index 0 across the whole spread.
        assert_eq!(
            lowest_hits.keys().copied().collect::<Vec<_>>(),
            vec![0],
            "LowestId must never leave candidates[0]"
        );
        // Spread did NOT always return index 0 — it fanned out past it.
        assert!(
            spread_hits.len() > 1,
            "Spread must distribute across >1 holder, not always candidates[0]"
        );
        assert!(
            spread_hits.values().sum::<usize>() - spread_hits.get(&0).copied().unwrap_or(0) > 0,
            "Spread must place steps on holders other than candidates[0]"
        );
    }

    /// Single-holder roster: every strategy lands on the sole holder
    /// (index 0). Guards the `len == 1` boundary — the spread path's
    /// `% 1 == 0` must agree with LowestId there, and neither may panic.
    #[test]
    fn every_strategy_agrees_on_a_single_holder() {
        let step_id = "00000000-0000-4000-8000-000000000abc";
        for strategy in [AssignmentStrategy::LowestId, AssignmentStrategy::Spread] {
            assert_eq!(
                pick_index_for(strategy, step_id, 1),
                0,
                "a single-holder role must pick index 0 under {strategy:?}"
            );
        }
    }

    /// The step payloads the jobs API publishes for a freshly opened
    /// packet of `kind`, materialised from the platform bundle exactly
    /// as `POST /api/jobs` does and serialised the way `STEP_CREATED`
    /// carries them — so what this test reads is what `handle_event`
    /// reads.
    fn bundled_packet_steps(kind: &str) -> Vec<(String, StepEventPayload)> {
        use boss_core::job::{JobId, StepId, Subject};
        let spec =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .find(|w| w.kind == kind)
                .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"));
        let subject = Subject::new("custom", "train/20260918-1141");
        boss_jobs::registry::materialize_steps(
            &spec,
            &subject,
            JobId::new(),
            &serde_json::Value::Object(Default::default()),
            StepId::new,
        )
        .into_iter()
        .map(|step| {
            let slug = step.spec_slug.clone().expect("a bundled step has a slug");
            let payload = serde_json::from_value(boss_jobs::events::step_state_payload(&step))
                .expect("a serialised Step is a StepEventPayload");
            (slug, payload)
        })
        .collect()
    }

    /// The deployment's executes-lane (`infra/cluster/manifests/boss.yaml`):
    /// the agent alias, executing for `platform-admin`.
    const EXECUTOR: &str = "claude@algedonic.dev";
    const EXECUTOR_ROLES: &str = "platform-admin";

    /// A train's steps nominate nobody — they are born the conductor's
    /// (backlog af796788). Measured 2026-09-18 on three consecutive
    /// pr-trains: every task step arrived with `authority_role =
    /// platform-admin` and no assignee, so the executes-lane below
    /// handed all seven to the agent alias and the conductor completed
    /// them over its head. The bundle now declares the conductor as
    /// each step's audience, so the step is born placed and the
    /// dispatcher's first guard passes it over — while a genuine
    /// agent-executable step (a backlog item's `build`) still reaches
    /// the executor through the same lane.
    #[test]
    fn the_conductors_train_steps_nominate_nobody() {
        let train = bundled_packet_steps("pr-train");
        let conductors = [
            "collect",
            "assemble",
            "pr",
            "ci",
            "merged",
            "deployed",
            "converged",
        ];
        for slug in conductors {
            let (_, step) = train
                .iter()
                .find(|(s, _)| s == slug)
                .unwrap_or_else(|| panic!("the train has a `{slug}` step"));
            assert!(
                born_placed(step),
                "`{slug}` is born placed, so the dispatcher never reaches a pick for it"
            );
            assert_eq!(
                step.assignee_id.as_deref(),
                Some("automation:train-conductor"),
                "`{slug}` is the conductor's, not the executor's"
            );
        }

        // The control: a step the agent genuinely executes is still
        // unplaced at birth, carries its role, and the lane names the
        // executor for it — the fix narrowed nothing but the train.
        let backlog = bundled_packet_steps("backlog-item");
        let (_, build) = backlog
            .iter()
            .find(|(s, _)| s == "build")
            .expect("a backlog item has a `build` step");
        assert!(!born_placed(build), "a build step waits for a nomination");
        let role = build
            .metadata
            .as_ref()
            .and_then(|m| m.get("authority_role"))
            .and_then(|v| v.as_str())
            .expect("a build step carries its role");
        assert_eq!(
            executor_for(
                Partition::Real,
                false,
                Some(EXECUTOR),
                Some(EXECUTOR_ROLES),
                &[role]
            )
            .as_deref(),
            Some(EXECUTOR)
        );
    }
}
