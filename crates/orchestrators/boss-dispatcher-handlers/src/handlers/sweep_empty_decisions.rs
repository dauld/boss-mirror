//! `maintenance.sweep.inspect` (empty-decisions target) — the sweep
//! that catches a human judgement recorded nowhere.
//!
//! A completed step whose StepType surface is `approval` is a decision
//! point. When one completes carrying neither content (metadata beyond
//! the materialization keys the machine stamps) nor notes, the
//! judgement it was supposed to record is lost. The empty-decisions
//! sweep (spawn rule `maintenance-sweep-empty-decisions-daily`, schema
//! 202608310100) files a `maintenance-sweep` packet daily whose
//! `Inspect: empty-decisions` checklist step nothing was completing —
//! so it piled up. This handler is the executor the design intended
//! (ee8ec68a: mechanical inspections become automation): on the
//! Inspect step it counts the empty decisions in the window, completes
//! the checklist with what it found, and stamps `action_needed` so the
//! packet routes to Remediate (findings) or Clear (none).
//!
//! It owns no database — it reads the same public surfaces any caller
//! reads (`/api/jobs/step-types` for which kinds are approval, then
//! `/api/jobs`), and the ONE write completes the checklist step.
//! `approval` comes from the registry, never a hardcoded kind name
//! (CLAUDE.md §9, no-step-kind-match).
//!
//! The rule names the sweep target it inspects (`target` arg; absent
//! means `empty-decisions`, the first one). A second target,
//! `deploy-convergence`, reads the trains' merged/converged stamps
//! (`sweep_deploy_convergence`, backlog 8e8311f5); the routing PATCH
//! and the checklist PUT are shared. Targets that measure a host —
//! disk, images, conformance — arrive with an ops-request instead
//! (`measure-*-sweep-on-inspect-ready`) and are not this handler's.

use async_trait::async_trait;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

use super::common::{
    StepEvent, api_client, dispatcher_actor_header, dispatcher_reader_header, row_or_refuse,
    rows_or_refuse, sim_origin_value,
};
use super::sweep_deploy_convergence;

/// The keys the machine stamps at materialization or completion — NOT
/// authored content. A decision whose only metadata is these recorded
/// no judgement. (The empty-decisions sweep procedure, cdfe2e1a.)
pub(crate) const MATERIALIZATION_KEYS: &[&str] = &[
    "authority_role",
    "context_md",
    "procedure",
    "outcome_kind",
    "started_at",
    "completed_at",
    "sign_off_context",
    "spec_slug",
    "title_template",
];

/// One decision that recorded nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmptyDecision {
    pub job_id: String,
    pub step_id: String,
    pub title: String,
}

fn has_content(metadata: &Value) -> bool {
    let Some(map) = metadata.as_object() else {
        return false;
    };
    map.iter().any(|(k, v)| {
        if MATERIALIZATION_KEYS.contains(&k.as_str()) {
            return false;
        }
        match v {
            Value::Null => false,
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true, // a number or bool IS a recorded value
        }
    })
}

