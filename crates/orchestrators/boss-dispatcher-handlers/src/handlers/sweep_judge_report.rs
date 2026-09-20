//! `maintenance.sweep.judge` — an answered measurement judges the sweep
//! that asked for it.
//!
//! The gap this closes (backlog 970c0c94, measured 2026-09-18). Every
//! `measure-*-sweep-on-inspect-ready` rule files an ops-request the
//! moment a daily sweep's `inspect` checklist becomes ready, linked
//! back by `metadata.for_sweep`, and the forge's ops-runner ANSWERS it
//! (exit 0). But the reading landed on the ops-request's `execute` step
//! as free text — disk-report exited 0 at 68% root as it would at 99%,
//! conformance-report exited 0 while reporting one undeclared object —
//! and never on the sweep's `inspect` step, which stayed assigned to
//! the agent forever: four of them sat 24h with a clean reading on
//! another packet. The report verbs now end their output with ONE
//! verdict line (`verdict: clean` or `verdict: <finding>`, judged by a
//! threshold the estate already declares), and this handler reads it.
//!
//! ## What it does
//!
//! On `jobs.job.closed` for an `ops-request` that closed `answered`:
//!
//! 1. Read the closed report. It is this rule's only when its
//!    `metadata.verb` is the `verb` the rule names AND it carries a
//!    `for_sweep` link — a report nobody filed for a sweep is somebody's
//!    debug read, and a verb that does not judge this sweep's question
//!    must not complete it (the disk-report a copy-pasted rule files for
//!    the image-freshness sweep says nothing about image freshness).
//! 2. Read the EXIT the runner recorded, and only then the verdict off
//!    the `execute` step's recorded `output`: the LAST line starting
//!    `verdict: `, because the ops-runner merges stdout and stderr into
//!    one field. No verdict line means the verb predates the verdict —
//!    warn, write nothing, the step waits for a person exactly as
//!    before. A verb that FAILED is never read for a verdict at all
//!    (see below).
//! 3. Read the sweep. It must be a `maintenance-sweep` of the `target`
//!    the rule names, still open, with `inspect` ready or active — a
//!    completed one is a redelivery (JetStream is at-least-once), a
//!    pending one was never routed to, and a step already carrying this
//!    report's id as `source` was written by an earlier delivery.
//! 4. CLEAN: route the sweep to Clear (`PATCH /metadata` with
//!    `action_needed = "false"` FIRST, because the Clear/Remediate
//!    predicates read it when the completion re-evaluates them), then
//!    complete `inspect` with the step's own required fields —
//!    `findings = "none"`, `measured` = the verb and its verdict — plus
//!    `reading` (the verdict line, copied not retyped) and `source` (the
//!    report's id) so a reader of the step follows the evidence to the
//!    packet that measured it. The step's existing metadata rides back:
//!    PUT replaces top-level `metadata` wholesale.
//! 5. UNCLEAN: complete nothing. The verdict line and the report's id
//!    are MERGED onto the step's metadata (`PATCH /steps/{id}/metadata`,
//!    `reading` + `source`) so the agent reads the finding on the step
//!    it is assigned, not in another packet. Routing stays theirs.
//!
//! ## The exit is read BEFORE the verdict (53f54b3f)
//!
//! Step 2 used to read the output with no regard for whether the verb
//! finished — every fixture in this module hardcoded `exit_code: "0"`,
//! so the failed case had never run. A `disk-report` killed at its
//! timeout, or a `conformance-report` whose derivation died mid-walk,
//! leaves a TRUNCATED output, and a truncated measurement is the
//! false-empty class wearing a new hat: absence of a finding read as
//! absence of a problem. Worse, a partial output can still carry an
//! earlier `verdict: clean` line, which would COMPLETE the sweep's
//! inspect and route it to Clear — a day's disk headroom signed off by
//! a reading that never happened.
//!
//! So a failed verb is never read for a verdict. It takes the finding
//! path: the inspect step is not completed, and the failure is written
//! where the agent already assigned to that step reads it, in the
//! spelling the step surface and the receiving yard already draw as
//! troubled (`failed`, `failed_exit`, `failed_source` —
//! `apps/web/src/steps/failedVerb.ts`, 074e1287). NO second packet is
//! filed, and that is the one place this differs from `ops.judge`: a
//! sweep is an OPEN, assigned packet that will now look troubled, so
//! the failure already has a reader, and an urgent alarm beside it
//! every time a daily report dies is the noise CLAUDE.md §Diagnosis
//! warns about. `ops.judge` has no such reader — its check closes and
//! nobody holds anything — which is why it alerts.
//!
//! `exit 75` is not a failure: the estate's verbs use EX_TEMPFAIL for
//! `not yet`, having measured nothing, and that is the no-verdict path
//! this handler already had. One definition of "the verb failed" for
//! the whole family, in `jobs_complete_linked_step::verb_failure`.
//!
//! Same family as `maintenance.sweep.inspect` (empty-decisions,
//! deploy-convergence), which inspects from the jobs API directly;
//! this one inspects from a host verb's answer, which is why it fires
//! on the report's close rather than on the sweep's step becoming
//! ready. Every noun is a rule arg — `target`, `verb` — so a sweep
//! whose measurement gains a verdict is one rule file dropped in.

