//! An ACTIVE step keeps the holder that claimed it until it is released.
//!
//! A claim is the Ready→Active compare-and-set, and it records who won
//! it. Two step PUTs used to take an active step off that actor without
//! a claim of their own:
//!
//! - a PUT naming a DIFFERENT holder (backlog 650ebd0c, the review of
//!   car e341f7cd): a redelivered dispatcher nomination wrote its pick
//!   over the claimant about a second after the claim;
//! - a PUT CLEARING the holder while the step stays Active (backlog
//!   0f42efa0, the review of car fb3e9424): `{"assignee_id":null}` or
//!   `""` answered 204 and left an active step nobody held, so the next
//!   PUT naming anyone met no holder to protect and installed them —
//!   the first defect again, in two writes.
//!
//! So both are refused, and the one way an active step changes hands is
//! the RELEASE — `{"status":"ready","assignee_id":null}` in ONE body,
//! the body the abandoned-step reclaim and `boss step release` send —
//! followed by a claim through the CAS: two writes, each on the record.
//! A release names nobody; naming someone in it is the one-write
//! reassignment again.
//!
//! `""` and a blank are the same clear as `null`: every reader of the
//! holder here (and the dispatcher's assignee check) reads them as
//! nobody, so a rule that only knew `null` would leave `""` as the way
//! round it.
//!
//! The refusal's body is built HERE, once, and the dispatcher's test
//! reads this builder's output rather than a hand copy of its shape
//! (CLAUDE.md §9a) — the dispatcher acks a nomination this refuses as
//! "somebody holds it".

use boss_core::job::StepStatus;
use serde_json::Value;

/// The way to move an active step to another holder, named in the
/// refusal that stops a PUT doing it in one write (backlog 650ebd0c).
pub const HINT: &str = "an active step changes hands in two writes: free it with \
     {\"status\":\"ready\",\"assignee_id\":null}, then the next holder claims it through \
     POST .../claim. A nominator that finds the step already held has nothing to do.";

/// The error line of the refusal.
pub const ERROR: &str = "step is active and held — a PUT does not replace or clear its holder unless it releases the step";

/// A holder value that names someone: `None`, `""` and a blank do not.
fn named(holder: Option<&str>) -> Option<&str> {
    holder.filter(|h| !h.trim().is_empty())
}

/// Is this write refused? `old_*` is the stored row; `next_*` is the
/// row the PUT would write (the body overlaid on it). Only an ACTIVE
/// step whose stored holder names someone is judged: there is nothing
/// to keep on any other.
pub fn refuses(
    old_status: StepStatus,
    old_holder: Option<&str>,
    next_status: StepStatus,
    next_holder: Option<&str>,
) -> bool {
    if old_status != StepStatus::Active {
        return false;
    }
    let Some(holder) = named(old_holder) else {
        return false;
    };
    match named(next_holder) {
        // A different name, whatever the status beside it: a release
        // names nobody, so this is a reassignment either way.
        Some(next) => next != holder,
        // A clear is a release only when the same body moves the step
        // out of Active.
        None => next_status == StepStatus::Active,
    }
}

/// The 409's body, in the terminal freeze's shape (`step_status` +
/// `refused_fields`), naming who holds the step.
pub fn refusal_body(step_id: &str, holder: &str) -> Value {
    serde_json::json!({
        "error": ERROR,
        "step_id": step_id,
        "step_status": "active",
        "holder": holder,
        "refused_fields": ["assignee_id"],
        "hint": HINT,
    })
}

#[cfg(test)]
mod tests {
    use super::refuses;
    use boss_core::job::StepStatus::{Active, Completed, Pending, Ready};

    const H: Option<&str> = Some("emp-holder");

    #[test]
    fn a_different_holder_is_refused_in_any_status() {
        assert!(refuses(Active, H, Active, Some("emp-other")));
        assert!(refuses(Active, H, Ready, Some("emp-other")));
    }

    #[test]
    fn a_clear_is_refused_unless_the_step_leaves_active() {
        for cleared in [None, Some(""), Some("  ")] {
            assert!(refuses(Active, H, Active, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Ready, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Pending, cleared), "{cleared:?}");
            assert!(!refuses(Active, H, Completed, cleared), "{cleared:?}");
        }
    }

    #[test]
    fn the_holder_itself_and_steps_nobody_holds_pass() {
        assert!(!refuses(Active, H, Active, H));
        assert!(!refuses(Active, H, Completed, H));
        assert!(!refuses(Active, None, Active, None));
        assert!(!refuses(Active, Some(""), Active, Some("emp-other")));
        assert!(!refuses(Ready, H, Ready, Some("emp-other")));
        assert!(!refuses(Ready, H, Ready, None));
    }
}
