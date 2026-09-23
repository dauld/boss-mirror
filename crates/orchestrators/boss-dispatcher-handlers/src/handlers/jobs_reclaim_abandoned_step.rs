//! `jobs.reclaim_abandoned_step` — a clock rule RELEASES a step whose
//! named executor run is dead, a bound after the death was recorded.
//!
//! The gap this closes (backlog a3397b01, 2026-09-19): when a session
//! died at ~15:39Z it orphaned fifteen `agent-run` packets, and TWELVE
//! of them held a step that was claimed, built, gate green and never
//! handed back. The durable inbox (923b6571) gave QUEUED work a door
//! out — `boss dispatch --next` reads a station and takes the first
//! WAITING step — and an active step is not waiting, it is somebody's.
//! So the twelve were invisible to every queue read, and recovery was
//! by hand.
//!
//! ## Two facts, two bounds — and why that is the safety argument
//!
//! `agent-run-dies-when-building-is-silent` completes the run's
//! `building` step with `result = died` after four hours of packet
//! silence, and says in its own header that it leaves the executing
//! step "exactly as the builder left it — claimed, active — because
//! releasing it is a routing decision this rule must not make." This
//! handler makes that decision, and deliberately does NOT make it on
//! the same tick:
//!
//! - **The run is dead** is written onto the run's packet at the death
//!   rule's bound, with the silence measured, the bound and the tick.
//! - **Its work is free** is written onto the claimed step here, an
//!   `after_hours` bound LATER, naming the run it was taken from and
//!   the instant that run was declared dead.
//!
//! A reclaim is the one move in this loop that can put two executors on
//! one step, so the packet's constraint was that it be observable in
//! the record BEFORE it acts. Two separated facts are how: for the
//! whole of `after_hours` the record says a run is dead and its step is
//! still held, which is a state an operator can read and act on before
//! anything moves. Collapsing them into one reaction would act on a
//! fact nobody could have read first.
//!
//! ## How a slow-but-alive executor is protected
//!
//! 1. **The step names its run.** `boss dispatch` writes
//!    `agent_run = <run id>` onto the step it claims (backlog dd6d44b7),
//!    so the handler asks whether THAT run died rather than inferring
//!    abandonment from the step's own stillness. A re-dispatched step
//!    carries the NEW run's id and is never freed on the old one's
//!    death.
//! 2. **A step with no edge is reported and left alone.** Nothing names
//!    its executor, so nothing here can tell abandoned from busy — and
//!    guessing is the one move a reclaim may not make. Steps claimed
//!    before the edge existed fall here, permanently; they are a hand's
//!    work, and the warn names each one. The population warned about is
//!    the one `boss dispatch --next` selects from — a step carrying the
//!    agent block's model key — so a PERSON's claimed task is never in
//!    this rule's sight, and the warn stays a thing somebody reads.
//! 3. **The run's own silence.** Death is measured on the run packet
//!    off movement the executor produces; an executor that is slow but
//!    alive keeps its run moving and it never dies.
//! 4. **A run that moved after it died is not free.** Anything that
//!    touched the run packet after the death stamp — a late handback, a
//!    hand — holds the step. The death is the last word only while it
//!    stays the last word.
//!
//! It is a BOUND, not a mutex, and the rule file says so out loud. The
//! lock shape is a lease the executor renews; nothing in the tree
//! renews anything today, so this is the cheaper half that reuses a
//! live mechanism.
//!
//! ## Which way it scans, and why
//!
//! From the OPEN BOARD, not from the run registry. The question is
//! "which claimed steps are held by a dead run", and every such step is
//! on an open packet — a packet with an active step cannot be closed.
//! The open board is bounded by operational reality (176 packets on
//! 2026-09-19); closed `agent-run` packets grow forever, so a scan of
//! them would be a cost that rises with the deployment's age for an
//! answer that does not.
//!
//! ## Idempotence
//!
//! A released step is `ready`, not `active`, so the next tick does not
//! see it; and the release CLEARS the `agent_run` edge, so even a step
//! re-claimed and re-abandoned is judged against its own run. Nothing
//! is ever written twice.

use super::common::{api_client, get_json, open_jobs, row_or_refuse, write_json};
use super::jobs_age_out_step::last_moved;
use super::jobs_complete_linked_step::{step_by_slug, unusable_link};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;

/// Default metadata key the release's own evidence lands under on the
/// freed step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "reclaimed";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick (`schedule_runner::tick`) — the same clock the
/// death rule reads, and for the same reason: a handler that held one
/// of its own could judge hours a daily cadence never gave it.
const TICK_AT: &str = "_at";

