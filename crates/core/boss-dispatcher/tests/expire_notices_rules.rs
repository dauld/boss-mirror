//! The two rules that retire the step notifier's notices (backlog
//! 0b2bac00), at the layer they are implemented in: rule rows over
//! `messages.expire_notices`.
//!
//! Measured for emp-david over 2026-09-16T23:54Z..2026-09-23T16:40Z:
//! 90 direct notices, of which 83 pointed at completed steps and 1 at a
//! skipped one, all still counted in his badge — because the only
//! expiry fired on a JOB closing and moved signals alone. What is worth
//! pinning here, and invisible from the handler's unit tests:
//!
//! 1. The step rule SELECTS on a step ending — `completed` and
//!    `skipped`, both — and on nothing else: a notice about a ready or
//!    active step is still true.
//! 2. Its predicate evaluates on every `jobs.step.updated`. The expr
//!    binder makes an absent identifier a `PredicateFailed`, which
//!    dead-letters, and this is one of the busiest topics there is;
//!    `status` is a field every serialized Step carries.
//! 3. The job-close backstop fires on every close, whatever the kind.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::match_event;
use serde_json::json;

mod common;
use common::authored_rule;

const HANDLER: &str = "messages.expire_notices";
const ON_STEP: &str = "expire-notices-on-step-ended";
const ON_JOB: &str = "expire-notices-on-job-closed";

struct NoHelpers;

impl HelperResolver for NoHelpers {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        Err(EvalError::UnknownHelper(name.to_string()))
    }
}

/// A `jobs.step.updated` in the shape `events::step_state_payload`
/// builds: the whole serialized Step plus `step_id`.
fn step_updated(status: &str) -> serde_json::Value {
    json!({
        "id": "0000ef0d-0000-4000-8000-000000000002",
        "step_id": "0000ef0d-0000-4000-8000-000000000002",
        "job_id": "9876ef0d-0000-4000-8000-000000000001",
        "kind": "task",
        "title": "Build the change",
        "assignee_id": "emp-david",
        "status": status,
        "sort_order": 0,
        "blocked_by": [],
        "metadata": {},
    })
}

fn fires(rule: &str, topic: &str, payload: &serde_json::Value) -> bool {
    let reg = authored_rule(rule);
    let outcome = match_event(&reg, topic, payload, &NoHelpers);
    assert!(
        outcome.skipped.is_empty(),
        "{rule}: a predicate failed on `{topic}`, which would dead-letter it: {:?}",
        outcome.skipped
    );
    outcome
        .matched
        .iter()
        .any(|m| m.rule_name == rule && m.invocations.iter().any(|i| i.handler == HANDLER))
}

#[test]
fn a_step_ending_retires_its_notices() {
    for status in ["completed", "skipped"] {
        assert!(
            fires(ON_STEP, "jobs.step.updated", &step_updated(status)),
            "a `{status}` step's notice is past relevancy"
        );
    }
}

#[test]
fn a_step_still_open_keeps_its_notices() {
    for status in ["pending", "ready", "active"] {
        assert!(
            !fires(ON_STEP, "jobs.step.updated", &step_updated(status)),
            "a `{status}` step is still waiting on its assignee"
        );
    }
}

#[test]
fn a_job_closing_retires_every_notice_under_it() {
    let closed = json!({
        "id": "9876ef0d-0000-4000-8000-000000000001",
        "closed_on": null,
        "kind": "backlog-item",
        "outcome": null,
        "title": "t",
        "parent_step_id": null,
    });
    assert!(fires(ON_JOB, "jobs.job.closed", &closed));
}