use super::common::{api_client, get_json, write_json};
use super::jobs_complete_linked_step::{VerbFailure, step_by_slug, unusable_link, verb_failure};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::json;
use std::sync::Arc;

/// The one shape every sweep report ends with. Pinned on the shell
/// side by `boss-testing/tests/sweep_report_verdicts_sh.rs`.
pub(crate) const VERDICT_PREFIX: &str = "verdict: ";
/// The verdict that completes a step; anything else is a finding.
pub(crate) const CLEAN: &str = "clean";

/// The step the ops-runner completes with the verb's output.
const REPORT_STEP: &str = "execute";
/// The sweep step a clean verdict completes.
const INSPECT_STEP: &str = "inspect";

/// PURE: the verdict line a recorded output carries — the LAST line
/// starting with `verdict: `, trimmed — or `None` when the verb wrote
/// none (it predates the verdict, or was killed before its last line).
pub(crate) fn verdict_line(output: &str) -> Option<&str> {
    output
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with(VERDICT_PREFIX))
}

/// PURE: is this verdict line the clean one? Exactly `verdict: clean`,
/// whitespace aside — `verdict: clean-ish` is a finding.
pub(crate) fn is_clean(verdict: &str) -> bool {
    verdict
        .trim()
        .strip_prefix(VERDICT_PREFIX)
        .map(str::trim)
        .is_some_and(|v| v == CLEAN)
}

/// What a report IS, as one value (53f54b3f) — so the three branches
/// below cannot disagree about it, and the decision is testable without
/// a jobs API.
pub(crate) enum Reading<'a> {
    /// The verb ran to a clean verdict: the sweep's inspect completes.
    Clean(&'a str),
    /// The verb ran and found something: the agent's, on the step.
    Finding(&'a str),
    /// The verb RAN AND FAILED, so there is no measurement to read —
    /// whatever its output says, it is partial. Never a completion.
    Unmeasured(VerbFailure),
}

/// PURE: read the EXIT first, the output only then. A failed verb's
/// output is not consulted for a verdict at all, because a truncated
/// report can carry an earlier `verdict: clean` line and completing an
/// inspect on it signs off a reading that never happened. `None` when
/// the verb exited cleanly (or `not yet`) and wrote no `verdict:` line:
/// it predates the verdict, and the step waits for a person as before.
pub(crate) fn reading_of(report: &serde_json::Value) -> Option<Reading<'_>> {
    if let Some(failure) = verb_failure(report) {
        return Some(Reading::Unmeasured(failure));
    }
    let output = step_by_slug(report, REPORT_STEP)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("output"))
        .and_then(|o| o.as_str())
        .unwrap_or("");
    let verdict = verdict_line(output)?;
    Some(if is_clean(verdict) {
        Reading::Clean(verdict)
    } else {
        Reading::Finding(verdict)
    })
}

