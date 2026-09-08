//! Core dispatcher logic. Consumes step events, picks role-matched
//! Employees, PUTs assignments.

use crate::config::AssignmentStrategy;
use anyhow::{Context, Result};
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
                // Inherit the triggering event's sim-ness, same as the
                // rules dispatcher. The assignment path is a SEPARATE
                // consumer, and it was the last hop losing the marker:
                // its writes landed `_simulated: false` on simulated
                // Jobs because nothing here ever set the task-local.
                // The SAME bit also gates who may be assigned (see
                // `partition_permits`), so it is threaded into
                // handle_event explicitly rather than re-read there.
                let simulated = event_is_simulated(&inner);
                let outcome = boss_core::sim_origin::with_sim_chain(
                    simulated,
                    handle_event(&ctx, &subject, &inner, simulated),
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

/// The assignment-side sim boundary, one checkable question — the exact
/// mirror of the workforce's `row_is_simulated` (boss-sim/workforce.rs):
/// `true` only when the event SAYS `_simulated: true`; absent, null,
/// false, or a mis-typed value all read as REAL.
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
fn event_is_simulated(payload: &Value) -> bool {
    payload.get("_simulated").and_then(Value::as_bool) == Some(true)
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
/// role? The one rule of the 9c23395c fix: a SIMULATED packet must never
/// be assigned to an operator identity — the five `[sim]
/// decision-routing probe` packets landed in David's real queue through
/// the owner-preference path because no assignment route checked the
/// partition. Real packets are untouched in every direction.
fn partition_permits(simulated: bool, employee_role: &str) -> bool {
    !simulated || employee_role != OPERATOR_ROLE
}

async fn handle_event(
    ctx: &DispatcherCtx,
    subject: &str,
    payload: &Value,
    simulated: bool,
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
    if step
        .assignee_id
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
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
    if step
        .metadata
        .as_ref()
        .and_then(|m| m.get("claimable"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        debug!(
            job_id,
            step_id, "step is claimable — leaving it for its role queue"
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
    if let Some(executor) = executor_for(
        simulated,
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
            Some(o) => owner_is_active_holder(ctx, o, &role_candidates, simulated)
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
    let chosen =
        pick_employee_with_role_fallback(ctx, &role_candidates, step_id, simulated).await?;
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
        // A SIMULATED step whose only role holders are operator identities
        // is unfillable BY DESIGN (`partition_permits`): it dead-letters
        // loudly instead of polluting a real queue, and the exercise that
        // filed it learns immediately.
        // The old `Ok(())` here dropped the step on the floor: it was never
        // assigned, so its Job never closed, silently losing work (a
        // conservation violation). This was the brewery day-1 ap-payment-run
        // hang — the first AP run opened before the roster was queryable.
        anyhow::bail!(
            "no eligible employee for step {step_id} (job {job_id}); \
             candidates={role_candidates:?} simulated={simulated} — \
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
/// Never fires for a simulated step (the executor is a REAL registered
/// agent — a deployment fact, not a sim identity — and the 9c23395c rule
/// is that sim packets never enter a real actor's queue), never fires
/// for a decision-shaped step, never fires without a configured
/// executor, and only fires when one of the step's candidate roles is a
/// role the executor is declared to execute for — a brewery `brewer`
/// step must not land on the platform agent just because the agent
/// exists.
fn executor_for(
    simulated: bool,
    decision_shaped: bool,
    executor_id: Option<&str>,
    executor_roles: Option<&str>,
    role_candidates: &[&str],
) -> Option<String> {
    if simulated || decision_shaped {
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
    simulated: bool,
) -> Result<Option<(String, String)>> {
    if role_candidates.is_empty() {
        let chosen = pick_employee(ctx, None, step_id, simulated).await?;
        return Ok(chosen.map(|id| (id, String::new())));
    }
    for r in role_candidates {
        if let Some(id) = pick_employee(ctx, Some(r), step_id, simulated).await? {
            return Ok(Some((id, (*r).to_string())));
        }
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct Employee {
    id: String,
    role: String,
    status: String,
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

/// The active roster, fetched from the people API. The one HTTP call
/// every roster reader shares; the TTL cache in `ctx.roster` is what
/// keeps it off the hot path.
async fn fetch_active_roster(ctx: &DispatcherCtx) -> Result<Vec<Employee>> {
    let url = format!("{}/api/people", ctx.people_api_url.trim_end_matches('/'));
    let resp = ctx
        .client
        .get(&url)
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    resp.json().await.context("decode /api/people response")
}

/// Is `owner_id` an active holder of any of `roles`? Reads the SAME
/// TTL-cached roster `pick_employee` uses (same `roster_is_stale` +
/// `fetch_active_roster`), so the owner check and the role pick can
/// never see a different roster.
async fn owner_is_active_holder(
    ctx: &DispatcherCtx,
    owner_id: &str,
    roles: &[&str],
    simulated: bool,
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
        simulated,
    ))
}

/// The membership question, pure: is `owner_id` an ACTIVE employee whose
/// role is one of `roles`, reachable from this partition? An inactive
/// holder, a wrong role, a different id, or an operator identity on a
/// SIMULATED packet all read false — the same eligibility
/// `pick_employee`'s candidate filter uses, so the owner is preferred
/// only when they could have been picked anyway. (The partition leg is
/// the 9c23395c fix: the five `[sim]` probes reached emp-david through
/// exactly this owner-preference route.)
fn is_active_holder(
    employees: &[Employee],
    owner_id: &str,
    roles: &[&str],
    simulated: bool,
) -> bool {
    employees.iter().any(|e| {
        e.status == "active"
            && e.id == owner_id
            && roles.contains(&e.role.as_str())
            && partition_permits(simulated, &e.role)
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
    simulated: bool,
) -> Result<Option<String>> {
    let mut cache = ctx.roster.lock().await;
    if roster_is_stale(&cache) {
        cache.employees = fetch_active_roster(ctx).await?;
        cache.fetched_at = Some(std::time::Instant::now());
    }
    let candidates = eligible_candidates(&cache.employees, role, simulated);
    if candidates.is_empty() {
        // No eligible holder — preserve the None contract; never `% 0`.
        return Ok(None);
    }
    let idx = pick_index_for(ctx.strategy, step_id, candidates.len());
    Ok(candidates.get(idx).map(|e| e.id.clone()))
}

/// The candidate pool, pure: active, role-matched (when a role
/// constrains the step), reachable from the packet's partition
/// (`partition_permits` — a SIMULATED packet's pool never contains an
/// operator identity), in stable id order so the strategy index above is
/// reproducible. Factored out of `pick_employee` so the eligibility
/// rule — the surface the 9c23395c defect lived on — is testable
/// without the roster cache / HTTP.
fn eligible_candidates<'a>(
    employees: &'a [Employee],
    role: Option<&str>,
    simulated: bool,
) -> Vec<&'a Employee> {
    let mut candidates: Vec<&Employee> = employees
        .iter()
        .filter(|e| e.status == "active")
        .filter(|e| role.map(|r| e.role == r).unwrap_or(true))
        .filter(|e| partition_permits(simulated, &e.role))
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
        eligible_candidates, event_is_simulated, executor_for, is_active_holder, owner_assignee,
        owner_id_from_job_body, partition_permits, pick_index, pick_index_for, stable_hash,
    };
    use crate::config::AssignmentStrategy;
    use boss_jobs::step_registry::StepRegistry;
    use std::collections::HashMap;

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
                false,
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
                false,
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
                false,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &brewer
            ),
            None
        );
        // Unconfigured deployments behave exactly as before.
        assert_eq!(
            executor_for(false, false, None, Some("platform-admin"), &platform),
            None
        );
        assert_eq!(
            executor_for(false, false, Some(""), Some("platform-admin"), &platform),
            None
        );
        assert_eq!(executor_for(false, false, Some("x"), None, &platform), None);
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
        };
        let roster = vec![
            emp("emp-david", "platform-admin", "active"),
            emp("emp-gone", "platform-admin", "inactive"),
            emp("emp-brewer", "brewer", "active"),
        ];
        let admin = ["platform-admin"];
        assert!(is_active_holder(&roster, "emp-david", &admin, false));
        assert!(!is_active_holder(&roster, "emp-gone", &admin, false)); // inactive
        assert!(!is_active_holder(&roster, "emp-brewer", &admin, false)); // wrong role
        assert!(!is_active_holder(&roster, "emp-ghost", &admin, false)); // not on roster
        assert!(!is_active_holder(&roster, "emp-david", &[], false)); // no candidate roles
    }

    /// The assignment-side sim boundary predicate mirrors the workforce's
    /// `row_is_simulated` EXACTLY: only a literal `_simulated: true` reads
    /// as simulated; absent, null, false, or a mis-typed value all read
    /// as REAL. See `event_is_simulated` for why the mirror direction is
    /// load-bearing (the two halves must agree on which side an ambiguous
    /// packet falls, or it becomes workable by nobody).
    #[test]
    fn the_assignment_boundary_fails_closed_on_shape() {
        use serde_json::json;
        assert!(event_is_simulated(&json!({"_simulated": true})));
        assert!(!event_is_simulated(&json!({"_simulated": false})));
        assert!(!event_is_simulated(&json!({})), "absent means real");
        assert!(!event_is_simulated(&json!({"_simulated": null})));
        assert!(
            !event_is_simulated(&json!({"_simulated": "true"})),
            "a string is not a claim - fail closed on shape too"
        );
    }

    /// The partition rule itself: only the (simulated, operator-role)
    /// pair is refused. Real packets reach operators; sim packets reach
    /// every non-operator role.
    #[test]
    fn the_partition_refuses_exactly_sim_cross_operator() {
        assert!(!partition_permits(true, "platform-admin"));
        assert!(partition_permits(false, "platform-admin"));
        assert!(partition_permits(true, "brewer"));
        assert!(partition_permits(false, "brewer"));
    }

    fn partition_roster() -> Vec<super::Employee> {
        let emp = |id: &str, role: &str, status: &str| super::Employee {
            id: id.into(),
            role: role.into(),
            status: status.into(),
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
            eligible_candidates(&roster, Some("platform-admin"), true).is_empty(),
            "sim packets must never route to operator identities"
        );
        // Unconstrained sim steps still reach the sim-workable roster.
        assert_eq!(
            ids(eligible_candidates(&roster, None, true)),
            vec!["emp-aa-100"]
        );
        // Non-operator roles are untouched by the partition.
        assert_eq!(
            ids(eligible_candidates(&roster, Some("brewer"), true)),
            vec!["emp-aa-100"]
        );
        // And a sim OWNER who is an operator is no longer an active
        // holder for a sim packet — the owner-preference path (the route
        // the five [sim] probes actually took) closes with the same rule.
        assert!(!is_active_holder(
            &roster,
            "emp-david",
            &["platform-admin"],
            true
        ));
    }

    /// REAL packets are byte-for-byte unaffected: same candidates, same
    /// stable ordering, operators fully reachable.
    #[test]
    fn a_real_packet_reaches_operators_exactly_as_before() {
        let roster = partition_roster();
        let ids = |v: Vec<&super::Employee>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(eligible_candidates(&roster, Some("platform-admin"), false)),
            vec!["emp-agent", "emp-david"]
        );
        assert_eq!(
            ids(eligible_candidates(&roster, None, false)),
            vec!["emp-aa-100", "emp-agent", "emp-david"]
        );
        assert!(is_active_holder(
            &roster,
            "emp-david",
            &["platform-admin"],
            false
        ));
    }

    /// The executor is a REAL registered agent (a deployment fact, not a
    /// sim identity): a simulated packet never routes to it, even when
    /// the step is executable and the role matches.
    #[test]
    fn a_sim_packet_never_reaches_the_executor() {
        let platform = ["platform-admin"];
        assert_eq!(
            executor_for(
                true,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &platform
            ),
            None
        );
        // Real packets keep the executor path exactly as before.
        assert_eq!(
            executor_for(
                false,
                false,
                Some("claude@algedonic.dev"),
                Some("platform-admin"),
                &platform
            ),
            Some("claude@algedonic.dev".to_string())
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
}
