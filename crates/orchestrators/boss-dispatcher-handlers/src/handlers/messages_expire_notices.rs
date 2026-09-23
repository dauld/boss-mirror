//! `messages.expire_notices` — retire the notices the step notifier
//! sent, once the thing they announced has ended.
//!
//! Backlog 0b2bac00 (2026-09-23): `messages.notify` sends an
//! assignee's ready/assigned notice as a `direct`, and
//! `messages.expire_for_job` retires unread SIGNALS only, so a notice
//! outlived its step for good. Measured for emp-david over
//! 2026-09-16T23:54Z..2026-09-23T16:40Z: 90 direct notices, 1 read; of
//! the 89 steps readable, 83 were completed and 1 skipped, and all 84
//! still counted in his badge and his "waiting on you" headline.
//!
//! Which notices: the ones whose id carries the notifier's
//! [`DEFAULT_ID_PREFIX`] — `notify:{step}:{recipient}`. The prefix is
//! read from the notifier's own constant rather than a rule argument,
//! so the sender and the retirer cannot name different notices. A
//! person's direct (a minted id) and the `done:` announcement a step's
//! END sends (its own prefix) never match.
//!
//! Which entity, read off the event: a step event names its step
//! (`jobs.step.updated` always carries `job_id` and `step_id` —
//! `step_state_payload` inserts the latter), so the step's own notices
//! retire; `jobs.job.closed` names only the job (`id`, roster in
//! migration 137 — no `step_id`), so every notice under it retires.
//! The second is the backstop for a step that never ends because its
//! job closed around it.

use super::common::post_json;
use super::messages_notify::DEFAULT_ID_PREFIX;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

pub struct MessagesExpireNotices {
    client: reqwest::Client,
    messages_base: String,
}

impl MessagesExpireNotices {
    pub fn new(messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            messages_base: messages_base.into(),
        })
    }

    pub fn with_client(client: reqwest::Client, messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            messages_base: messages_base.into(),
        })
    }
}

/// The entity path whose notices are past relevancy, read off the
/// event. `None` when the payload names neither a step nor a job.
fn entity_path(payload: &serde_json::Value) -> Option<String> {
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    match (field("job_id"), field("step_id")) {
        (Some(job), Some(step)) => Some(format!("/jobs/{job}/steps/{step}")),
        // No trailing slash: `/jobs/{id}` catches the notices under
        // every step of the job, and uuids rule out one job id being a
        // prefix of another.
        _ => field("id").map(|job| format!("/jobs/{job}")),
    }
}

#[async_trait]
impl Handler for MessagesExpireNotices {
    fn name(&self) -> &'static str {
        "messages.expire_notices"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // Permanent, not Downstream: a payload that names nothing fails
        // identically on every redelivery.
        let path = entity_path(&ctx.event_payload).ok_or_else(|| {
            HandlerError::Permanent(format!(
                "{} carried neither job_id+step_id nor id",
                ctx.triggering_topic
            ))
        })?;
        let url = format!(
            "{}/api/messages/expire",
            self.messages_base.trim_end_matches('/')
        );
        // The colon is part of the prefix: `notify` alone would also
        // match an id minted `notifyX:…` by some later rule.
        post_json(
            &self.client,
            &url,
            &serde_json::json!({
                "entity_path_prefix": path,
                "id_prefix": format!("{DEFAULT_ID_PREFIX}:"),
            }),
            &ctx.rule_name,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Captured = Arc<Mutex<Option<serde_json::Value>>>;

    /// A stand-in for boss-messages that records the expire body.
    async fn mock_messages() -> (String, Captured) {
        use axum::{Json, Router, routing::post};
        let captured: Captured = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        let app = Router::new().route(
            "/api/messages/expire",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    Json(serde_json::json!({ "expired": 1 }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), captured)
    }

    fn ctx(topic: &str, payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "expire-notices-on-step-ended".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: topic.into(),
            event_payload: payload,
        }
    }

    /// The step's own notices, named by the notifier's own prefix.
    #[tokio::test]
    async fn a_step_event_retires_that_steps_notices() {
        let (base, captured) = mock_messages().await;
        let h = MessagesExpireNotices::with_client(reqwest::Client::new(), base);
        h.invoke(
            &[],
            &ctx(
                "jobs.step.updated",
                serde_json::json!({
                    "id": "step-1", "step_id": "step-1", "job_id": "job-1",
                    "status": "completed",
                }),
            ),
        )
        .await
        .unwrap();
        let body = captured.lock().unwrap().clone().expect("POSTed");
        assert_eq!(body["entity_path_prefix"], "/jobs/job-1/steps/step-1");
        assert_eq!(body["id_prefix"], "notify:");
    }

    /// The backstop: a job that closes around a step that never ended.
    #[tokio::test]
    async fn a_job_closed_event_retires_every_notice_under_the_job() {
        let (base, captured) = mock_messages().await;
        let h = MessagesExpireNotices::with_client(reqwest::Client::new(), base);
        h.invoke(
            &[],
            &ctx(
                "jobs.job.closed",
                serde_json::json!({ "id": "job-1", "kind": "backlog-item", "parent_step_id": null }),
            ),
        )
        .await
        .unwrap();
        let body = captured.lock().unwrap().clone().expect("POSTed");
        assert_eq!(body["entity_path_prefix"], "/jobs/job-1");
        assert_eq!(body["id_prefix"], "notify:");
    }

    #[tokio::test]
    async fn a_payload_naming_nothing_is_permanent() {
        let h = MessagesExpireNotices::with_client(reqwest::Client::new(), "http://127.0.0.1:1");
        let err = h
            .invoke(&[], &ctx("jobs.step.updated", serde_json::json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    }
}
