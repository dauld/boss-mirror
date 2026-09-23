//! `messages.notify` — turn a step lifecycle event into an inbox
//! message ADDRESSED TO SOMEONE.
//!
//! This is the **push** side of the human-powered-state-machine
//! dispatcher. The **pull** side (the `/api/jobs/assignments` My Day
//! query) is what actually drives work; this handler adds awareness.
//!
//! Two events reach a person, and nothing else does:
//!
//! - a step becoming READY **with an assignee** — somebody put it in
//!   front of you, so it sends a `direct`;
//! - a step marked `notify_on_done` COMPLETING — the wait-is-over
//!   announcement, which still resolves a role to its on-call member
//!   and sends a `signal`.
//!
//! A ready step with only an `authority_role` sends NOTHING (David,
//! 2026-08-14: "we aren't ready for human on-call yet"). It is still
//! routed — it sits in the role's pull queue — but nobody is paged for
//! a duty that does not exist. A step with neither an assignee nor a
//! role was always a no-op.

use super::common::{
    StepEvent, dispatcher_actor_header, dispatcher_reader_header, sim_origin_value,
};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// The id prefix of the notice a step's READY or ASSIGNED event sends —
/// `notify:{step}:{recipient}` — when the rule passes no `id_prefix`.
/// One constant because `messages.expire_notices` retires exactly these
/// when the step ends (backlog 0b2bac00): the sender and the retirer
/// read the same definition, so the two cannot drift apart.
pub const DEFAULT_ID_PREFIX: &str = "notify";

#[derive(Debug, Deserialize)]
struct EmployeeLite {
    id: String,
}

pub struct MessagesNotify {
    client: reqwest::Client,
    people_base: String,
    messages_base: String,
}

impl MessagesNotify {
    pub fn new(people_base: impl Into<String>, messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            people_base: people_base.into(),
            messages_base: messages_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// mock server; production passes a fresh client).
    pub fn with_client(
        client: reqwest::Client,
        people_base: impl Into<String>,
        messages_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            people_base: people_base.into(),
            messages_base: messages_base.into(),
        })
    }
}

#[async_trait]
impl Handler for MessagesNotify {
    fn name(&self) -> &'static str {
        "messages.notify"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;

        // `id_prefix` (optional rule arg, default "notify"): a step
        // may legitimately notify twice in its life — at READY (this
        // handler's original job) and at DONE (the wait-over signal;
        // rule `notify-on-step-done-marked`). The dedup id must not
        // collapse the two, so the done-rule passes its own prefix.
        let id_prefix = args
            .iter()
            .find(|(k, _)| k == "id_prefix")
            .and_then(|(_, v)| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or(DEFAULT_ID_PREFIX);

        // An ASSIGNEE wins over a role, and a step with neither is the
        // only no-op.
        //
        // This used to key on `authority_role` alone, reasoning that a
        // step without one was "generic — an operator picks it off a
        // queue". That is true of outcome steps and false of the
        // `task` StepType, which is documented as "Simple assigned
        // task for HR, IT, admin" with `required_roles = []`:
        // assignment IS its routing mechanism. So filing a task FOR
        // someone told them nothing, and the manual notification the
        // Job model exists to remove had to be sent by hand.
        //
        // The assignee takes precedence because it is the more
        // specific claim: a role says someone like you should do this,
        // an assignee says you specifically.
        //
        // Measured before changing it: of 39,347 steps, 2,550 carry an
        // assignee and no role, and 2,525 of those are on simulated
        // Jobs — about 14 extra messages per sim-day, in a system
        // already sending thousands. Proportionate, not a storm.
        let recipient_id: Option<String> = ev.assignee_id.map(str::to_string);
        let role = ev
            .metadata
            .get("authority_role")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty());
        if recipient_id.is_none() && role.is_none() {
            return Ok(());
        }

