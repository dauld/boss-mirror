//! A packet's `outcome` is what its close recorded (backlog 36352452).
//!
//! The terminal close (`close_job_on_terminal`) and the all-steps-
//! terminal catch-all stamp `metadata.outcome` from the completed
//! terminal; a hand close by the job PUT names its own. After that the
//! value is the record of how the packet ended, and every reader of a
//! closed packet — the yard, the watchlist, the close marker's rules —
//! takes it as fact. The review of car 9392b8b5 measured two writes that
//! rewrote it anyway: a job PUT `{status: open, metadata: {outcome:
//! aborted}}` reopened a closed packet and changed its outcome in one
//! write, and `PATCH /metadata {outcome: forged}` on a closed packet
//! answered 204. So `outcome` is reserved like `corrections` and
//! `repins`: the close writes it, and the only other writer is the
//! repair of a close that lost it (`boss job outcome`, 228c9a7d), which
//! may write exactly the value the completed terminal declares.

use boss_core::job::{JobStatus, Step, StepStatus};
use serde_json::Value;

use crate::registry::WorkflowSpec;

/// The job metadata key a close writes its outcome under.
pub const OUTCOME_KEY: &str = "outcome";

/// The hint a refused reopen carries.
pub const REOPEN_HINT: &str = "a closed or cancelled packet is a finished record and does not \
     reopen. Work that must continue is a new packet — file one that names this one — and a \
     finding about what this one said is a correction (`boss correct <packet>`).";

/// The hint a job PUT that changes a recorded outcome carries.
pub const PUT_REFUSAL_HINT: &str = "`outcome` is written by the close — the terminal step's, \
     or a hand close that moves the packet to closed in the same write — and not by a later \
     PUT. Send the stored value back, or leave the key out and it is kept.";

/// The hint a metadata merge that writes `outcome` carries.
pub const PATCH_REFUSAL_HINT: &str = "`outcome` is written by the close, not by a metadata \
     merge. A closed packet whose close lost it is repaired by `boss job outcome <packet>`, \
     which merges exactly the outcome its completed terminal declares — the one value this \
     door accepts, and only while none is recorded.";

/// PURE: does a job PUT body change the recorded outcome? An omitted
/// key is not a change (the handler carries the stored value forward);
/// an explicit `null` over a recorded outcome is a delete, and is.
pub fn put_changes_outcome(stored: &Value, sent: &Value) -> bool {
    sent.get(OUTCOME_KEY)
        .is_some_and(|v| v != stored.get(OUTCOME_KEY).unwrap_or(&Value::Null))
}

/// PURE: the outcome a packet with these steps closed with — the first
/// completed step that is a declared terminal of `spec`, paired by
/// index (`sort_order`, the materializer's contract), steps read in
/// sort order. `None` when no terminal completed.
pub fn derived(spec: &WorkflowSpec, steps: &[Step]) -> Option<String> {
    let mut completed: Vec<&Step> = steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed)
        .collect();
    completed.sort_by_key(|s| s.sort_order);
    completed.into_iter().find_map(|s| {
        usize::try_from(s.sort_order)
            .ok()
            .and_then(|i| spec.steps.get(i))
            .and_then(|spec_step| spec_step.terminal.as_ref())
            .map(|t| t.outcome.clone())
    })
}

/// PURE: may a metadata merge that carries `sent` as the outcome land
/// on a packet in `status` whose stored metadata is `stored`? Yes for
/// an unchanged re-send (a `null` where none is recorded included), and
/// for the repair: a CLOSED packet recording no outcome, merged the one
/// its completed terminal declares (`derived`). Everything else — a
/// set on an open packet, a change, a delete — is refused.
pub fn patch_may_write(
    status: JobStatus,
    stored: &Value,
    sent: &Value,
    derived: Option<&str>,
) -> bool {
    let recorded = stored.get(OUTCOME_KEY);
    let unchanged = sent == recorded.unwrap_or(&Value::Null);
    let repair = status == JobStatus::Closed
        && recorded.is_none_or(Value::is_null)
        && derived.is_some_and(|d| sent.as_str() == Some(d));
    unchanged || repair
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{StepSpec, Terminal};
    use serde_json::json;

    fn spec() -> WorkflowSpec {
        let step = |title: &str, outcome: Option<&str>| StepSpec {
            title: title.into(),
            terminal: outcome.map(|o| Terminal { outcome: o.into() }),
            ..Default::default()
        };
        WorkflowSpec::platform_seed(
            "k",
            "K",
            "platform",
            vec!["custom".into()],
            vec![
                step("work", None),
                step("done", Some("done")),
                step("aborted", Some("aborted")),
            ],
        )
    }

    fn steps(statuses: [StepStatus; 3]) -> Vec<Step> {
        let job = boss_core::job::JobId::new();
        statuses
            .into_iter()
            .enumerate()
            .map(|(i, status)| {
                let mut s = Step::new(job, "task", "s", i as i32);
                s.status = status;
                s
            })
            .collect()
    }

    #[test]
    fn a_put_that_omits_or_resends_the_outcome_is_not_a_change() {
        let stored = json!({ "outcome": "done", "note": 1 });
        assert!(!put_changes_outcome(&stored, &json!({ "note": 2 })));
        assert!(!put_changes_outcome(&stored, &json!({ "outcome": "done" })));
        assert!(put_changes_outcome(
            &stored,
            &json!({ "outcome": "aborted" })
        ));
        assert!(put_changes_outcome(&stored, &json!({ "outcome": null })));
        assert!(!put_changes_outcome(
            &json!({}),
            &json!({ "outcome": null })
        ));
        assert!(put_changes_outcome(
            &json!({}),
            &json!({ "outcome": "done" })
        ));
    }

    #[test]
    fn the_outcome_is_the_first_completed_terminal() {
        use StepStatus::*;
        let spec = spec();
        assert_eq!(
            derived(&spec, &steps([Completed, Completed, Skipped])).as_deref(),
            Some("done")
        );
        assert_eq!(
            derived(&spec, &steps([Completed, Skipped, Completed])).as_deref(),
            Some("aborted")
        );
        assert_eq!(derived(&spec, &steps([Completed, Skipped, Skipped])), None);
    }

    #[test]
    fn a_merge_writes_an_outcome_only_as_the_repair_of_a_lost_one() {
        let closed = JobStatus::Closed;
        let recorded = json!({ "outcome": "done" });
        let lost = json!({});
        // Unchanged re-sends land anywhere.
        assert!(patch_may_write(closed, &recorded, &json!("done"), None));
        assert!(patch_may_write(JobStatus::Open, &lost, &Value::Null, None));
        // A change or a delete of a recorded outcome does not.
        assert!(!patch_may_write(
            closed,
            &recorded,
            &json!("forged"),
            Some("done")
        ));
        assert!(!patch_may_write(
            closed,
            &recorded,
            &Value::Null,
            Some("done")
        ));
        // The repair: closed, none recorded, exactly the derived value.
        assert!(patch_may_write(closed, &lost, &json!("done"), Some("done")));
        assert!(!patch_may_write(
            closed,
            &lost,
            &json!("aborted"),
            Some("done")
        ));
        assert!(!patch_may_write(closed, &lost, &json!("done"), None));
        // An open packet has had no close to repair.
        assert!(!patch_may_write(
            JobStatus::Open,
            &lost,
            &json!("done"),
            Some("done")
        ));
    }
}
