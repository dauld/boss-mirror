//! `jobs.subjob_resolve` — the D7 delegate-subjob write-back.
//!
//! Fires when a *child* Job (one spawned by a `delegate-subjob` step)
//! closes. The dispatcher rule listens on `jobs.job.closed` and gates
//! on the closing Job carrying a `parent_step_id` in its metadata
//! (set by `jobs.spawn` when the Job was delegated). This handler then
//! resolves the parent step:
//!
//!   1. `GET /api/jobs/{child}` → read the child's
//!      `metadata.parent_step_id`, `metadata.parent_job_id`, and the
//!      terminal `metadata.outcome`.
//!   2. `PATCH /api/jobs/{parent_job}/steps/{parent_step}/metadata`
//!      with the child outcome as `subjob_outcome` — the step merge
//!      door, which keeps every key it is not sent.
//!   3. `PUT /api/jobs/{parent_job}/steps/{parent_step}` with
//!      `status = "completed"` and nothing else (backlog e39a9d2a: the
//!      parent was read and its whole metadata PUT back until the step
//!      PUT's refusal of a metadata body made that the wrong door).
//!
//! Completing the parent step drives the parent Job's own re-eval (the
//! delegate-subjob step's downstream predicates can now flip), closing
//! the D6/D7 loop: spawn-on-ready → run child → resolve-on-close.
//!
//! No args are required — everything the handler needs rides on the
//! child Job it can fetch from the close event's `id`. The optional
//! `outcome_metadata_key` arg overrides which child-metadata key holds
//! the terminal outcome (defaults to `outcome`, what
//! `close_job_on_terminal` stamps).

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

use super::common::{complete_step, dispatcher_reader_header, sim_origin_value};

pub struct JobsSubjobResolve {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsSubjobResolve {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// wiremock server).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    async fn get_job(&self, job_id: &str) -> Result<serde_json::Value, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} response not JSON: {e}")))
    }
}

#[async_trait]
impl Handler for JobsSubjobResolve {
    fn name(&self) -> &'static str {
        "jobs.subjob_resolve"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // The `jobs.job.closed` payload carries the closing Job's id.
        let child_id = ctx
            .event_payload
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("job.closed payload missing `id`".to_string()))?
            .to_string();

        // Fetch the child Job. `GET /api/jobs/{id}` flattens the Job
        // fields and adds a `steps` array; the fields we need live at
        // the top level under `metadata`.
        let child = self.get_job(&child_id).await?;
        let child_meta = child.get("metadata").cloned().unwrap_or(json!({}));

        // Not a delegated child → nothing to resolve. The rule's `when`
        // already gates on this; the handler re-checks so a mis-wired
        // rule degrades to a no-op rather than a spurious PUT.
        let Some(parent_step_id) = child_meta.get("parent_step_id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let parent_job_id = child_meta
            .get("parent_job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "child Job {child_id} has parent_step_id but no parent_job_id"
                ))
            })?;

        // The child's terminal outcome — what we write back. Prefer the
        // child Job metadata (stamped by close_job_on_terminal); fall
        // back to the close event payload's top-level `outcome`.
        let outcome_key = match arg(args, "outcome_metadata_key") {
            Some(Value::String(s)) => s.as_str(),
            _ => "outcome",
        };
        let outcome = child_meta
            .get(outcome_key)
            .and_then(|v| v.as_str())
            .or_else(|| ctx.event_payload.get("outcome").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        // Complete the parent step: `subjob_outcome` through the step
        // merge door, then the status alone (backlog e39a9d2a). This
        // read the parent step's metadata, merged the outcome in and
        // PUT the whole map back, because the PUT replaces metadata
        // wholesale — correct only while nothing wrote the step
        // between the read and the PUT, and refused outright under the
        // decided end state. The merge door keeps every key it is not
        // sent, so the parent is not read at all.
        let mut fields = serde_json::Map::new();
        fields.insert("subjob_outcome".to_string(), json!(outcome));
        complete_step(
            &self.client,
            self.jobs_base.trim_end_matches('/'),
            parent_job_id,
            parent_step_id,
            fields,
            &ctx.rule_name,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "resolve-subjob-on-close".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    /// THE OUTCOME THROUGH THE MERGE DOOR, THEN THE FLIP (backlog
    /// e39a9d2a, stage 2 of design 93d2bddb). This read the parent
    /// step's metadata, added `subjob_outcome` and PUT the whole map
    /// back with the status, so a key written to the parent step
    /// between that read and the PUT was refused 409 (stage 1) — and
    /// under the decided end state every such PUT is. Now it sends the
    /// one key it owns through the step merge door and PUTs the status
    /// alone; the parent is not read at all.
    #[tokio::test]
    async fn the_outcome_goes_through_the_merge_door_and_the_flip_carries_no_metadata() {
        use crate::handlers::listing_stub::serve;
        // No parent fixture: a read of the parent would 404 and fail
        // the firing, so passing proves the parent is not read.
        let stub = serve(vec![(
            "/api/jobs/child-1",
            json!({ "id": "child-1", "metadata": {
                "parent_job_id": "parent-1",
                "parent_step_id": "ps-1",
                "outcome": "approved",
            }}),
        )])
        .await;
        JobsSubjobResolve::new(&stub.base)
            .invoke(&[], &ctx(json!({ "id": "child-1" })))
            .await
            .expect("both writes answered");
        assert_eq!(
            stub.sent(),
            vec![
                (
                    "PATCH /api/jobs/parent-1/steps/ps-1/metadata".to_string(),
                    json!({ "subjob_outcome": "approved" }),
                ),
                (
                    "PUT /api/jobs/parent-1/steps/ps-1".to_string(),
                    json!({ "status": "completed" }),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn noop_when_close_payload_missing_id() {
        let h = JobsSubjobResolve::new("http://127.0.0.1:1");
        let res = h
            .invoke(&[], &ctx(json!({ "closed_on": "2026-06-01" })))
            .await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }
}