        // NO ON-CALL BROADCAST FOR A READY STEP. David, 2026-08-14,
        // answering approval 934cb22c: "No. We aren't ready for human
        // on-call yet."
        //
        // The fallback resolved a step's `authority_role` to the
        // deterministic on-call member and messaged them, which is
        // where the volume came from: `automation:dispatcher` sent 910
        // messages to one person in two days, and the residue is
        // pipeline steps an AGENT works — every step an agent completes
        // makes the next one ready and pings the human holding the
        // role. Nobody is on call, so the message is addressed to a
        // duty that does not exist yet.
        //
        // Nothing is lost that drives work: the PULL side is the real
        // routing (the `/api/jobs/assignments` role queue — "Up for
        // grabs" on My Day), and a role-only step still sits there.
        // The push was awareness, and awareness aimed at a vacancy is
        // noise.
        //
        // A step.done topic is EXEMPT, and that exemption is why this
        // is a condition rather than a deleted branch.
        // `notify-on-step-done-marked` is opt-in per step
        // (`notify_on_done: true`, which the pr-train Workflow sets on
        // ci / merged / deployed) and it is how an operator learns a
        // wait is over — David asked for it on 2026-08-09. Low volume
        // by construction, and addressed to a real question.
        //
        // Keyed on the TRIGGERING TOPIC, not on the `id_prefix` rule
        // argument. The first cut used the prefix, and the suite caught
        // it: `a_done_topic_announces_done_not_ready` invokes a
        // `step.done.*` topic with no args, so a done announcement
        // would have been suppressed because a DEDUP-ID argument
        // happened to be absent. The topic is what the event IS; the
        // prefix is bookkeeping about message ids, and hanging
        // behaviour off it would be a coincidence rather than a rule.
        let is_done = ctx.triggering_topic.starts_with("step.done.");
        if recipient_id.is_none() && !is_done {
            return Ok(());
        }