/// The status a claim moves a step FROM (`Ready` → `Active`, the CAS in
/// `claim_step`), which is therefore the status a release moves it back
/// to. The pair is what makes the reclaim a restoration rather than a
/// new routing opinion.
const CLAIMED: &str = "active";
const WAITING: &str = "ready";

pub struct JobsReclaimAbandonedStep {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsReclaimAbandonedStep {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local stand-in for jobs-api).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// What the run packet says about its own ending, as this handler needs
/// to read it.
#[derive(Debug, PartialEq)]
pub(crate) enum RunVerdict {
    /// The run's `run_step` is completed carrying `dead_result`, and
    /// nothing has touched the packet since — `at` is when that was
    /// recorded, server-stamped.
    Dead { at: DateTime<Utc> },
    /// The run is not (or not yet, or no longer) evidence that the step
    /// is free, with the reason for the journal.
    Holds { why: String },
}

/// Read the run packet's verdict on itself.
///
/// The death instant is the step's own `completed_at` — the server
/// stamps it at the flip and never takes it from a body — with the
/// death rule's `aged_out.at` (the tick that judged the silence) as the
/// fallback for a row that predates the column. A death with NEITHER
/// stamp is `Holds`: "I cannot tell when this died" is a finding, not a
/// licence to start the clock now, which would free the step an hour
/// later on an age nobody measured.
pub(crate) fn run_verdict(
    run: &serde_json::Value,
    run_step: &str,
    dead_result: &str,
    evidence_key: &str,
) -> RunVerdict {
    let holds = |why: String| RunVerdict::Holds { why };
    let Some(step) = step_by_slug(run, run_step) else {
        return holds(format!("the run carries no `{run_step}` step"));
    };
    if step.get("status").and_then(|v| v.as_str()) != Some("completed") {
        return holds(format!("`{run_step}` is still open — the run is running"));
    }
    let result = step
        .pointer("/metadata/result")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if result != dead_result {
        return holds(format!(
            "`{run_step}` ended `{result}`, not `{dead_result}`"
        ));
    }
    let parse = |v: Option<&serde_json::Value>| {
        v.and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let Some(at) = parse(step.get("completed_at"))
        .or_else(|| parse(step.pointer(&format!("/metadata/{evidence_key}/at"))))
        .or_else(|| parse(step.pointer("/metadata/aged_out/at")))
    else {
        return holds(format!(
            "`{run_step}` says `{dead_result}` but carries no instant — when it died \
             cannot be read, so when its work comes free cannot be either"
        ));
    };
    // GUARD 4. The death is the last word only while it stays the last
    // word: a packet that moved after it — a late handback, a hand
    // correcting the record — is not a packet nobody is attending.
    if let Some(moved) = last_moved(run, None)
        && moved > at
    {
        return holds(format!(
            "the run moved at {} , after it died at {} — somebody is attending it",
            moved.to_rfc3339(),
            at.to_rfc3339()
        ));
    }
    RunVerdict::Dead { at }
}

/// The `after_hours` arg as a non-negative bound. Unlike the death
/// rule's `hours`, zero is legal and means "release as soon as the
/// death is on the record" — an authoring choice this handler will
/// honour rather than refuse, because the separation of the two facts
/// is the RULE's argument to make, not this shape's. Negative or
/// unparseable is permanent: no redelivery fixes a typo.
fn after_hours(args: &[(String, Value)]) -> Result<f64, HandlerError> {
    let raw = arg_string(args, "after_hours")?;
    match raw.trim().parse::<f64>() {
        Ok(h) if h.is_finite() && h >= 0.0 => Ok(h),
        _ => Err(HandlerError::Permanent(format!(
            "after_hours must be a non-negative number of hours, got {raw:?}"
        ))),
    }
}

/// The metadata merge that takes the dead run's edge OFF a claimed
/// step and records why, in one server-side write.
///
/// The edge is removed rather than left standing because a stale one is
/// actively wrong downstream: `agent-run-delivers-when-its-step-is-done`
/// follows it, and a step completed later would try to deliver onto a
/// run that is closed and dead. `boss dispatch` writes the new run's id
/// at the next claim, so nothing needs it in the meantime.
///
/// IT IS AN EXPLICIT `null` THROUGH THE MERGE DOOR, and that is the
/// whole of backlog ce3a4b16. Until 2026-09-22 this was one PUT whose
/// `metadata` simply left the key out — which reads as a clear and is
/// not one: since b91a2103 the step PUT CARRIES `agent_run` forward
/// whenever a body omits it (`crates/core/boss-jobs/src/http/steps.rs`,
/// pinned by `the_run_edge_survives_a_metadata_put.rs`), because
/// omission is how every other completer says "leave the edge alone".
/// So the release believed it cleared the edge and did not, and the
/// reclaimed step went back to `ready` still naming the run that
/// abandoned it. `PATCH .../steps/{id}/metadata` is the only door that
/// DELETES a key, and only for a key given as `null`.
///
/// The evidence rides the same merge, so the clear is never recorded
/// without its reason — and, because the merge happens inside one
/// adapter transaction, a concurrent writer's other keys survive it,
/// which the old read-spread-PUT could not promise.
pub(crate) fn release_patch(
    link: &str,
    evidence_key: &str,
    evidence: serde_json::Value,
) -> serde_json::Value {
    json!({
        link: serde_json::Value::Null,
        evidence_key: evidence,
    })
}

/// The body that puts the claim back where it came from: `ready`,
/// nobody's.
///
/// It carries NO `metadata` at all. The PUT is an overlay — a field
/// absent from the body is left as stored — so the merge above is the
/// step's metadata by the time this lands, and a `metadata` key here
/// would replace it wholesale with a client-side copy.
pub(crate) fn release_put() -> serde_json::Value {
    json!({
        "status": WAITING,
        "assignee_id": serde_json::Value::Null,
    })
}

#[async_trait]
impl Handler for JobsReclaimAbandonedStep {
    fn name(&self) -> &'static str {
        "jobs.reclaim_abandoned_step"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let run_kind = arg_string(args, "run_kind")?;
        let run_step = arg_string(args, "run_step")?;
        let dead_result = arg_string(args, "dead_result")?;
        let link = arg_string(args, "link")?;
        let bound = after_hours(args)?;
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };

        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — jobs.reclaim_abandoned_step judges \
                     hours and needs a sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        // The open board, every kind: a claimed step lives on an open
        // packet whatever the packet is for, and the run kind is what
        // narrows the FOLLOW, not the scan.
        let board = open_jobs(&self.client, self.base(), None, &ctx.rule_name).await?;
        for job in &board {
            let job_id = job.get("id").and_then(|v| v.as_str()).unwrap_or("");
            // The population is the one `waiting_step` selects from
            // one door over (`boss dispatch --next`), with the status
            // flipped: a step carrying the agent block's model key,
            // CLAIMED rather than waiting. Selecting on the same fact
            // the inbox does keeps a person's claimed task out of this
            // rule's sight entirely — a human claim is not abandoned
            // work, and warning about one every hour would be a check
            // nobody reads.
            let claimed: Vec<&serde_json::Value> = job
                .get("steps")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
                .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some(CLAIMED))
                .filter(|s| {
                    s.pointer(&format!("/metadata/{}", boss_jobs::agent_spec::MODEL_KEY))
                        .is_some()
                })
                .collect();
            for step in claimed {
                let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                // GUARD 1 AND 2. The edge, or nothing: a claimed step
                // that does not name its run is one this rule cannot
                // judge, and it says so rather than falling back on the
                // step's own stillness — which is exactly the inference
                // that would race a live executor.
                let Some(run_id) = step
                    .pointer(&format!("/metadata/{link}"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    tracing::warn!(
                        rule = %ctx.rule_name,
                        packet = %job_id,
                        step = %step_id,
                        "agent-workable and claimed, with no `{link}` edge — nothing \
                         names its executor, so nothing here can tell abandoned from \
                         busy; left alone (claimed before dd6d44b7? a hand's work)"
                    );
                    continue;
                };
                if let Some(why) = unusable_link(run_id) {
                    tracing::warn!(
                        rule = %ctx.rule_name,
                        packet = %job_id,
                        step = %step_id,
                        "`{link}` on this claimed step: {why}"
                    );
                    continue;
                }
                let run = get_json(
                    &self.client,
                    &format!("{}/api/jobs/{run_id}", self.base()),
                    &ctx.rule_name,
                )
                .await?;
                // A body with no row is a bad answer, not "a run of
                // another kind" to pass over (backlog f2eac973).
                let run = row_or_refuse(run, &format!("GET /api/jobs/{run_id}"))
                    .map_err(HandlerError::Downstream)?;
                let run = &run;
                // A run of another kind under this key is not this
                // rule's business (the gate stamps `agent_run` on a
                // gate-run too, under the same spelling).
                if run.get("kind").and_then(|v| v.as_str()) != Some(run_kind) {
                    continue;
                }
                let died_at = match run_verdict(run, run_step, dead_result, evidence_key) {
                    RunVerdict::Dead { at } => at,
                    RunVerdict::Holds { why } => {
                        tracing::debug!(
                            rule = %ctx.rule_name,
                            packet = %job_id,
                            step = %step_id,
                            run = %run_id,
                            "held: {why}"
                        );
                        continue;
                    }
                };
                // GUARD 3. The second bound, measured from the death —
                // the window in which the record says DEAD and the step
                // is still held, so the release is readable before it
                // happens.
                let since_death = (now - died_at).num_seconds() as f64 / 3600.0;
                if since_death < bound {
                    continue;
                }
                let step_url = format!("{}/api/jobs/{job_id}/steps/{step_id}", self.base());
                // THE EDGE COMES OFF FIRST, while the step is still
                // `active` (ce3a4b16). For the window between these two
                // writes nothing can claim the step, so nothing can
                // observe it free and still named. The other order
                // would publish a `ready` step naming a dead run, and a
                // claim landing in that window would have ITS fresh
                // edge nulled by the write that followed.
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{step_url}/metadata"),
                    &release_patch(
                        link,
                        evidence_key,
                        json!({
                            "from_run": run_id,
                            "from_actor": run.pointer("/metadata/agent"),
                            "died_at": died_at.to_rfc3339(),
                            "since_death_hours": (since_death * 100.0).round() / 100.0,
                            "after_hours": bound,
                            "at": now.to_rfc3339(),
                            "rule": ctx.rule_name,
                        }),
                    ),
                    &ctx.rule_name,
                )
                .await?;
                write_json(
                    &self.client,
                    reqwest::Method::PUT,
                    &step_url,
                    &release_put(),
                    &ctx.rule_name,
                )
                .await?;
                tracing::info!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    step = %step_id,
                    run = %run_id,
                    "run died {since_death:.1}h ago, past the {bound}h bound — step released \
                     to `{WAITING}`, unassigned"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const PACKET: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER: &str = "22222222-2222-2222-2222-222222222222";
    const DEAD_RUN: &str = "aaaaaaaa-1111-4111-8111-111111111111";
    const LIVE_RUN: &str = "bbbbbbbb-2222-4222-8222-222222222222";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "an-abandoned-step-is-reclaimed-when-its-run-died".into(),
            triggering_event_id: "tick-1".into(),
            triggering_topic: "schedule".into(),
            event_payload: payload,
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("run_kind".to_string(), Value::String("agent-run".into())),
            ("run_step".to_string(), Value::String("building".into())),
            ("dead_result".to_string(), Value::String("died".into())),
            ("link".to_string(), Value::String("agent_run".into())),
            ("after_hours".to_string(), Value::String("2".into())),
        ]
    }

