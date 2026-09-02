//! `jobs.complete_step` — auto-complete a zero-duration marker step.
//!
//! Structural markers — `outcome` (a terminal fork result) and
//! `milestone` (a named checkpoint) — represent state the machine
//! reaches on its own, not work a human or agent performs. They carry no
//! `required_roles` and a `typical_duration_hours` of `0.0`. When such a
//! step becomes Ready the system should advance it immediately so the
//! downstream `ready_when` predicates re-evaluate without waiting on an
//! executor.
//!
//! The third marker, `trigger`, is handled NOT here but at Job
//! materialization (its completion authority is `auto-on-materialize`):
//! the firing trigger is born `Completed` and its alternatives `Skipped`,
//! so a trigger never reaches Ready and `should_auto_complete` excludes
//! it explicitly.
//!
//! This handler is the **system half** of the "both layered" routing: the
//! dispatcher's role-assignment loop hands role-bearing Ready steps to
//! Employees (the workforce drives those), and this handler completes the
//! no-role, zero-duration markers itself. `task` steps — no role either,
//! but real HR/IT/admin work with an unset duration — are deliberately
//! NOT markers and are left for an executor.
//!
//! ## Why one `step.ready.*` rule, not three per-kind rules
//!
//! It listens on `step.ready.*` — the same topic the push-notifier
//! (`messages.notify`) uses — so it rides that single NATS subscription.
//! Adding explicit `step.ready.trigger` / `.outcome` / `.milestone`
//! subscriptions would each *overlap* the existing `step.ready.*`
//! subscription, so NATS core would deliver every marker event twice
//! (once per matching subscription) and the marker would be completed
//! twice. Sharing the one wildcard subscription keeps it exactly-once.
//! The marker test is therefore read per-event from the StepType registry
//! (`required_roles` empty AND `typical_duration_hours <= 0.0`) rather
//! than encoded as a hardcoded kind list in the rule topic — a new
//! structural marker kind auto-completes with no change here, per the
//! "registries over hardcoded paths" principle.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::step_registry::{Completion, StepRegistry};
use serde_json::json;
use std::sync::Arc;

use super::common::{StepEvent, dispatcher_actor_header, sim_origin_value};

pub struct JobsCompleteStep {
    client: reqwest::Client,
    jobs_base: String,
    registry: Arc<StepRegistry>,
}

impl JobsCompleteStep {
    pub fn new(jobs_base: impl Into<String>, registry: Arc<StepRegistry>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            registry,
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// mock server; production passes a fresh client).
    pub fn with_client(
        client: reqwest::Client,
        jobs_base: impl Into<String>,
        registry: Arc<StepRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            registry,
        })
    }

    /// A step kind is an auto-completable marker when its StepType carries
    /// no role and zero typical duration — a structural transition the
    /// machine fires itself (`trigger` / `outcome` / `milestone`). `task`
    /// (no role, but unset duration = real work) is excluded because its
    /// `typical_duration_hours` is `None`; every role-bearing or
    /// nonzero-duration kind is excluded by the conjunction. An unknown
    /// kind (not in the registry) is conservatively not a marker.
    fn is_marker(&self, kind: &str) -> bool {
        self.registry.get(kind).is_some_and(|st| {
            st.required_roles.is_empty() && st.typical_duration_hours.is_some_and(|h| h <= 0.0)
        })
    }

    /// Whether the dispatcher should complete this step itself rather than
    /// hand it to an Employee. Two cases:
    ///   - structural markers (trigger / outcome / milestone), and
    ///   - agent action steps (`executor = Agent`) that are NOT gates —
    ///     order-intake, acknowledgment, billing: computer-speed automation
    ///     a human shouldn't queue behind. Gates (an `outcome` enum the
    ///     Workflow forks on) are excluded — `gate.resolve` completes those
    ///     after computing the outcome from real stock.
    /// A gate — an `outcome` enum the Workflow forks on. Never auto-completed
    /// here even though it carries the marker shape (no role + 0 duration):
    /// gate.resolve must compute its outcome from real stock first.
    fn is_gate(&self, kind: &str) -> bool {
        self.registry
            .get(kind)
            .is_some_and(|st| st.fields.iter().any(|f| f.name == "outcome"))
    }

    fn should_auto_complete(&self, kind: &str) -> bool {
        if self.is_gate(kind) {
            return false;
        }
        // Triggers (`auto-on-materialize`) are resolved at Job
        // materialization — the firing one is born `Completed`, its
        // alternatives `Skipped` — so they never reach Ready and this
        // handler must never complete them. Excluded explicitly: a
        // trigger fits the marker shape (no role + 0 duration) but its
        // completion authority says materialization owns it, not the
        // step.ready path. (Outcome / milestone markers are NOT
        // auto-on-materialize and still complete here when they go
        // Ready.)
        if self
            .registry
            .get(kind)
            .is_some_and(|st| st.completion == Completion::AutoOnMaterialize)
        {
            return false;
        }
        // Structural markers (outcome / milestone) + agent action steps
        // (order-intake / billing) advance at computer speed without an
        // executor.
        self.is_marker(kind)
            || self
                .registry
                .get(kind)
                .is_some_and(|st| st.completion == Completion::Agent)
    }
}