        // With an assignee, there is nothing to resolve — that IS the
        // recipient. Only the role path needs a lookup.
        //
        // The two paths also differ in KIND, and that is the whole
        // point of the taxonomy (David, 2026-08-14: "let's make
        // assignment-to-a-person a direct"). An ASSIGNEE means somebody
        // put this step in front of you specifically, which is what
        // `direct` means and what the inbox's default "needs you"
        // filter shows. The role fallback means the machine could not
        // find a person and picked the on-call member of a role, which
        // is a `signal` — true, useful, and not addressed to anyone.
        //
        // Measured why it matters: the admin's inbox held 1,980 unread
        // signals against 3 unread directs, so anything arriving as a
        // signal is invisible by default. The first `approval` packets
        // were assigned to a person and still landed as signals, which
        // meant a question asked of him did not appear where he looks.
        let (recipient, waiting_on, kind) = match (&recipient_id, role) {
            (Some(id), _) => (id.clone(), format!("assigned to {id}"), "direct"),
            (None, Some(r)) => {
                // Resolve the role to its active members; notify the
                // deterministic on-call member (lowest id), mirroring
                // the assignment pick so the recipient is stable.
                let people_url = format!(
                    "{}/api/people?role={}&status=active",
                    self.people_base.trim_end_matches('/'),
                    r,
                );
                let resp = self
                    .client
                    .get(&people_url)
                    .header("x-boss-user", dispatcher_reader_header())
                    .header("x-sim-origin", sim_origin_value())
                    .send()
                    .await
                    .map_err(|e| HandlerError::Downstream(format!("GET {people_url}: {e}")))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(HandlerError::Downstream(format!(
                        "GET {people_url} returned {status}: {body}"
                    )));
                }
                let mut emps: Vec<EmployeeLite> = resp.json().await.map_err(|e| {
                    HandlerError::Downstream(format!("people response not JSON: {e}"))
                })?;
                emps.sort_by(|a, b| a.id.cmp(&b.id));
                // No active member in the role — leave it for the
                // pull-side role queue; nothing to notify.
                let Some(first) = emps.first() else {
                    return Ok(());
                };
                (
                    first.id.clone(),
                    format!("waiting on the {r} team"),
                    "signal",
                )
            }
            (None, None) => return Ok(()),
        };

        // Name the Subject, not just the step kind. Seven feedback
        // Jobs produce seven identical "Ready: task step needs the
        // platform-admin team" lines, and an inbox where every row
        // reads the same is a list you scroll past.
        //
        // The VERB follows the life moment on the triggering topic:
        // this handler serves both step.ready.* (its original job)
        // and step.done.* (the wait-over signal, rule
        // `notify-on-step-done-marked`). The first live done
        // notification read "Ready: …" (backlog `2f2565fb`) — a
        // wait-over signal announcing itself as new work.
        let done = ctx.triggering_topic.starts_with("step.done.");
        let subject = if done {
            format!("Done: {} — {}", ev.kind, ev.subject_id)
        } else {
            format!("Ready: {} — {}", ev.kind, ev.subject_id)
        };
        let body = if done {
            format!(
                "The '{}' step on {} {} completed — the wait it gated is \
                 over. Opening this message goes straight to the step.",
                ev.kind, ev.subject_kind, ev.subject_id
            )
        } else {
            format!(
                "A '{}' step is ready on {} {}, {}. \
                 Opening this message goes straight to the step.",
                ev.kind, ev.subject_kind, ev.subject_id, waiting_on
            )
        };
        let msg = json!({
            // Deterministic id `notify:{step_id}:{recipient}`. A
            // redelivered `step.ready.<kind>` event (JetStream
            // at-least-once) re-runs this handler; the stable id collapses
            // on the messages `ON CONFLICT (id) DO NOTHING` insert instead
            // of stacking a duplicate inbox row. Per-recipient so a future
            // role-fan-out keys cleanly; one row per (step, recipient).
            "id": format!("{id_prefix}:{}:{}", ev.step_id, recipient),
            "sender_id": "automation:dispatcher",
            "recipient_id": recipient,
            "subject": subject,
            "body": body,
            "kind": kind,
            // Link to the STEP, not the Job. The notification exists
            // because one specific step became ready; landing on the
            // Job leaves the reader to find it again among the others,
            // which is work the message already did. `/jobs/{job}/
            // steps/{step}` is the full-page step surface, so the link
            // opens the thing the message is about.
            //
            // `entity_type` follows the entity: nothing keys on it
            // (the inbox renders `entity_path` directly and shows the
            // type only as a label), and calling a step a job would be
            // a small lie that costs nothing to avoid.
            "entity_ref": {
                "entity_type": "step",
                "entity_id": ev.step_id,
                "entity_path": format!("/jobs/{}/steps/{}", ev.job_id, ev.step_id),
            },
        });
        let msg_url = format!(
            "{}/api/messages/send",
            self.messages_base.trim_end_matches('/')
        );
        let mresp = self
            .client
            .post(&msg_url)
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&msg)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {msg_url}: {e}")))?;
        if !mresp.status().is_success() {
            let status = mresp.status();
            let body = mresp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {msg_url} returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        ctx_on("step.ready.bill-approval", payload)
    }

    fn ctx_on(topic: &str, payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "notify-assignee-on-step-ready".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: topic.into(),
            event_payload: payload,
        }
    }

    /// The FIRST live done-notification (backlog `2f2565fb`, fired
    /// 2026-08-09 by the `notify-on-step-done-marked` rule) read
    /// "Ready: task — train/20260809-1931": the wait-over signal
    /// announced itself as new work, because the subject hardcoded
    /// the READY wording. The verb follows the triggering topic's
    /// life moment.
    #[tokio::test]
    async fn a_done_topic_announces_done_not_ready() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx_on("step.done.task", ready_payload()))
            .await
            .expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        let subject = sent["subject"].as_str().unwrap_or_default();
        let body = sent["body"].as_str().unwrap_or_default();
        assert!(
            subject.starts_with("Done:"),
            "a done topic must announce Done, got: {subject}"
        );
        assert!(
            body.contains("completed"),
            "the body must say the step completed, got: {body}"
        );
        assert!(
            !body.contains("is ready"),
            "a done body must not claim readiness, got: {body}"
        );
    }

    /// Stand-ins for `boss-people` and `boss-messages`. The messages
    /// side captures the posted body so the test can assert what an
    /// operator would actually receive — nothing pinned that before,
    /// which is why the link could point anywhere without a failure.
    async fn mock_services() -> (
        String,
        String,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        use axum::{
            Json, Router,
            routing::{get, post},
        };

        let people = Router::new().route(
            "/api/people",
            get(|| async {
                // Deliberately out of id order: the handler picks the
                // deterministic on-call member (lowest id).
                Json(serde_json::json!([{ "id": "emp-zz" }, { "id": "emp-aa" }]))
            }),
        );
        let people_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let people_addr = people_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(people_listener, people).await.unwrap() });

        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let cap = captured.clone();
        let messages = Router::new().route(
            "/api/messages/send",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    Json(serde_json::json!({ "ok": true }))
                }
            }),
        );
        let msg_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let msg_addr = msg_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(msg_listener, messages).await.unwrap() });

        (
            format!("http://{people_addr}"),
            format!("http://{msg_addr}"),
            captured,
        )
    }

    /// A ready step ADDRESSED to someone. Since the on-call
    /// broadcast was removed (David, 934cb22c: "we aren't ready for
    /// human on-call yet"), an assignee is what makes a READY step
    /// notify at all, so the tests that check message SHAPE use this.
    fn assigned_ready_payload() -> serde_json::Value {
        let mut p = ready_payload();
        p["assignee_id"] = serde_json::json!("emp-aa-001");
        p
    }

    fn ready_payload() -> serde_json::Value {
        serde_json::json!({
            "job_id": "11111111-1111-1111-1111-111111111111",
            "step_id": "22222222-2222-2222-2222-222222222222",
            "kind": "review-design",
            "subject_kind": "custom",
            "subject_id": "docs/design/the-three-layers.md",
            "metadata": { "authority_role": "platform-admin" }
        })
    }

    /// The notification exists because one specific step became ready,
    /// so it must open that step. Linking to the Job leaves the reader
    /// to find it again among the others — work the message already
    /// did.
    /// The defect this handler was filed for. A `task` step is
    /// documented as "Simple assigned task for HR, IT, admin" with
    /// `required_roles = []` — assignment IS its routing mechanism —
    /// and this handler used to key on `authority_role` alone, so
    /// filing a task FOR someone told them nothing.
    ///
    /// Caught by the inbox rather than by code: two backlog-items with
    /// gated triage steps notified automatically, while two ad-hoc
    /// tasks assigned to the same person needed a message sent by
    /// hand.
    #[tokio::test]
    async fn an_assigned_step_notifies_its_assignee_with_no_role() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["assignee_id"] = serde_json::json!("emp-bootstrap-admin");
        // No authority_role at all — the case that used to be a no-op.
        payload["metadata"] = serde_json::json!({});
        h.invoke(&[], &ctx(payload)).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-bootstrap-admin");
    }

    /// An assignee is the more specific claim: a role says someone
    /// like you should do this, an assignee says you specifically. So
    /// when both are present the person wins, and the role's on-call
    /// member (emp-aa in the mock) is NOT the recipient.
    #[tokio::test]
    async fn the_assignee_wins_over_the_role() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["assignee_id"] = serde_json::json!("emp-named");
        h.invoke(&[], &ctx(payload)).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-named");
        assert_ne!(sent["recipient_id"], "emp-aa");
    }

    /// Neither signal is still the only no-op. Outcome steps an
    /// operator picks off a queue must not generate an inbox row each.
    #[tokio::test]
    async fn a_step_with_neither_assignee_nor_role_stays_silent() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["metadata"] = serde_json::json!({});
        h.invoke(&[], &ctx(payload)).await.expect("no-op");
        assert!(captured.lock().unwrap().is_none(), "nothing should be sent");
    }

    #[tokio::test]
    async fn links_to_the_step_not_the_job() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx(assigned_ready_payload()))
            .await
            .expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(
            sent["entity_ref"]["entity_path"],
            "/jobs/11111111-1111-1111-1111-111111111111/steps/22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(sent["entity_ref"]["entity_type"], "step");
        assert_eq!(
            sent["entity_ref"]["entity_id"],
            "22222222-2222-2222-2222-222222222222"
        );
    }

    /// An inbox where every row reads the same is a list you scroll
    /// past. Seven feedback Jobs produced seven identical "Ready: task
    /// step needs the platform-admin team" lines.
    #[tokio::test]
    async fn subject_names_the_subject() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx_on("step.done.task", ready_payload()))
            .await
            .expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        let subject = sent["subject"].as_str().unwrap_or_default();
        assert!(
            subject.contains("docs/design/the-three-layers.md"),
            "subject must identify WHICH item: {subject}"
        );
        assert!(subject.contains("review-design"), "subject: {subject}");
        // This used to also assert the body named the responsible TEAM.
        // That wording only ever appeared on the ready-plus-role path,
        // and that path no longer sends anything (David, 934cb22c: "we
        // aren't ready for human on-call yet"), so there is no surviving
        // message that names a team. What the body must still do is say
        // what happened — dropping the assertion rather than replacing
        // it would leave the body unchecked.
        assert!(
            sent["body"]
                .as_str()
                .unwrap_or_default()
                .contains("completed"),
            "body must say what happened to the step: {}",
            sent["body"]
        );
    }

    /// Redelivery is at-least-once, so the id has to be stable per
    /// (step, recipient) or a JetStream retry stacks a duplicate row.
    #[tokio::test]
    async fn notifies_the_lowest_id_holder_with_a_stable_id() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx_on("step.done.task", ready_payload()))
            .await
            .expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-aa");
        assert_eq!(
            sent["id"],
            "notify:22222222-2222-2222-2222-222222222222:emp-aa"
        );
    }

    #[tokio::test]
    async fn neither_signal_makes_no_http_call_at_all() {
        // Renamed from `no_authority_role_is_noop`, which became an
        // overclaim: a step with no authority_role but WITH an assignee
        // now notifies. The narrower truth this still proves is the
        // valuable one — with neither signal the handler returns
        // without touching the network, since the URLs are unreachable
        // and any call would error.
        let h = MessagesNotify::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let payload = serde_json::json!({
            "job_id": "11111111-1111-1111-1111-111111111111",
            "step_id": "22222222-2222-2222-2222-222222222222",
            "kind": "outcome",
            "subject_kind": "vendor",
            "subject_id": "vnd-1",
            "metadata": { "outcome_kind": "completed" }
        });
        let res = h.invoke(&[], &ctx(payload)).await;
        assert!(res.is_ok(), "no-role step should be a no-op: {res:?}");
    }

    #[tokio::test]
    async fn malformed_payload_errors() {
        let h = MessagesNotify::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let res = h
            .invoke(&[], &ctx(serde_json::json!("not-an-object")))
            .await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }

    /// A step can notify at READY and again at DONE (the train's
    /// keep-going signal — feedback from David 2026-08-09: BOSS
    /// itself alerts us when a wait ends). The two must not collapse
    /// on the dedup id, so the rule passes `id_prefix = "done"` and
    /// the posted message id changes prefix.
    #[tokio::test]
    async fn id_prefix_arg_separates_done_notifications_from_ready() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let payload = assigned_ready_payload();
        let args = vec![(
            "id_prefix".to_string(),
            boss_dispatcher::rules::expr::Value::String("done".into()),
        )];
        h.invoke(&args, &ctx(payload)).await.expect("handles");
        let body = captured.lock().unwrap().clone().expect("posted");
        assert_eq!(
            body["id"], "done:22222222-2222-2222-2222-222222222222:emp-aa-001",
            "the id prefix must come from the rule arg"
        );
    }
}
