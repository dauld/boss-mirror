//! `jobs.spawn` handler — the load-bearing event-routed spawn handler.
//!
//! Spawning a Job in reaction to an event is a data-driven rule, not a
//! hardcoded branch: the rule registry composes this handler, which
//! turns args into the body of `POST /api/jobs` and stamps actor
//! provenance per the design D2 actor model.
//!
//! Rule shape:
//! ```toml
//! do = [{ handler = "jobs.spawn", args = {
//!   kind = "\"ingredient-restock\"",
//!   subject_kind = "\"vendor\"",
//!   subject = "vendor_for(part_sku)",
//! }}]
//! ```
//!
//! Required args: `kind`, `subject_kind`, `subject`.
//! Optional args: `title` (defaults to "Auto-spawn from rule
//! `<rule-name>`"), `priority` (defaults to "normal"),
//! `parent_step_id` (the delegate-subjob parent — D7).
//!
//! ## Delegate-subjob linkage (D7)
//!
//! When `parent_step_id` is supplied (the `step.ready.delegate-subjob`
//! rule passes it), the handler:
//!   1. Stamps `parent_step_id` into the spawned (child) Job's
//!      `metadata` — the reverse link the `jobs.subjob_resolve` handler
//!      reads to find the parent step when the child closes.
//!   2. After the spawn POST succeeds, reads the new Job's `id` from
//!      the response and `PUT`s the parent step's `embedded_job` to it
//!      — the forward `Step.embedded_job` link the SPA descends into.
//! The parent *Job* id needed for that PUT comes from the triggering
//! event payload's `job_id` (the `step.ready` marker carries it).

