//! `jobs.clear_waiting` — a closed Job wakes its waiters (e9291570).
//!
//! Fires on `jobs.job.closed` (flat payload: the closing Job's `id`).
//! Waiters declared their block by writing `metadata.waiting_on` (the
//! `('*', 'waiting_on')` job edge, migration 110); this handler finds
//! them with the jobs API's `?waiting_on=` filter — prefix-aware, so
//! a waiter that wrote an 8-char prefix still wakes — and clears the
//! key on each. The clear goes through `PUT /api/jobs/{id}`, whose
//! update path re-evaluates metadata-gated steps (aa9980c8), so a
//! step whose `ready_when` references `job.metadata.waiting_on`
//! becomes ready in the same write. Clearing writes `""` rather than
//! deleting the key: the edge guard resolves the empty string
//! trivially, and boards read "" as no wait.
//!
//! Idempotent by construction: re-delivery finds no remaining waiters
//! (the filter matches only jobs still carrying the value) and no-ops.

use super::common::{dispatcher_actor_header, dispatcher_reader_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::json;
use std::sync::Arc;

pub struct JobsClearWaiting {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsClearWaiting {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local server).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }
}

#[async_trait]
impl Handler for JobsClearWaiting {
    fn name(&self) -> &'static str {
        "jobs.clear_waiting"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let Some(blocker_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            // Malformed payload: nothing to retry into existence.
            return Ok(());
        };

        let base = self.jobs_base.trim_end_matches('/');
        let list_url = format!("{base}/api/jobs?waiting_on={blocker_id}&status=open&limit=100");
        let resp = self
            .client
            .get(&list_url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {list_url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {list_url} returned {status}: {body}"
            )));
        }
        let listing: serde_json::Value = resp.json().await.map_err(|e| {
            HandlerError::Downstream(format!("GET {list_url} response not JSON: {e}"))
        })?;
        // A listing with no `data` array is NO ANSWER. Read as zero
        // waiters it ACKed the close — the one event that will ever wake
        // them — and every waiter stayed blocked (backlog 37fc5837).
        let waiters: Vec<serde_json::Value> =
            super::common::rows_or_refuse(&listing, "the waiter read (GET /api/jobs?waiting_on=)")
                .map_err(HandlerError::Downstream)?;

        for waiter in waiters {
            let Some(id) = waiter.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            // PUT the full Job back with only `waiting_on` cleared —
            // the update endpoint takes the whole row, and jobs that
            // vanished between list and write just 404 into the error
            // path for a retry.
            let mut job = waiter.clone();
            if let Some(meta) = job.get_mut("metadata").and_then(|m| m.as_object_mut()) {
                meta.insert("waiting_on".to_string(), json!(""));
            } else {
                continue;
            }
            let put_url = format!("{base}/api/jobs/{id}");
            let resp = self
                .client
                .put(&put_url)
                .header("content-type", "application/json")
                .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
                .header("x-sim-origin", sim_origin_value())
                .json(&job)
                .send()
                .await
                .map_err(|e| HandlerError::Downstream(format!("PUT {put_url}: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(HandlerError::Downstream(format!(
                    "PUT {put_url} returned {status}: {body}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "jobs-clear-waiting-on".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    #[tokio::test]
    async fn noop_when_close_payload_missing_id() {
        let h = JobsClearWaiting::new("http://127.0.0.1:1");
        // No id → no-op Ok, NOT a downstream error: there is nothing
        // a redelivery could find, so erroring would retry forever.
        let res = h
            .invoke(&[], &ctx(json!({ "closed_on": "2026-08-10" })))
            .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn unreachable_api_is_a_downstream_error_for_redelivery() {
        let h = JobsClearWaiting::new("http://127.0.0.1:1");
        let res = h.invoke(&[], &ctx(json!({ "id": "j-1" }))).await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }

    /// A waiter read with no `data` array is no answer (backlog
    /// 37fc5837). Read as zero waiters it acked the close, and the
    /// close is the only event that will ever wake them — so every
    /// waiter stayed blocked, silently. Refusing redelivers instead.
    #[tokio::test]
    async fn a_waiter_read_with_no_data_array_refuses_by_name() {
        use crate::handlers::listing_stub::{assert_refused_by_name, no_data_array, serve};
        let stub = serve(vec![("/api/jobs?waiting_on=j-1", no_data_array())]).await;
        let h = JobsClearWaiting::new(stub.base.clone());
        let res = h.invoke(&[], &ctx(json!({ "id": "j-1" }))).await;
        assert_refused_by_name(res, "the waiter read");
    }
}