fn notes_empty(step: &Value) -> bool {
    step.get("notes")
        .and_then(Value::as_str)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

/// The empty approval decisions across `jobs`: completed steps whose
/// kind is in `approval_kinds`, completed on/after `since` (a
/// `YYYY-MM-DD` date compared lexically — `completed_on` is
/// day-granular), carrying neither content nor notes. Pure: the handler
/// feeds it what it fetched.
pub(crate) fn empty_approval_decisions(
    jobs: &[Value],
    approval_kinds: &BTreeSet<String>,
    since: &str,
) -> Vec<EmptyDecision> {
    let mut out = Vec::new();
    for job in jobs {
        let job_id = job.get("id").and_then(Value::as_str).unwrap_or_default();
        let steps = job.get("steps").and_then(Value::as_array);
        for step in steps.into_iter().flatten() {
            if step.get("status").and_then(Value::as_str) != Some("completed") {
                continue;
            }
            let kind = step.get("kind").and_then(Value::as_str).unwrap_or_default();
            if !approval_kinds.contains(kind) {
                continue;
            }
            let completed_on = step
                .get("completed_on")
                .and_then(Value::as_str)
                .unwrap_or("9999-12-31");
            if completed_on < since {
                continue;
            }
            let metadata = step.get("metadata").cloned().unwrap_or(Value::Null);
            if has_content(&metadata) || !notes_empty(step) {
                continue;
            }
            out.push(EmptyDecision {
                job_id: job_id.to_string(),
                step_id: step
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                title: step
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    out
}

/// The kinds whose StepType `surface` is `approval`, from a
/// `/api/jobs/step-types` listing (each entry has `kind` + `surface`).
pub(crate) fn approval_kinds(step_types: &[Value]) -> BTreeSet<String> {
    step_types
        .iter()
        .filter(|t| t.get("surface").and_then(Value::as_str) == Some("approval"))
        .filter_map(|t| t.get("kind").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// The `maintenance.sweep.inspect` handler, empty-decisions target.
pub struct MaintenanceSweepInspect {
    client: reqwest::Client,
    jobs_base: String,
}

impl MaintenanceSweepInspect {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Tests point the client at a local server.
    #[cfg(test)]
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    async fn get(&self, path: &str) -> Result<Value, HandlerError> {
        let url = format!("{}{path}", self.jobs_base.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {st}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }
}

/// THE ONE BODY the Inspect completion sends, for either target — so a
/// test can hold it against the fields maintenance-sweep.toml declares
/// for the step (`inspection_bodies_validate_against_the_step_they_complete`).
pub(crate) fn inspection_put_body(findings: String, measured: String, items: Vec<Value>) -> Value {
    json!({
        "status": "completed",
        "metadata": {
            "findings": findings,
            "measured": measured,
            "items": items,
        },
    })
}

/// The Inspect step's `findings` field is a STRING (maintenance-sweep.toml:
/// `field_type = "string", required = true`), one finding per line, or
/// the word `none`. The handler used to send an array — and from
/// 2026-09-13 the step API refused it: `400 invalid step metadata:
/// findings: expected type 'string', got array`, eight attempts, a
/// dead letter on each sweep, both self-inspections silent for a day
/// and the daily spawner's dedup guard then filing no new sweeps at all.
/// The dead letter on the packet is what named it.
pub(crate) fn findings_text(lines: &[String]) -> String {
    if lines.is_empty() {
        "none".to_string()
    } else {
        lines.join("\n")
    }
}

/// The sweep target a rule asks this handler to inspect. The first
/// rule (`inspect-empty-decisions-sweep-on-step-ready`) predates the
/// arg and names none, so absent means `empty-decisions`; a wrong
/// type is the rule author's error and is refused as one.
pub(crate) fn wanted_target(
    args: &[(String, boss_dispatcher::rules::expr::Value)],
) -> Result<&str, HandlerError> {
    match arg_string(args, "target") {
        Ok(t) => Ok(t),
        Err(HandlerError::MissingArg(_)) => Ok("empty-decisions"),
        Err(e) => Err(e),
    }
}

/// The rows of a `GET /api/jobs` listing, or a retryable refusal naming
/// the read. This replaced `data_rows`, which took either envelope or a
/// bare array and read ANY other body as zero rows — so an error answer
/// meant zero trains, zero approval kinds or zero open packets, and the
/// sweep cleared on a read that saw nothing (backlog d4698bc2).
fn listing_rows(v: &Value, what: &str) -> Result<Vec<Value>, HandlerError> {
    rows_or_refuse(v, what).map_err(HandlerError::Downstream)
}

/// `/api/jobs/step-types` answers a BARE array (boss-jobs
/// `list_step_types`), not an envelope — its own shape, judged here
/// rather than guessed at beside the listings.
fn step_type_rows(v: &Value) -> Result<Vec<Value>, HandlerError> {
    v.as_array().cloned().ok_or_else(|| {
        HandlerError::Downstream("GET /api/jobs/step-types answered no array".into())
    })
}

#[async_trait]
impl Handler for MaintenanceSweepInspect {
    fn name(&self) -> &'static str {
        "maintenance.sweep.inspect"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let wanted = wanted_target(args)?;
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        // Cheap filter first — this rule rides the shared `step.ready.*`
        // subscription, so it fires on every ready step. The Inspect
        // step is the only `checklist` a sweep has; skip everything else
        // before spending a fetch.
        if ev.kind != "checklist" {
            return Ok(());
        }

        let job = self.get(&format!("/api/jobs/{}", ev.job_id)).await?;
        // A body that is not a job would fail the kind check below and
        // skip this ready step without a word (backlog f2eac973).
        let job = row_or_refuse(job, &format!("GET /api/jobs/{}", ev.job_id))
            .map_err(HandlerError::Downstream)?;
        if job.get("kind").and_then(Value::as_str) != Some("maintenance-sweep") {
            return Ok(());
        }
        let target = job
            .pointer("/metadata/target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if target != wanted {
            // Each rule inspects the one target it names; a sweep of
            // another target is another rule's (or an ops-request's).
            return Ok(());
        }
        // Idempotent: a re-delivery finds the Inspect step already
        // completed and does nothing (the step API would 409 a write to
        // a terminal step anyway).
        let inspect = job
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(ev.step_id));
        let Some(inspect) = inspect else {
            return Ok(());
        };
        if inspect.get("status").and_then(Value::as_str) != Some("ready") {
            return Ok(());
        }
        let actor = format!("automation:{}", self.name());

        if target == "deploy-convergence" {
            // The trains' own stamps are the measurement; `now` is the
            // sweep's open instant, so a replay reads the same answer.
            let now = job
                .pointer("/metadata/opened_at")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&chrono::Utc))
                .ok_or_else(|| {
                    HandlerError::Downstream(format!(
                        "sweep {} has no parseable metadata.opened_at — the daily spawner stamps it",
                        ev.job_id
                    ))
                })?;
            let trains = self.get("/api/jobs?kind=pr-train&limit=200").await?;
            let trains = listing_rows(&trains, "the train read (GET /api/jobs?kind=pr-train)")?;
            let insp = sweep_deploy_convergence::inspect(&trains, now, &actor);
            return self
                .complete(
                    ctx,
                    &ev,
                    insp.action_needed,
                    findings_text(&insp.findings),
                    insp.measured,
                    insp.items,
                )
                .await;
        }

        // The sweep window: decisions completed on/after the day this
        // sweep was filed. A daily cadence means yesterday's empties were
        // caught by yesterday's sweep; this one clears when today added
        // none, so the packet does not treadmill.
        let since = job
            .get("opened_on")
            .and_then(Value::as_str)
            .unwrap_or("1970-01-01")
            .to_string();
        let stamp = job
            .pointer("/metadata/opened_at")
            .and_then(Value::as_str)
            .unwrap_or(since.as_str())
            .to_string();

        let step_types = self.get("/api/jobs/step-types").await?;
        let approval = approval_kinds(&step_type_rows(&step_types)?);

        // The warm packets: open Jobs whose approval steps have completed
        // are the ones still worth asking the approver about.
        let open = self.get("/api/jobs?status=open&limit=1000").await?;
        let open = listing_rows(&open, "the open-packet read (GET /api/jobs?status=open)")?;
        let findings = empty_approval_decisions(&open, &approval, &since);

        // Complete the Inspect checklist. Its own fields are `findings`
        // and `measured`; the checklist bundle wants `items`. One item
        // per lost decision (or a single clean item), stamped with the
        // sweep's own open instant — the handler takes no wall clock.
        let items: Vec<Value> = if findings.is_empty() {
            vec![json!({
                "label": format!("no empty approval decisions since {since}"),
                "checked": true,
                "checked_by": actor,
                "checked_at": stamp,
            })]
        } else {
            findings
                .iter()
                .map(|f| {
                    json!({
                        "label": format!("empty decision: {} (job {}, step {})", f.title, f.job_id, f.step_id),
                        "checked": true,
                        "checked_by": actor,
                        "checked_at": stamp,
                    })
                })
                .collect()
        };
        let finding_lines: Vec<String> = findings
            .iter()
            .map(|f| format!("{}/{}: {}", f.job_id, f.step_id, f.title))
            .collect();
        let measured = format!(
            "{} empty approval decision(s) among open packets since {since}",
            findings.len()
        );
        self.complete(
            ctx,
            &ev,
            !findings.is_empty(),
            findings_text(&finding_lines),
            measured,
            items,
        )
        .await
    }
}

impl MaintenanceSweepInspect {
    /// The two writes every target shares. Route FIRST: the
    /// Clear/Remediate predicates read `job.metadata.action_needed`, so
    /// it must be set before the Inspect completion re-evaluates them
    /// (PATCH merges top-level keys). Then complete the checklist with
    /// `findings`, `measured` and its `items`.
    async fn complete(
        &self,
        ctx: &InvocationContext,
        ev: &StepEvent<'_>,
        action_needed: bool,
        findings: String,
        measured: String,
        items: Vec<Value>,
    ) -> Result<(), HandlerError> {
        let base = self.jobs_base.trim_end_matches('/');
        let action_needed = if action_needed { "true" } else { "false" };
        let patch_url = format!("{base}/api/jobs/{}/metadata", ev.job_id);
        let resp = self
            .client
            .patch(&patch_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({ "action_needed": action_needed }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PATCH {patch_url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PATCH {patch_url} returned {st}: {body}"
            )));
        }

        let put_url = format!("{base}/api/jobs/{}/steps/{}", ev.job_id, ev.step_id);
        let resp = self
            .client
            .put(&put_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&inspection_put_body(findings, measured, items))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {put_url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {put_url} returned {st}: {body}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Backlog d4698bc2: `data_rows` read an error body as zero rows —
    /// zero trains, zero approval kinds, zero open packets — and the
    /// sweep CLEARED with "no empty approval decisions" on a read that
    /// saw nothing. Each of the three reads now refuses by name.
    #[test]
    fn a_read_that_answered_no_rows_refuses_rather_than_clearing_the_sweep() {
        let bad = json!({ "error": "narrowed" });
        let why = |r: Result<Vec<Value>, HandlerError>| match r {
            Err(HandlerError::Downstream(why)) => why,
            other => panic!("a bad answer is a retryable refusal, got {other:?}"),
        };
        assert!(why(listing_rows(&bad, "the open-packet read")).contains("the open-packet read"));
        assert!(why(step_type_rows(&bad)).contains("step-types"));
        assert_eq!(
            listing_rows(&json!({ "data": [], "total": 0 }), "x").unwrap(),
            Vec::<Value>::new(),
            "an empty listing is an answer"
        );
        assert_eq!(
            step_type_rows(&json!([{ "kind": "sign-off" }]))
                .unwrap()
                .len(),
            1,
            "step-types is a bare array, and that is its answer"
        );
    }

    /// Backlog f2eac973, AT THE CONSUMING LAYER. The job read unwrapped
    /// an envelope the jobs API never sends and fell back to the whole
    /// body, so a 200 answer that was not a job failed the kind check
    /// and the Inspect step was skipped with `Ok(())` — the sweep sat
    /// ready and nothing said why. It is now a retryable refusal naming
    /// the read.
    #[tokio::test]
    async fn a_job_read_that_answered_no_row_refuses_rather_than_skipping() {
        use axum::{Json as AxJson, Router, routing::get};
        let app = Router::new().route(
            "/api/jobs/{id}",
            get(|| async { AxJson(json!({ "error": "forbidden" })) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let handler =
            MaintenanceSweepInspect::with_client(reqwest::Client::new(), format!("http://{addr}"));
        let ctx = InvocationContext {
            rule_name: "inspect-empty-decisions-sweep-on-step-ready".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.ready.checklist".into(),
            event_payload: json!({
                "job_id": "j-sweep",
                "step_id": "s-inspect",
                "kind": "checklist",
                "metadata": {},
            }),
        };
        match handler.invoke(&[], &ctx).await {
            Err(HandlerError::Downstream(why)) => {
                assert!(why.contains("/api/jobs/j-sweep"), "{why}");
                assert!(why.contains("no row"), "{why}");
            }
            other => panic!("a body that is not a job is a refusal, got {other:?}"),
        }
    }

    fn kinds() -> BTreeSet<String> {
        ["sign-off".to_string()].into_iter().collect()
    }
    fn job(steps: Value) -> Value {
        json!({ "id": "j1", "steps": steps })
    }
    fn step(over: Value) -> Value {
        let mut base = json!({
            "id": "s1", "title": "Approve", "kind": "sign-off",
            "status": "completed", "completed_on": "2026-09-05", "metadata": {}, "notes": ""
        });
        if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
            for (k, v) in o {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn an_empty_sign_off_is_a_lost_decision() {
        let jobs = vec![job(json!([step(json!({}))]))];
        let found = empty_approval_decisions(&jobs, &kinds(), "2026-09-01");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].step_id, "s1");
    }

    #[test]
    fn only_materialization_keys_still_counts_as_empty() {
        let jobs = vec![job(json!([step(json!({
            "metadata": { "authority_role": "platform-admin", "outcome_kind": "completed", "completed_at": "2026-09-05T00:00:00Z" }
        }))]))];
        assert_eq!(
            empty_approval_decisions(&jobs, &kinds(), "2026-09-01").len(),
            1
        );
    }

    #[test]
    fn a_recorded_decision_is_not_flagged() {
        let jobs = vec![job(json!([step(json!({
            "metadata": { "decision": "approved", "reason": "ships clean" }
        }))]))];
        assert!(empty_approval_decisions(&jobs, &kinds(), "2026-09-01").is_empty());
    }

    #[test]
    fn notes_alone_count_as_a_recorded_decision() {
        let jobs = vec![job(json!([step(
            json!({ "notes": "approved on the call" })
        )]))];
        assert!(empty_approval_decisions(&jobs, &kinds(), "2026-09-01").is_empty());
    }

    #[test]
    fn a_non_approval_kind_is_ignored() {
        let jobs = vec![job(json!([step(json!({ "kind": "task" }))]))];
        assert!(empty_approval_decisions(&jobs, &kinds(), "2026-09-01").is_empty());
    }

    #[test]
    fn an_incomplete_step_is_ignored() {
        let jobs = vec![job(json!([step(json!({ "status": "ready" }))]))];
        assert!(empty_approval_decisions(&jobs, &kinds(), "2026-09-01").is_empty());
    }

    #[test]
    fn a_decision_before_the_window_is_ignored() {
        let jobs = vec![job(json!([step(json!({ "completed_on": "2026-08-20" }))]))];
        assert!(empty_approval_decisions(&jobs, &kinds(), "2026-09-01").is_empty());
    }

    /// The step declares `findings` a string; an array is a 400 at the
    /// step API (2026-09-13, both self-inspections dead-lettered).
    #[test]
    fn findings_are_one_string_the_step_accepts() {
        assert_eq!(findings_text(&[]), "none");
        assert_eq!(
            findings_text(&["a/b: x".to_string(), "c/d: y".to_string()]),
            "a/b: x\nc/d: y"
        );
    }

    /// THE PIN (CLAUDE.md §9a): the body this handler PUTs and the
    /// fields the workflow declares for the step are two homes for one
    /// shape. Read the step off infra/platform/workflows/maintenance-
    /// sweep.toml and validate both targets' bodies against it with the
    /// core's own validator — the check the step API ran when it
    /// refused the array (2026-09-13: eight attempts, two dead letters,
    /// a day of uninspected sweeps).
    #[test]
    fn inspection_bodies_validate_against_the_step_they_complete() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows"
        );
        let specs = boss_jobs::seed_loader::load_workflows(dir).expect("the platform bundle loads");
        let sweep = specs
            .iter()
            .find(|w| w.kind == "maintenance-sweep")
            .expect("maintenance-sweep is a platform protocol");
        let inspect = sweep
            .steps
            .iter()
            .find(|st| st.title == "inspect")
            .expect("the sweep has an inspect step");
        assert!(
            !inspect.fields.is_empty(),
            "the inspect step declares fields"
        );
        let stamp = "2026-09-13T00:00:00Z";
        let item = |label: &str| json!({"label": label, "checked": true, "checked_by": "automation:t", "checked_at": stamp});
        // empty-decisions, both with and without findings
        for lines in [vec![], vec!["j1/s1: Approve".to_string()]] {
            let body = inspection_put_body(
                findings_text(&lines),
                "1 empty approval decision(s) among open packets since 2026-09-12".into(),
                vec![item("x")],
            );
            boss_jobs::step_registry::StepRegistry::validate_authored_fields(
                &inspect.fields,
                &body["metadata"],
            )
            .unwrap_or_else(|e| panic!("the step API would refuse this body: {e:?}"));
        }
        // deploy-convergence, from its own inspection
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-13T20:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let insp = super::super::sweep_deploy_convergence::inspect(&[], now, "automation:t");
        let body = inspection_put_body(findings_text(&insp.findings), insp.measured, insp.items);
        boss_jobs::step_registry::StepRegistry::validate_authored_fields(
            &inspect.fields,
            &body["metadata"],
        )
        .unwrap_or_else(|e| panic!("the step API would refuse the convergence body: {e:?}"));
        // And the shape that failed live is refused here too.
        let bad = json!({"findings": ["a"], "measured": "m", "items": [item("x")]});
        assert!(
            boss_jobs::step_registry::StepRegistry::validate_authored_fields(&inspect.fields, &bad)
                .is_err(),
            "an array for findings must be refused, as the step API refused it"
        );
    }

    #[test]
    fn a_rule_without_a_target_inspects_empty_decisions_as_before() {
        assert_eq!(wanted_target(&[]).unwrap(), "empty-decisions");
    }

    #[test]
    fn a_rule_names_the_target_it_inspects() {
        use boss_dispatcher::rules::expr::Value as V;
        let args = vec![("target".to_string(), V::String("deploy-convergence".into()))];
        assert_eq!(wanted_target(&args).unwrap(), "deploy-convergence");
        let bad = vec![("target".to_string(), V::Int(3))];
        assert!(matches!(
            wanted_target(&bad),
            Err(HandlerError::BadArgType { .. })
        ));
    }

    #[test]
    fn approval_kinds_reads_the_registry_surface() {
        let types = vec![
            json!({ "kind": "sign-off", "surface": "approval" }),
            json!({ "kind": "task", "surface": "default" }),
            json!({ "kind": "review-design", "surface": "approval" }),
        ];
        let k = approval_kinds(&types);
        assert!(k.contains("sign-off") && k.contains("review-design") && !k.contains("task"));
    }
}