    /// The tick the schedule runner emits for a sub-day cadence.
    fn tick(at: &str) -> serde_json::Value {
        json!({ "_day": &at[..10], "_at": at })
    }

    /// A packet whose `build` step is CLAIMED, carrying the run edge
    /// `boss dispatch` writes at the claim (dd6d44b7).
    fn packet(id: &str, edge: Option<&str>) -> serde_json::Value {
        let mut metadata = json!({ "authority_role": "platform-admin", "agent_model": "opus" });
        if let Some(run) = edge {
            metadata["agent_run"] = json!(run);
        }
        json!({
            "id": id,
            "kind": "backlog-item",
            "status": "open",
            "metadata": { "area": "agent-runs" },
            "steps": [
                { "id": format!("{id}-triage"), "spec_slug": "triage", "status": "completed",
                  "completed_at": "2026-09-19T10:00:00Z", "metadata": {} },
                { "id": format!("{id}-build"), "spec_slug": "build", "status": "active",
                  "assignee_id": "claude@algedonic.dev", "metadata": metadata },
            ],
        })
    }

    /// An `agent-run` whose `building` ended `result` at `at`.
    fn run(id: &str, result: &str, at: Option<&str>) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "agent-run",
            "status": "closed",
            "metadata": { "packet": PACKET, "step": "build", "agent": "claude@algedonic.dev" },
            "steps": [
                { "id": format!("{id}-briefed"), "spec_slug": "briefed", "status": "completed",
                  "completed_at": "2026-09-19T10:00:00Z", "metadata": {} },
                { "id": format!("{id}-building"), "spec_slug": "building", "status": "completed",
                  "completed_at": at,
                  "metadata": { "result": result, "aged_out": { "at": at, "bound_hours": 4.0 } } },
            ],
        })
    }

    /// An `agent-run` still building — a slow executor.
    fn running(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "agent-run",
            "status": "open",
            "metadata": { "packet": OTHER, "step": "build", "agent": "claude@algedonic.dev" },
            "steps": [
                { "id": format!("{id}-briefed"), "spec_slug": "briefed", "status": "completed",
                  "completed_at": "2026-09-19T14:00:00Z", "metadata": {} },
                { "id": format!("{id}-building"), "spec_slug": "building", "status": "ready",
                  "metadata": {} },
            ],
        })
    }

    /// Every write the handler makes, as (method, job id, step id, body).
    /// The METHOD is recorded because which door a release uses is the
    /// whole subject of ce3a4b16: the same body through a PUT and
    /// through the merge door do not have the same effect.
    type Writes = Arc<Mutex<Vec<(String, String, String, serde_json::Value)>>>;

    /// A jobs API that answers the way the real one does on the two
    /// doors this handler writes through — because the bug this test
    /// module missed for a release was in the SERVER'S reading of the
    /// body, not in the body (backlog ce3a4b16). Modelled here:
    ///
    /// - **PUT `/steps/{id}` is an OVERLAY**, not a replacement of the
    ///   row: a key absent from the body leaves the stored field alone
    ///   (`crates/core/boss-jobs/src/http/steps.rs`).
    /// - **AND IT CARRIES `agent_run` FORWARD** whenever the body's
    ///   `metadata` omits it (b91a2103, pinned by
    ///   `crates/core/boss-jobs/tests/the_run_edge_survives_a_metadata_put.rs`).
    ///   Omission is how a completer says "leave the edge alone", so a
    ///   PUT can never DELETE it.
    /// - **PATCH `/steps/{id}/metadata` merges top-level keys, and a
    ///   `null` REMOVES one** — the only door that clears the edge.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let list_jobs = by_id.clone();
        let one_job = by_id.clone();
        let put_jobs = by_id.clone();
        let patch_jobs = by_id.clone();
        let put_log = writes.clone();
        let patch_log = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = list_jobs.clone();
                    async move {
                        let rows: Vec<serde_json::Value> = by_id
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = one_job.clone();
                    async move {
                        let row = by_id.lock().unwrap().get(&id).cloned();
                        match row {
                            Some(j) => Json(j).into_response(),
                            None => {
                                (axum::http::StatusCode::NOT_FOUND, "no such job").into_response()
                            }
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let writes = put_log.clone();
                        let by_id = put_jobs.clone();
                        async move {
                            writes.lock().unwrap().push((
                                "PUT".to_string(),
                                id.clone(),
                                step_id.clone(),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] != json!(step_id) {
                                        continue;
                                    }
                                    for field in ["status", "assignee_id"] {
                                        if let Some(v) = body.get(field) {
                                            step[field] = v.clone();
                                        }
                                    }
                                    let Some(sent) = body.get("metadata") else {
                                        continue;
                                    };
                                    let carried = step
                                        .pointer("/metadata/agent_run")
                                        .filter(|_| sent.get("agent_run").is_none())
                                        .cloned();
                                    step["metadata"] = sent.clone();
                                    if let Some(run) = carried
                                        && let Some(obj) = step["metadata"].as_object_mut()
                                    {
                                        obj.insert("agent_run".into(), run);
                                    }
                                }
                            }
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let writes = patch_log.clone();
                        let by_id = patch_jobs.clone();
                        async move {
                            writes.lock().unwrap().push((
                                "PATCH".to_string(),
                                id.clone(),
                                step_id.clone(),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] != json!(step_id) {
                                        continue;
                                    }
                                    if !step["metadata"].is_object() {
                                        step["metadata"] = json!({});
                                    }
                                    let Some(obj) = step["metadata"].as_object_mut() else {
                                        continue;
                                    };
                                    for (k, v) in body.as_object().into_iter().flatten() {
                                        if v.is_null() {
                                            obj.remove(k);
                                        } else {
                                            obj.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    use axum::response::IntoResponse;

    /// THE OBLIGATION. A step whose named run died three hours ago is
    /// released — `ready`, nobody's, the dead edge gone, the evidence
    /// naming the run it was taken from — and the same tick leaves
    /// alone a step whose run is still building. A second tick writes
    /// nothing more.
    #[tokio::test]
    async fn a_step_held_by_a_dead_run_is_released_and_a_live_ones_is_not() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, Some(DEAD_RUN)),
            packet(OTHER, Some(LIVE_RUN)),
            run(DEAD_RUN, "died", Some("2026-09-19T12:00:00Z")),
            running(LIVE_RUN),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");

        let written = puts.lock().unwrap().clone();
        assert_eq!(
            written.len(),
            2,
            "one step released, through the two doors a release needs: {written:?}"
        );
        for (_, job, step, _) in &written {
            assert_eq!(job, PACKET);
            assert_eq!(step, &format!("{PACKET}-build"));
        }

        // THE STEP AS THE SERVER NOW HOLDS IT, not the bodies that were
        // sent (ce3a4b16). Every body this handler sent read correctly
        // while the edge stayed standing, because a PUT that OMITS
        // `agent_run` carries it forward — so the request is not the
        // fact, and this test asks the row.
        let step = stored_step(&base, PACKET, &format!("{PACKET}-build")).await;
        assert_eq!(step["status"], "ready");
        assert_eq!(step["assignee_id"], serde_json::Value::Null);
        assert!(
            step["metadata"].get("agent_run").is_none(),
            "the dead run's edge is gone from the STORED step, or the \
             delivery rule would later deliver onto a closed run: {step}"
        );
        assert_eq!(
            step["metadata"]["agent_model"], "opus",
            "what made the step claimable is its record and is kept"
        );
        assert_eq!(step["metadata"]["reclaimed"]["from_run"], DEAD_RUN);
        assert_eq!(
            step["metadata"]["reclaimed"]["from_actor"],
            "claude@algedonic.dev"
        );
        assert_eq!(
            step["metadata"]["reclaimed"]["died_at"],
            "2026-09-19T12:00:00+00:00"
        );
        assert_eq!(step["metadata"]["reclaimed"]["since_death_hours"], 3.0);
        assert_eq!(step["metadata"]["reclaimed"]["after_hours"], 2.0);
        assert_eq!(
            step["metadata"]["reclaimed"]["rule"],
            "an-abandoned-step-is-reclaimed-when-its-run-died"
        );

        h.invoke(&args(), &ctx(tick("2026-09-19T16:00:00Z")))
            .await
            .expect("the next tick runs");
        assert_eq!(
            puts.lock().unwrap().len(),
            2,
            "a released step is `ready`, not `active` — nothing is written twice"
        );
    }

    /// THE DOOR IS THE FIX (backlog ce3a4b16). The release used to
    /// build one PUT whose `metadata` simply left `agent_run` out, and
    /// every assertion about that body passed — but omission is how a
    /// caller says LEAVE IT ALONE, and since b91a2103 the server reads
    /// it that way and carries the edge forward. So the reclaimed step
    /// went back to `ready` still naming the run that abandoned it, and
    /// `agent-run-delivers-when-its-step-is-done` would follow that
    /// edge onto a closed, dead run.
    ///
    /// The clear is therefore an explicit `null` through the merge
    /// door, which is the only write that DELETES a key — and it goes
    /// FIRST, while the step is still `active`: for the window between
    /// the two writes nothing can claim the step, whereas releasing it
    /// to `ready` first would publish a step naming a dead run and let
    /// a fresh claim's edge be nulled by the write that followed.
    #[tokio::test]
    async fn the_edge_is_cleared_through_the_merge_door_and_never_by_omission() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, Some(DEAD_RUN)),
            run(DEAD_RUN, "died", Some("2026-09-19T12:00:00Z")),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");

        let written = puts.lock().unwrap().clone();
        let methods: Vec<&str> = written.iter().map(|(m, ..)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec!["PATCH", "PUT"],
            "the edge is cleared through the merge door BEFORE the step \
             is freed, so no claim can land between them: {written:?}"
        );
        assert_eq!(
            written[0].3["agent_run"],
            serde_json::Value::Null,
            "an explicit null is what deletes a key; omitting it does not"
        );
        assert_eq!(
            written[0].3["reclaimed"]["from_run"], DEAD_RUN,
            "the evidence rides the same merge, so the clear is never \
             recorded without its reason"
        );
        assert!(
            written[1].3.get("metadata").is_none(),
            "the PUT moves status and assignee only — a `metadata` key \
             on it would replace the merge it just made: {:?}",
            written[1].3
        );
    }

    /// One step as the mock server now holds it — the release is judged
    /// on the row, never on the request (ce3a4b16).
    async fn stored_step(base: &str, job: &str, step_id: &str) -> serde_json::Value {
        let job: serde_json::Value = reqwest::get(format!("{base}/api/jobs/{job}"))
            .await
            .expect("the packet reads back")
            .json()
            .await
            .expect("the packet is JSON");
        job["steps"]
            .as_array()
            .expect("the packet has steps")
            .iter()
            .find(|s| s["id"] == json!(step_id))
            .cloned()
            .expect("the released step is on the packet")
    }

    /// THE SECOND BOUND IS THE READABLE WINDOW. A run dead for one hour
    /// under a two-hour bound holds its step: for that window the
    /// record says DEAD and the step is still claimed, which is the
    /// state an operator can read before anything moves.
    #[tokio::test]
    async fn a_freshly_dead_run_still_holds_its_step() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, Some(DEAD_RUN)),
            run(DEAD_RUN, "died", Some("2026-09-19T14:00:00Z")),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "one hour dead under a two-hour bound is not free"
        );
    }

    /// A step a PERSON claimed is not this rule's business at all: no
    /// agent block, so it is not in the population the inbox hands out
    /// and not a candidate here. Without this the handler would warn
    /// about every human claim on the board, once an hour, forever.
    #[tokio::test]
    async fn a_step_with_no_agent_block_is_not_a_candidate() {
        let mut human = packet(PACKET, None);
        human["steps"][1]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("agent_model");
        let (base, puts) = mock_jobs(vec![human]).await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");
        assert!(puts.lock().unwrap().is_empty());
    }

    /// GUARD 2: a claimed step that does not NAME its run is left
    /// alone. This is the whole difference between a reclaim and a
    /// guess — the step's own stillness says nothing about whether
    /// anyone is building.
    #[tokio::test]
    async fn a_claimed_step_with_no_run_edge_is_left_alone() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, None),
            run(DEAD_RUN, "died", Some("2026-09-19T10:00:00Z")),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");
        assert!(puts.lock().unwrap().is_empty());
    }

    /// A run that ended some other way is not a death. `refused` is a
    /// hand's word, and the hand that writes it can release the step
    /// itself; a clock has no such excuse.
    #[tokio::test]
    async fn only_the_named_dead_result_frees_a_step() {
        for ending in ["refused", "gated", "delivered"] {
            let (base, puts) = mock_jobs(vec![
                packet(PACKET, Some(DEAD_RUN)),
                run(DEAD_RUN, ending, Some("2026-09-19T10:00:00Z")),
            ])
            .await;
            let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
            h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
                .await
                .expect("the tick runs");
            assert!(puts.lock().unwrap().is_empty(), "{ending} freed a step");
        }
    }

    /// GUARD 1, the new fact doing its work: a step RE-DISPATCHED after
    /// its first run died carries the NEW run's id, so the old death
    /// frees nothing. Without the edge this case is indistinguishable
    /// from the abandoned one, and the release would take the step out
    /// from under a live executor.
    #[tokio::test]
    async fn a_redispatched_step_is_judged_against_its_current_run() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, Some(LIVE_RUN)),
            run(DEAD_RUN, "died", Some("2026-09-19T10:00:00Z")),
            running(LIVE_RUN),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-19T15:00:00Z")))
            .await
            .expect("the tick runs");
        assert!(puts.lock().unwrap().is_empty());
    }

    /// GUARD 4: a run that MOVED after it died is one somebody is
    /// attending — a late handback recorded by hand — and its step is
    /// not free, whatever the bound says.
    #[test]
    fn a_run_that_moved_after_its_death_holds_its_step() {
        let mut r = run(DEAD_RUN, "died", Some("2026-09-19T10:00:00Z"));
        assert!(matches!(
            run_verdict(&r, "building", "died", DEFAULT_EVIDENCE_KEY),
            RunVerdict::Dead { .. }
        ));
        r["steps"][0]["completed_at"] = json!("2026-09-19T13:00:00Z");
        let held = run_verdict(&r, "building", "died", DEFAULT_EVIDENCE_KEY);
        assert!(
            matches!(&held, RunVerdict::Holds { why } if why.contains("after it died")),
            "{held:?}"
        );
    }

    /// A death with no instant is a finding, not a licence to start the
    /// clock at now — which would free the step `after_hours` later on
    /// an age nobody measured.
    #[test]
    fn a_death_with_no_instant_holds_the_step() {
        let mut r = run(DEAD_RUN, "died", None);
        r["steps"][1]["metadata"]["aged_out"] = json!({ "bound_hours": 4.0 });
        let held = run_verdict(&r, "building", "died", DEFAULT_EVIDENCE_KEY);
        assert!(
            matches!(&held, RunVerdict::Holds { why } if why.contains("carries no instant")),
            "{held:?}"
        );
    }

    /// The death rule's own tick stands in for a row whose
    /// `completed_at` predates the column.
    #[test]
    fn the_death_instant_falls_back_to_the_rules_tick() {
        let mut r = run(DEAD_RUN, "died", None);
        r["steps"][1]["metadata"]["aged_out"] = json!({ "at": "2026-09-19T11:30:00Z" });
        assert_eq!(
            run_verdict(&r, "building", "died", DEFAULT_EVIDENCE_KEY),
            RunVerdict::Dead {
                at: DateTime::parse_from_rfc3339("2026-09-19T11:30:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            }
        );
    }

    /// A daily firing carries no `_at`; the handler refuses rather than
    /// reading a clock of its own, and says which cadence to use — the
    /// same contract `jobs.age_out_step` holds one level up.
    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let (base, puts) = mock_jobs(vec![
            packet(PACKET, Some(DEAD_RUN)),
            run(DEAD_RUN, "died", Some("2026-09-19T10:00:00Z")),
        ])
        .await;
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), &base);
        let err = h
            .invoke(&args(), &ctx(json!({ "_day": "2026-09-19" })))
            .await
            .expect_err("no _at, no judgement");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at") && m.contains("hourly")),
            "{err}"
        );
        assert!(puts.lock().unwrap().is_empty());
    }

    #[test]
    fn a_bound_that_is_not_a_number_is_permanent() {
        for bad in ["-2", "soon", ""] {
            let mut a = args();
            a[4].1 = Value::String(bad.into());
            assert!(
                matches!(after_hours(&a), Err(HandlerError::Permanent(_))),
                "{bad:?}"
            );
        }
        assert_eq!(after_hours(&args()).unwrap(), 2.0);
        let mut zero = args();
        zero[4].1 = Value::String("0".into());
        assert_eq!(
            after_hours(&zero).unwrap(),
            0.0,
            "zero is the rule's argument to make, not this shape's"
        );
    }

    /// THE SHIPPED RULE IS THE ONE THIS HANDLER READS. Every noun here
    /// is a rule arg, so a renamed arg is a rule that silently does
    /// nothing — `arg_string` errors at the first tick and the only
    /// reader is a journal. Parsed through the registry's own parser,
    /// the one the seed publishes with, rather than a second reading of
    /// the file. The `link` VALUE is the key `boss dispatch` stamps on
    /// the step it claims (dd6d44b7), and it is asserted against the
    /// CORE CONST rather than a literal (backlog 1783c6dd).
    ///
    /// WHY THAT MATTERS, AND WHY A LITERAL LOOKED FINE. Production
    /// already reads `boss_jobs::agent_runs::EDGE_KEY` everywhere; it
    /// was only the pins that spelled the key out. Rename the const and
    /// the Rust side moves with it, so every test comparing produced
    /// JSON to produced JSON still agrees — but the RULE FILE is data
    /// and does not move, and a pin that compares the file to a LITERAL
    /// agrees with it. The rename would land green with the rule
    /// following a key nothing writes any more, which is the silent
    /// half of CLAUDE.md 9a: three copies, each held to a different
    /// anchor, so no single test can see them disagree.
    ///
    /// The crate boundary the original builder hit is gone — this crate
    /// depends on `boss-jobs` — so the triangle closes here rather than
    /// needing an equality test between two rule files, which would
    /// have been a fourth anchor rather than one.
    #[test]
    fn the_shipped_rule_carries_the_args_this_handler_reads() {
        const RULE: &str = "an-abandoned-step-is-reclaimed-when-its-run-died";
        let dir = boss_testing::dispatcher_rules_dir();
        let raw = boss_dispatcher::rules::registry::parse_raw_path(&dir)
            .unwrap_or_else(|e| panic!("parse {}: {e}", dir.display()));
        let rule = raw
            .rules
            .iter()
            .find(|r| r.name == RULE)
            .unwrap_or_else(|| panic!("{RULE} is in the shipped registry"));
        let shipped = &rule
            .do_steps
            .first()
            .unwrap_or_else(|| panic!("{RULE} declares a do step"))
            .args;
        // Args are expression sources, so a string literal arrives
        // quoted: `"agent-run"` including the quotes.
        for (key, want) in [
            ("run_kind", "agent-run"),
            ("run_step", "building"),
            ("dead_result", "died"),
            ("link", boss_jobs::agent_runs::EDGE_KEY),
            ("after_hours", "2"),
        ] {
            assert_eq!(
                shipped.get(key).map(String::as_str),
                Some(format!("\"{want}\"").as_str()),
                "the shipped rule's `{key}`"
            );
        }
        // And every arg it carries is read by this handler under that
        // name — an arg nothing reads is a declaration with no effect.
        let handler_args = args();
        let named: Vec<&str> = handler_args.iter().map(|(k, _)| k.as_str()).collect();
        for key in shipped.keys() {
            assert!(
                named.contains(&key.as_str()) || key == "evidence_key",
                "`{key}` is read by nothing"
            );
        }
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsReclaimAbandonedStep::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "jobs.reclaim_abandoned_step");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.reclaim_abandoned_step")
                .cloned(),
            Some(vec!["jobs.step.updated"]),
            "the cascade table knows what this handler emits"
        );
    }
}