#[async_trait]
impl Handler for JobsCompleteStep {
    fn name(&self) -> &'static str {
        "jobs.complete_step"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        // Structural markers AND non-gate agent steps auto-complete here.
        // Every human step (and every gate, which gate.resolve handles) is a
        // no-op — those route to an executor (the dispatcher assigns the
        // role-bearing ones; the workforce drives them), including the
        // `task` kind, which has no role but is genuine work.
        if !self.should_auto_complete(ev.kind) {
            return Ok(());
        }

        // PUT the marker to `completed`. PATCH-on-PUT keeps the
        // materialized metadata intact, so any registry-required-at-done
        // fields stay satisfied; sending only `status` avoids clobbering
        // them. Attribution is the rule (`rule:<name>`), never a person —
        // a marker is no one's work. No sign-off: markers carry no
        // `authority_role`, so the sign-off gate doesn't apply.
        let url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.jobs_base.trim_end_matches('/'),
            ev.job_id,
            ev.step_id,
        );
        let body = json!({ "status": "completed" });
        let resp = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {status}: {text}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> Arc<JobsCompleteStep> {
        JobsCompleteStep::new("http://127.0.0.1:1", Arc::new(StepRegistry::v1()))
    }

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "complete-marker-on-step-ready".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.ready.trigger".into(),
            event_payload: payload,
        }
    }

    #[test]
    fn agent_actions_and_markers_auto_complete_gates_and_humans_do_not() {
        let h = handler();
        // Triggers do NOT auto-complete here — they are resolved at Job
        // materialization (firing → Completed, alternatives → Skipped),
        // so they never reach Ready.
        assert!(!h.should_auto_complete("trigger"));
        // The other structural markers still auto-complete on Ready.
        assert!(h.should_auto_complete("outcome"));
        // Agent action steps auto-complete (order-intake / billing).
        assert!(h.should_auto_complete("order-intake"));
        assert!(h.should_auto_complete("billing"));
        // Gates do NOT — gate.resolve computes their outcome first.
        assert!(!h.should_auto_complete("demand-gate"));
        assert!(!h.should_auto_complete("availability-gate"));
        // Genuine human work does NOT.
        assert!(!h.should_auto_complete("production-consume"));
        assert!(!h.should_auto_complete("handoff"));
        assert!(!h.should_auto_complete("task"));
    }

    #[test]
    fn markers_are_the_zero_duration_no_role_kinds() {
        let h = handler();
        // The three structural markers.
        assert!(h.is_marker("trigger"));
        assert!(h.is_marker("outcome"));
        assert!(h.is_marker("milestone"));
        // `task`: no role but real work (unset duration) — NOT a marker.
        assert!(!h.is_marker("task"));
        // `gate-verdict`: no role AND the runner completes it via the
        // API — it must stay off the marker shape (duration None, the
        // same escape `task` uses), or this handler would complete every
        // gate's verdict EMPTY the moment it goes Ready, freezing the
        // step against the runner's real write and breaking every gate.
        // The §9a equality test between this property-classification and
        // the StepType declaration in step_types.toml.
        assert!(!h.is_marker("gate-verdict"));
        assert!(!h.should_auto_complete("gate-verdict"));
        // Role-bearing / nonzero-duration work — NOT markers.
        assert!(!h.is_marker("demand-gate"));
        assert!(!h.is_marker("bill-approval"));
        assert!(!h.is_marker("production-consume"));
        // Unknown kind — conservatively not a marker.
        assert!(!h.is_marker("not-a-real-kind"));
    }

    #[tokio::test]
    async fn non_marker_step_is_noop() {
        // A role-bearing ready step must short-circuit before any HTTP
        // call (the jobs URL is unreachable; a call would error).
        let h = handler();
        let payload = json!({
            "job_id": "11111111-1111-1111-1111-111111111111",
            "step_id": "22222222-2222-2222-2222-222222222222",
            "kind": "bill-approval",
            "subject_kind": "vendor",
            "subject_id": "vnd-1",
            "metadata": { "authority_role": "bookkeeper" }
        });
        let res = h.invoke(&[], &ctx(payload)).await;
        assert!(res.is_ok(), "non-marker should be a no-op: {res:?}");
    }

    #[tokio::test]
    async fn malformed_payload_errors() {
        let h = handler();
        let res = h.invoke(&[], &ctx(json!("not-an-object"))).await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }
}