use super::expr::Value;
use super::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct JobsSpawn {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsSpawn {
    /// Construct with the jobs-api base URL (e.g. `http://127.0.0.1:7900`).
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests use this to point
    /// at a wiremock server; production passes a fresh client).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }
}

#[async_trait]
impl Handler for JobsSpawn {
    fn name(&self) -> &'static str {
        "jobs.spawn"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let subject_kind = arg_string(args, "subject_kind")?;
        let subject = arg_string(args, "subject")?;

        // Optional delegate-subjob (D7) parent link. When present the
        // spawned Job is a child of a delegate-subjob step; we stamp
        // the reverse link into its metadata and later set the parent
        // step's forward `embedded_job` pointer.
        let parent_step_id = match arg(args, "parent_step_id") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                return Err(HandlerError::BadArgType {
                    arg: "parent_step_id".to_string(),
                    expected: "string",
                    got: other.kind(),
                });
            }
        };

        // Build the actor identity per the rule-as-actor model. This
        // string lands in audit_log via the gateway's x-boss-user
        // header → jobs-api → events.JOB_OPENED.
        let actor_id = format!("rule:{}", ctx.rule_name);
        let user_header = json!({
            "id": actor_id,
            "role": "system",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();

        let mut metadata = json!({
            "spawned_by_rule": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "triggered_by_topic": ctx.triggering_topic,
        });
        if let (Some(psid), Some(map)) = (&parent_step_id, metadata.as_object_mut()) {
            // The reverse link: jobs.subjob_resolve gates on this key's
            // presence and uses it to PUT the parent step back to done.
            map.insert("parent_step_id".to_string(), json!(psid));
            // Also stamp the parent *Job* id (carried on the triggering
            // `step.ready` payload) so the resolve handler can address
            // PUT /api/jobs/{parent_job}/steps/{parent_step} without a
            // search — the child knows both ends of the link.
            if let Some(parent_job_id) = ctx.event_payload.get("job_id").and_then(|v| v.as_str()) {
                map.insert("parent_job_id".to_string(), json!(parent_job_id));
            }
        }
        // Merge any `metadata.<field>` args into the Job metadata so a rule
        // can parameterize the spawned Job — e.g. the reorder rule passes
        // `metadata.part_sku` so the restock buys one ingredient and the
        // per-SKU dedup can match an in-flight restock for that SKU.
        if let Some(map) = metadata.as_object_mut() {
            for (k, v) in args {
                if let Some(field) = k.strip_prefix("metadata.") {
                    let jv = match v {
                        Value::String(s) => json!(s),
                        Value::Int(i) => json!(i),
                        Value::Bool(b) => json!(b),
                        _ => continue,
                    };
                    map.insert(field.to_string(), jv);
                }
            }
        }

        // The docstring always promised an optional `title` arg; the
        // body ignored it until the design-review spawn needed one (a
        // review titled "Auto-spawn from rule …" in an operator's
        // queue is a label, not a title).
        let title = match arg(args, "title") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => format!("Auto-spawn from rule {}", ctx.rule_name),
        };
        let body = json!({
            "kind": kind,
            "subject": {
                "subject_kind": subject_kind,
                "id": subject,
            },
            "title": title,
            "owner_id": actor_id,
            "priority": "standard",
            "status": "open",
            "metadata": metadata,
            "tags": ["dispatcher-spawned"]
        });

        let url = format!("{}/api/jobs", self.jobs_base.trim_end_matches('/'));
        // The spawned Job inherits the triggering event's sim-ness.
        // Without this header jobs-api sees no sim chain and records
        // the Job as real — and because a Job's `simulated` bit is
        // immutable once written, a restock spawned by the simulated
        // brewery became a permanent real Job that the epoch trim
        // would never collect.
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-boss-user", user_header.as_str())
            .header("x-sim-origin", crate::dispatcher::sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {body}"
            )));
        }

        // D7 forward link: set the parent step's `embedded_job` to the
        // Job we just created so traversal descends from the
        // delegate-subjob step into the child's step graph. Skipped
        // entirely for ordinary spawns (no parent_step_id arg).
        if let Some(parent_step_id) = parent_step_id {
            // Parse `{ "id": "<uuid>" }` out of the create-job response.
            let created: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| HandlerError::Downstream(format!("spawn response not JSON: {e}")))?;
            let child_job_id = created
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HandlerError::Downstream("spawn response missing `id`".to_string()))?
                .to_string();

            // The parent *Job* id rides on the triggering event payload
            // (`step.ready.<kind>` carries `job_id`); we need it to
            // address the PUT /api/jobs/{job}/steps/{step}.
            let parent_job_id = ctx
                .event_payload
                .get("job_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    HandlerError::Downstream(
                        "step.ready payload missing job_id; cannot link parent step".to_string(),
                    )
                })?;

            // PATCH-on-PUT: send only `embedded_job`; the jobs-api
            // overlays it onto the current step row and keeps every
            // other field intact (status stays Ready — the step is
            // *waiting* on the child, not done yet).
            let step_url = format!(
                "{}/api/jobs/{}/steps/{}",
                self.jobs_base.trim_end_matches('/'),
                parent_job_id,
                parent_step_id,
            );
            let put_body = json!({ "embedded_job": child_job_id });
            let put_resp = self
                .client
                .put(&step_url)
                .header("Content-Type", "application/json")
                .header("x-boss-user", user_header.as_str())
                .header("x-sim-origin", crate::dispatcher::sim_origin_value())
                .json(&put_body)
                .send()
                .await
                .map_err(|e| HandlerError::Downstream(format!("PUT {step_url}: {e}")))?;
            if !put_resp.status().is_success() {
                let status = put_resp.status();
                let put_text = put_resp.text().await.unwrap_or_default();
                return Err(HandlerError::Downstream(format!(
                    "PUT {step_url} returned {status}: {put_text}"
                )));
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::Value;
    use super::*;

    #[tokio::test]
    async fn rejects_missing_kind_arg() {
        let h = JobsSpawn::new("http://127.0.0.1:1");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "x".into(),
            event_payload: serde_json::json!({}),
        };
        let res = h
            .invoke(
                &[("subject_kind".to_string(), Value::String("vendor".into()))],
                &ctx,
            )
            .await;
        assert!(matches!(res, Err(HandlerError::MissingArg(_))));
    }

    #[tokio::test]
    async fn rejects_wrong_type_arg() {
        let h = JobsSpawn::new("http://127.0.0.1:1");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "x".into(),
            event_payload: serde_json::json!({}),
        };
        let res = h
            .invoke(
                &[
                    ("kind".to_string(), Value::Int(42)),
                    ("subject_kind".to_string(), Value::String("vendor".into())),
                    ("subject".to_string(), Value::String("vnd-1".into())),
                ],
                &ctx,
            )
            .await;
        assert!(matches!(res, Err(HandlerError::BadArgType { .. })));
    }
}