/// PURE: what a failed measurement writes onto the inspect step it
/// refuses to complete. `reading` is the sentence the assigned agent
/// sees where the verdict would have been; the other three are the
/// spelling `apps/web/src/steps/failedVerb.ts` already draws as
/// troubled, so the step surface and the receiving yard say a verb
/// failed here without a line of frontend change.
pub(crate) fn unmeasured_note(
    verb: &str,
    host: &str,
    failure: &VerbFailure,
    report_id: &str,
) -> serde_json::Value {
    json!({
        "reading": format!(
            "{verb} on {host} FAILED (exit {}) — nothing measured, so nothing is signed off: {}",
            failure.exit, failure.line
        ),
        "source": report_id,
        "failed": failure.line,
        "failed_exit": failure.exit,
        "failed_source": report_id,
    })
}

/// PURE: the body that completes a sweep's `inspect` step from a clean
/// report — the step's existing metadata plus the fields the step
/// requires and the evidence this handler adds. ONE definition, so a
/// test can hold it against the fields maintenance-sweep.toml declares
/// (`the_clean_completion_validates_against_the_inspect_step`).
pub(crate) fn clean_completion_body(
    existing: &serde_json::Map<String, serde_json::Value>,
    verb: &str,
    host: &str,
    verdict: &str,
    report_id: &str,
    actor: &str,
    at: &str,
) -> serde_json::Value {
    let mut merged = existing.clone();
    merged.insert("findings".into(), json!("none"));
    merged.insert(
        "measured".into(),
        json!(format!(
            "{verb} on {host}: {verdict} (ops-request {report_id})"
        )),
    );
    merged.insert("reading".into(), json!(verdict));
    merged.insert("source".into(), json!(report_id));
    merged.insert(
        "items".into(),
        json!([{
            "label": format!("{verb} on {host} answered {verdict}"),
            "checked": true,
            "checked_by": actor,
            "checked_at": at,
        }]),
    );
    json!({ "status": "completed", "metadata": serde_json::Value::Object(merged) })
}

pub struct MaintenanceSweepJudge {
    client: reqwest::Client,
    jobs_base: String,
}

impl MaintenanceSweepJudge {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Tests point the client at a local stand-in for jobs-api.
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn job(&self, id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        Ok(job.get("data").cloned().unwrap_or(job))
    }
}

fn meta_str<'a>(job: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    job.get("metadata")?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn status_of(v: &serde_json::Value) -> &str {
    v.get("status").and_then(|s| s.as_str()).unwrap_or("")
}

#[async_trait]
impl Handler for MaintenanceSweepJudge {
    fn name(&self) -> &'static str {
        "maintenance.sweep.judge"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let target = arg_string(args, "target")?;
        let verb = arg_string(args, "verb")?;
        let rule = ctx.rule_name.as_str();

