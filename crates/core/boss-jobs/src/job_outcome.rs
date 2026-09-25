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

use boss_core::job::JobStatus;
use serde_json::Value;

/// The job metadata key a close writes its outcome under.
pub const OUTCOME_KEY: &str = "outcome";

/// The hint a refused reopen carries.
pub const REOPEN_HINT: &str = "a closed or cancelled packet is a finished record and does not \
     reopen. Work that must continue is a new packet — file one that names this one — and a \
     finding about what this one said is a correction (`boss correct <packet>`).";

/// The hint a refused admission carries (backlog 570e72bd).
pub const ADMISSION_HINT: &str = "a packet ends through its protocol's terminal, or through a \
     job PUT that closes it, and the close writes its `outcome`. Admit it `open` (or `draft`) \
     with no `outcome`; a packet that records something already finished is a correction or a \
     note on the packet that did the work.";

/// The hint a refused hand skip of a protocol step carries.
pub const HAND_SKIP_HINT: &str = "a protocol step is skipped by its protocol: the engine skips \
     one whose `ready_when` can no longer hold, and a terminal's close skips what is left. To \
     stop the work, complete the protocol's abort terminal (it completes from any open state) \
     or cancel the packet; a skipped step reads as resolved to the steps behind it, so a skip \
     by hand would open them with the work undone.";

/// The hint a step whose `ready_when` does not hold carries.
pub const READY_WHEN_HINT: &str = "the step's own `ready_when` is read against the packet as \
     stored, and it does not hold. Do what it waits on — complete the steps it names, or set the \
     job-metadata marker it reads through `PATCH /api/jobs/{id}/metadata` — and the engine \
     opens the step; then complete it.";

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

/// PURE: may a metadata merge that carries `sent` as the outcome land
/// on a packet in `status` whose stored metadata is `stored`? Yes for
/// an unchanged re-send (a `null` where none is recorded included), and
/// for the repair: a CLOSED packet recording no outcome, merged the one
/// its completed terminal declares (`derived`, from
/// [`crate::registry::WorkflowSpec::completed_terminal_outcome`] — the one statement of
/// that rule; this module held a second copy until 5f99cd11).
/// Everything else — a set on an open packet, a change, a delete — is
/// refused.
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
    use serde_json::json;

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
