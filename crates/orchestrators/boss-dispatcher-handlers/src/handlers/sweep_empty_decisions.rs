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

use async_trait::async_trait;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

use super::common::{
    StepEvent, api_client, dispatcher_actor_header, dispatcher_reader_header, sim_origin_value,
};

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

fn data_rows(v: &Value) -> Vec<Value> {
    v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| v.as_array().cloned())
        .unwrap_or_default()
}

#[async_trait]
impl Handler for MaintenanceSweepInspect {
    fn name(&self) -> &'static str {
        "maintenance.sweep.inspect"
    }

    async fn invoke(
        &self,
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        // Cheap filter first — this rule rides the shared `step.ready.*`
        // subscription, so it fires on every ready step. The Inspect
        // step is the only `checklist` a sweep has; skip everything else
        // before spending a fetch.
        if ev.kind != "checklist" {
            return Ok(());
        }
        let base = self.jobs_base.trim_end_matches('/');

        let job = self.get(&format!("/api/jobs/{}", ev.job_id)).await?;
        let job = job.get("data").cloned().unwrap_or(job);
        if job.get("kind").and_then(Value::as_str) != Some("maintenance-sweep") {
            return Ok(());
        }
        let target = job
            .pointer("/metadata/target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if target != "empty-decisions" {
            // Other targets (disk, images, conformance) inspect the
            // forge/cluster and belong to the ops-runner path, not this
            // SoR-scanning handler.
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
        let approval = approval_kinds(&data_rows(&step_types));

        // The warm packets: open Jobs whose approval steps have completed
        // are the ones still worth asking the approver about.
        let open = self.get("/api/jobs?status=open&limit=1000").await?;
        let findings = empty_approval_decisions(&data_rows(&open), &approval, &since);

        let action_needed = if findings.is_empty() { "false" } else { "true" };

        // Route FIRST: the Clear/Remediate predicates read
        // `job.metadata.action_needed`, so it must be set before the
        // Inspect completion re-evaluates them. PATCH merges top-level
        // keys.
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

        // Complete the Inspect checklist. Its own fields are `findings`
        // and `measured`; the checklist bundle wants `items`. One item
        // per lost decision (or a single clean item), stamped with the
        // sweep's own open instant — the handler takes no wall clock.
        let actor = format!("automation:{}", self.name());
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
        let findings_list: Vec<Value> = findings
            .iter()
            .map(|f| json!(format!("{}/{}: {}", f.job_id, f.step_id, f.title)))
            .collect();
        let measured = format!(
            "{} empty approval decision(s) among open packets since {since}",
            findings.len()
        );
        let put_url = format!("{base}/api/jobs/{}/steps/{}", ev.job_id, ev.step_id);
        let resp = self
            .client
            .put(&put_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({
                "status": "completed",
                "metadata": {
                    "findings": findings_list,
                    "measured": measured,
                    "items": items,
                },
            }))
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