        // The close marker names the packet. A malformed marker is not
        // something a redelivery can fix, so it is a no-op, not an error.
        let Some(report_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        // 1. The report — this rule's only if it is the verb that judges
        //    this target's question and it was filed for a sweep.
        let report = self.job(report_id, rule).await?;
        if meta_str(&report, "verb") != Some(verb) {
            return Ok(());
        }
        let Some(sweep_id) = meta_str(&report, "for_sweep") else {
            tracing::debug!(rule = %rule, report = %report_id, "answered {verb} was not filed for a sweep — nothing to judge");
            return Ok(());
        };
        if let Some(why) = unusable_link(sweep_id) {
            tracing::warn!(rule = %rule, report = %report_id, "for_sweep: {why}");
            return Ok(());
        }
        let host = meta_str(&report, "host").unwrap_or("host");

        // 2. THE EXIT, then the verdict — in that order (53f54b3f).
        let Some(reading) = reading_of(&report) else {
            tracing::warn!(
                rule = %rule,
                report = %report_id,
                sweep = %sweep_id,
                "{verb} answered without a `verdict:` line — the verb predates the verdict; nothing judged, the inspect step waits for a person as before"
            );
            return Ok(());
        };

        // 3. The sweep, and the one step this may touch.
        let sweep = self.job(sweep_id, rule).await?;
        if sweep.get("kind").and_then(|k| k.as_str()) != Some("maintenance-sweep")
            || meta_str(&sweep, "target") != Some(target)
        {
            // Another rule's sweep (each names one target), or a link
            // that points somewhere this obligation does not act.
            return Ok(());
        }
        if matches!(status_of(&sweep), "closed" | "cancelled") {
            return Ok(());
        }
        let Some(inspect) = step_by_slug(&sweep, INSPECT_STEP) else {
            return Ok(());
        };
        if !matches!(status_of(inspect), "ready" | "active") {
            // completed: a redelivery, or a person got there first;
            // pending: never routed to. Either way not ours.
            return Ok(());
        }
        let Some(step_id) = inspect.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let existing = match inspect.get("metadata") {
            Some(serde_json::Value::Object(m)) => m.clone(),
            _ => serde_json::Map::new(),
        };
        if existing.get("source").and_then(|s| s.as_str()) == Some(report_id) {
            // Already written from this very report by an earlier
            // delivery; the record is true.
            return Ok(());
        }

        let step_url = format!("{}/api/jobs/{sweep_id}/steps/{step_id}", self.base());
        match reading {
            Reading::Clean(verdict) => {
                // 4. Route FIRST, then complete — the Clear predicate
                //    reads `job.metadata.action_needed` when the
                //    completion re-evaluates the sweep's steps.
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{sweep_id}/metadata", self.base()),
                    &json!({ "action_needed": "false" }),
                    rule,
                )
                .await?;
                let at = step_by_slug(&report, REPORT_STEP)
                    .and_then(|s| s.get("completed_at"))
                    .and_then(|v| v.as_str())
                    .or_else(|| ctx.event_payload.get("closed_on").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                // The same spelling the step API records as
                // `completed_by` for a dispatcher write
                // (`automation:rule:<name>`), so the item and the step
                // agree on who checked it.
                let actor = format!("automation:rule:{rule}");
                let body =
                    clean_completion_body(&existing, verb, host, verdict, report_id, &actor, &at);
                write_json(&self.client, reqwest::Method::PUT, &step_url, &body, rule).await?;
                tracing::info!(rule = %rule, sweep = %sweep_id, report = %report_id, "{verdict} — inspect completed and routed to Clear");
            }
            Reading::Finding(verdict) => {
                // 5. A finding stays the agent's; the reading goes
                //    where they will read it.
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{step_url}/metadata"),
                    &json!({ "reading": verdict, "source": report_id }),
                    rule,
                )
                .await?;
                tracing::info!(rule = %rule, sweep = %sweep_id, report = %report_id, "{verdict} — written onto the inspect step, which stays open");
            }
            Reading::Unmeasured(failure) => {
                // 6. THE VERB FAILED (53f54b3f). No completion, no
                //    routing to Clear, and nothing read out of a
                //    partial output — the step stays the agent's and
                //    now LOOKS troubled, on the packet they are already
                //    assigned to. No alarm is filed: this open sweep is
                //    the alarm (see the module doc).
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{step_url}/metadata"),
                    &unmeasured_note(verb, host, &failure, report_id),
                    rule,
                )
                .await?;
                tracing::warn!(
                    rule = %rule,
                    sweep = %sweep_id,
                    report = %report_id,
                    "{verb} FAILED (exit {}) — nothing measured, inspect left open and annotated; {}",
                    failure.exit,
                    failure.line
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const REPORT: &str = "11111111-1111-1111-1111-111111111111";
    const SWEEP: &str = "22222222-2222-2222-2222-222222222222";
    const INSPECT: &str = "s-inspect";
    const RULE: &str = "judge-disk-headroom-sweep-on-report-answered";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            // The close marker in the shape all three emit sites produce:
            // every key present, and NO step metadata.
            event_payload: json!({
                "id": REPORT,
                "closed_on": "2026-09-18",
                "kind": "ops-request",
                "outcome": "answered",
                "title": "disk-report on forge — the disk-headroom sweep's measurement",
                "subject_id": "forge",
                "parent_step_id": null,
            }),
        }
    }

    fn args(target: &str, verb: &str) -> Vec<(String, Value)> {
        vec![
            ("target".to_string(), Value::String(target.into())),
            ("verb".to_string(), Value::String(verb.into())),
        ]
    }

    /// The answered report, as the ops-runner completes it — the verb
    /// having exited 0.
    fn report(verb: &str, for_sweep: Option<&str>, output: &str) -> serde_json::Value {
        report_exit(verb, for_sweep, output, "0")
    }

    /// The same, with the exit the runner recorded on the execute step
    /// (53f54b3f): the fixture here hardcoded `"0"`, which is why no
    /// test had ever driven this handler with a report that died.
    fn report_exit(
        verb: &str,
        for_sweep: Option<&str>,
        output: &str,
        exit: &str,
    ) -> serde_json::Value {
        let mut metadata = json!({ "host": "forge", "verb": verb, "args": [], "exit": exit });
        if let Some(s) = for_sweep {
            metadata["for_sweep"] = json!(s);
        }
        json!({
            "id": REPORT,
            "kind": "ops-request",
            "status": "closed",
            "metadata": metadata,
            "steps": [
                { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "r-execute", "spec_slug": "execute", "status": "completed",
                  "completed_at": "2026-09-18T10:02:00Z",
                  "metadata": { "disposition": "answered", "exit_code": exit, "runner_host": "forge",
                                "output": output, "authority_role": "platform-admin" } },
                { "id": "r-answered", "spec_slug": "answered", "status": "completed", "metadata": {} },
            ],
        })
    }

    /// The open sweep whose inspect step is in `inspect_status`.
    fn sweep(target: &str, inspect_status: &str) -> serde_json::Value {
        json!({
            "id": SWEEP,
            "kind": "maintenance-sweep",
            "status": "open",
            "metadata": { "target": target, "area": "infra", "opened_at": "2026-09-18T10:00:00Z" },
            "steps": [
                { "id": "s-opened", "spec_slug": "opened", "status": "completed", "metadata": {} },
                { "id": INSPECT, "spec_slug": "inspect", "status": inspect_status,
                  "assignee_id": "agent-claude",
                  "metadata": { "authority_role": "platform-admin" } },
                { "id": "s-remediate", "spec_slug": "remediate", "status": "pending", "metadata": {} },
                { "id": "s-clear", "spec_slug": "clear", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// Every write the handler made: (method, path, body), in order.
    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: `GET /api/jobs/{id}` serves one; the three
    /// write doors the handler may use are recorded and applied, so a
    /// second delivery reads what the first wrote.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let (g, pj, ps, pm) = (by_id.clone(), by_id.clone(), by_id.clone(), by_id.clone());
        let (wj, ws, wm) = (writes.clone(), writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = g.clone();
                    async move {
                        by_id
                            .lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let (w, by_id) = (wj.clone(), pj.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let (Some(m), Some(b)) =
                                    (job["metadata"].as_object_mut(), body.as_object())
                            {
                                for (k, v) in b {
                                    m.insert(k.clone(), v.clone());
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let (w, by_id) = (ws.clone(), ps.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PUT".into(),
                                format!("/api/jobs/{id}/steps/{step_id}"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                for step in job["steps"].as_array_mut().into_iter().flatten() {
                                    if step["id"] == json!(step_id) {
                                        if let Some(s) = body.get("status") {
                                            step["status"] = s.clone();
                                        }
                                        if let Some(m) = body.get("metadata") {
                                            step["metadata"] = m.clone();
                                        }
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
                        let (w, by_id) = (wm.clone(), pm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/steps/{step_id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                for step in job["steps"].as_array_mut().into_iter().flatten() {
                                    if step["id"] == json!(step_id)
                                        && let (Some(m), Some(b)) =
                                            (step["metadata"].as_object_mut(), body.as_object())
                                    {
                                        for (k, v) in b {
                                            m.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    const CLEAN_OUTPUT: &str =
        "== filesystems ==\n/dev/root 228G 100G 120G 46% /\n== verdict ==\nverdict: clean\n";
    const TIGHT_OUTPUT: &str = "== filesystems ==\n/dev/root 228G 160G 63G 72% /\n== verdict ==\nverdict: disk_tight free=63g floor=80g\n";

    /// A clean report routes the sweep to Clear FIRST and then completes
    /// its inspect step with the step's required fields, the verdict
    /// copied as `reading`, the report's id as `source`, and the step's
    /// existing metadata kept.
    #[tokio::test]
    async fn a_clean_verdict_routes_to_clear_and_completes_the_inspect_step() {
        let (base, writes) = mock_jobs(vec![
            report("disk-report", Some(SWEEP), CLEAN_OUTPUT),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "route + complete, nothing else: {w:?}");
        assert_eq!(
            (w[0].0.as_str(), w[0].1.as_str()),
            ("PATCH", &*format!("/api/jobs/{SWEEP}/metadata"))
        );
        assert_eq!(
            w[0].2,
            json!({ "action_needed": "false" }),
            "Clear reads action_needed = \"false\""
        );
        assert_eq!(
            (w[1].0.as_str(), w[1].1.as_str()),
            ("PUT", &*format!("/api/jobs/{SWEEP}/steps/{INSPECT}"))
        );
        let m = &w[1].2["metadata"];
        assert_eq!(w[1].2["status"], "completed");
        assert_eq!(m["reading"], "verdict: clean", "the verdict line, copied");
        assert_eq!(m["source"], REPORT, "the packet that measured it");
        assert_eq!(m["findings"], "none");
        assert!(
            m["measured"]
                .as_str()
                .is_some_and(|s| s.contains("disk-report") && s.contains("verdict: clean")),
            "measured names the verb and the verdict: {m}"
        );
        assert_eq!(
            m["authority_role"], "platform-admin",
            "PUT replaces metadata wholesale, so the existing keys ride back"
        );
        assert_eq!(
            m["items"][0]["checked_by"],
            format!("automation:rule:{RULE}")
        );
        assert_eq!(m["items"][0]["checked_at"], "2026-09-18T10:02:00Z");
    }

    /// An unclean report completes NOTHING: the step stays on the agent,
    /// with the verdict merged onto its metadata so the finding is read
    /// on the step, and the sweep is not routed either way.
    #[tokio::test]
    async fn an_unclean_verdict_writes_the_reading_onto_the_open_step_and_completes_nothing() {
        let (base, writes) = mock_jobs(vec![
            report("disk-report", Some(SWEEP), TIGHT_OUTPUT),
            sweep("disk-headroom", "active"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "one merge onto the step, nothing else: {w:?}");
        assert_eq!(
            (w[0].0.as_str(), w[0].1.as_str()),
            (
                "PATCH",
                &*format!("/api/jobs/{SWEEP}/steps/{INSPECT}/metadata")
            )
        );
        assert_eq!(
            w[0].2,
            json!({ "reading": "verdict: disk_tight free=63g floor=80g", "source": REPORT })
        );
    }

    /// The verb predates the verdict: nothing is written and nothing is
    /// guessed — the step waits for a person exactly as before.
    #[tokio::test]
    async fn a_report_without_a_verdict_line_judges_nothing() {
        let (base, writes) = mock_jobs(vec![
            report(
                "disk-report",
                Some(SWEEP),
                "== filesystems ==\n/dev/root 228G 100G 120G 46% /\n",
            ),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty());
    }

    /// A rule names one (target, verb) pair. Another verb's answer, a
    /// report filed for no sweep, and a sweep of another target are all
    /// somebody else's — a clean disk says nothing about image freshness.
    #[tokio::test]
    async fn only_the_named_verb_judges_the_named_target() {
        // Right verb, wrong target: the sweep the link names is not this rule's.
        let (base, writes) = mock_jobs(vec![
            report("disk-report", Some(SWEEP), CLEAN_OUTPUT),
            sweep("image-freshness", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty(), "wrong target");

        // Wrong verb for this rule.
        let (base, writes) = mock_jobs(vec![
            report("conformance-report", Some(SWEEP), CLEAN_OUTPUT),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty(), "wrong verb");

        // A debug read nobody filed for a sweep.
        let (base, writes) = mock_jobs(vec![
            report("disk-report", None, CLEAN_OUTPUT),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty(), "no for_sweep link");
    }

    /// At-least-once delivery: the second delivery of one close finds
    /// the inspect step completed (clean) or already carrying this
    /// report as `source` (unclean) and writes nothing more.
    #[tokio::test]
    async fn a_redelivery_writes_nothing_more() {
        for (output, expect_writes) in [(CLEAN_OUTPUT, 2), (TIGHT_OUTPUT, 1)] {
            let (base, writes) = mock_jobs(vec![
                report("disk-report", Some(SWEEP), output),
                sweep("disk-headroom", "ready"),
            ])
            .await;
            let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
            let a = args("disk-headroom", "disk-report");
            h.invoke(&a, &ctx()).await.unwrap();
            h.invoke(&a, &ctx()).await.unwrap();
            assert_eq!(
                writes.lock().unwrap().len(),
                expect_writes,
                "{output}: the second delivery must add no write"
            );
        }
    }

    /// A pending inspect step was never routed to, and a closed sweep is
    /// finished: neither is touched.
    #[tokio::test]
    async fn a_pending_step_or_a_closed_sweep_is_not_touched() {
        let (base, writes) = mock_jobs(vec![
            report("disk-report", Some(SWEEP), CLEAN_OUTPUT),
            sweep("disk-headroom", "pending"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty(), "pending");

        let mut closed = sweep("disk-headroom", "ready");
        closed["status"] = json!("closed");
        let (base, writes) = mock_jobs(vec![
            report("disk-report", Some(SWEEP), CLEAN_OUTPUT),
            closed,
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty(), "closed");
    }

    #[test]
    fn the_verdict_is_the_last_verdict_line_and_clean_is_exact() {
        assert_eq!(verdict_line(CLEAN_OUTPUT), Some("verdict: clean"));
        assert_eq!(
            verdict_line(TIGHT_OUTPUT),
            Some("verdict: disk_tight free=63g floor=80g")
        );
        // The ops-runner merges streams, and a killed verb may leave a
        // marker after the verdict: the LAST verdict line wins, and
        // trailing text does not hide it.
        assert_eq!(
            verdict_line(
                "verdict: 1 undeclared object\nverdict: clean\n[ops-runner: output truncated]\n"
            ),
            Some("verdict: clean")
        );
        assert_eq!(verdict_line("no verdict here\n"), None);
        assert!(is_clean("verdict: clean"));
        assert!(is_clean("  verdict: clean  "));
        assert!(!is_clean("verdict: cleanish"));
        assert!(!is_clean("verdict: 3 images older than 14d"));
        assert!(!is_clean("verdict: unanswered (exit 4)"));
    }

    /// THE PIN (CLAUDE.md §9a): the body this handler PUTs and the fields
    /// maintenance-sweep.toml declares for `inspect` are two homes for
    /// one shape — the same pin `sweep_empty_decisions` holds, with the
    /// same validator the step API ran when it refused an array for
    /// `findings` on 2026-09-13.
    #[test]
    fn the_clean_completion_validates_against_the_inspect_step() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows"
        );
        let specs = boss_jobs::seed_loader::load_workflows(dir).expect("the platform bundle loads");
        let inspect = specs
            .iter()
            .find(|w| w.kind == "maintenance-sweep")
            .expect("maintenance-sweep is a platform protocol")
            .steps
            .iter()
            .find(|st| st.title == INSPECT_STEP)
            .expect("the sweep has an inspect step")
            .clone();
        let mut existing = serde_json::Map::new();
        existing.insert("authority_role".into(), json!("platform-admin"));
        let body = clean_completion_body(
            &existing,
            "disk-report",
            "forge",
            "verdict: clean",
            REPORT,
            "rule:t",
            "2026-09-18T10:02:00Z",
        );
        boss_jobs::step_registry::StepRegistry::validate_authored_fields(
            &inspect.fields,
            &body["metadata"],
        )
        .unwrap_or_else(|e| panic!("the step API would refuse this body: {e:?}"));
    }

    /// THE FAILED MEASUREMENT (53f54b3f), in its sharpest shape: the
    /// report died AFTER printing a clean verdict line, so the old
    /// handler would have completed the sweep's inspect and routed it
    /// to Clear — a day's disk headroom signed off by a reading that
    /// never finished. Nothing is completed; the step stays the
    /// agent's, annotated in the spelling the surfaces draw as
    /// troubled.
    #[tokio::test]
    async fn a_failed_report_completes_nothing_and_troubles_the_step_instead() {
        let output = format!("{CLEAN_OUTPUT}disk-report: FAILED — df died mid-read\n");
        let (base, writes) = mock_jobs(vec![
            report_exit("disk-report", Some(SWEEP), &output, "1"),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(
            !w.iter().any(|(m, _, _)| m == "PUT"),
            "a measurement that did not finish completes nothing: {w:?}"
        );
        assert!(
            !w.iter()
                .any(|(_, p, _)| p == &format!("/api/jobs/{SWEEP}/metadata")),
            "and routes nothing to Clear: {w:?}"
        );
        assert_eq!(w.len(), 1, "one write, onto the step: {w:?}");
        let (method, path, body) = &w[0];
        assert_eq!(method, "PATCH");
        assert_eq!(path, &format!("/api/jobs/{SWEEP}/steps/{INSPECT}/metadata"));
        assert_eq!(body["failed"], "disk-report: FAILED — df died mid-read");
        assert_eq!(body["failed_exit"], "1");
        assert_eq!(body["failed_source"], REPORT);
        assert_eq!(body["source"], REPORT);
        let reading = body["reading"].as_str().unwrap();
        assert!(reading.contains("FAILED (exit 1)"), "{reading}");
        assert!(
            !reading.contains("verdict: clean"),
            "the partial reading is never presented as the verdict: {reading}"
        );
    }

    /// A verb KILLED at its timeout has no FAILED line of its own — the
    /// runner's marker is the last line — and no verdict line at all.
    /// It is still a failure, and the step must say so rather than
    /// falling through the predates-the-verdict path in silence.
    #[tokio::test]
    async fn a_killed_report_is_written_onto_the_step_not_passed_over() {
        let output = "== filesystems ==\n\n[ops-runner: command killed at 300s timeout]\n";
        let (base, writes) = mock_jobs(vec![
            report_exit("disk-report", Some(SWEEP), output, "124"),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "the step is annotated, nothing else: {w:?}");
        assert_eq!(w[0].2["failed_exit"], "124");
        assert_eq!(
            w[0].2["failed"], "[ops-runner: command killed at 300s timeout]",
            "a verdict must name what failed"
        );
    }

    /// EX_TEMPFAIL is not a failure — the estate's verbs use 75 for
    /// `not yet`, having measured nothing — so it takes the path a verb
    /// with no verdict line always took: nothing written, the step
    /// waits for a person, and no note claiming a failure that did not
    /// happen.
    #[tokio::test]
    async fn a_not_yet_report_is_not_a_failure_and_writes_nothing() {
        let output = "disk-report: not yet: the host is mid-converge; nothing measured\n";
        let (base, writes) = mock_jobs(vec![
            report_exit("disk-report", Some(SWEEP), output, "75"),
            sweep("disk-headroom", "ready"),
        ])
        .await;
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args("disk-headroom", "disk-report"), &ctx())
            .await
            .unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "a not-yet claims nothing: {:?}",
            writes.lock().unwrap()
        );
    }

    /// The decision is one value, readable without a jobs API.
    #[test]
    fn the_exit_decides_before_the_output_does() {
        let clean = report("disk-report", Some(SWEEP), CLEAN_OUTPUT);
        assert!(matches!(reading_of(&clean), Some(Reading::Clean(_))));
        let tight = report("disk-report", Some(SWEEP), TIGHT_OUTPUT);
        assert!(matches!(reading_of(&tight), Some(Reading::Finding(_))));
        // The same clean output, from a verb that did not finish.
        let died = report_exit("disk-report", Some(SWEEP), CLEAN_OUTPUT, "1");
        assert!(matches!(reading_of(&died), Some(Reading::Unmeasured(_))));
        let not_yet = report_exit("disk-report", Some(SWEEP), "not yet\n", "75");
        assert!(reading_of(&not_yet).is_none());
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "maintenance.sweep.judge");
        let emits = boss_dispatcher::cascade::handler_emits()
            .get("maintenance.sweep.judge")
            .cloned()
            .expect("the cascade table knows this handler");
        for topic in [
            "jobs.step.completed",
            "jobs.job.updated",
            "jobs.step.updated",
        ] {
            assert!(emits.contains(&topic), "emits {topic}: {emits:?}");
        }
    }

    #[tokio::test]
    async fn a_missing_arg_is_a_missing_arg_error() {
        let h = MaintenanceSweepJudge::with_client(reqwest::Client::new(), "http://unused");
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::MissingArg(ref a) if a == "target"),
            "{err:?}"
        );
    }
}
