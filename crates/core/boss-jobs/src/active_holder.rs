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
//! WHAT THIS DOES NOT ENFORCE (backlog 6ef4a36b, the review of car
//! 781b9209): that the next holder arrives through the claim. Once a
//! step is released it is Ready, and a PUT can still take a Ready step
//! to Active naming anyone — the PUT-as-claim path the ten platform
//! step surfaces' Start and the sim's workforce use to start work.
//! This module keeps an ACTIVE step's holder. Design 611fbffd
//! (answered 2026-09-26) decided who may START one: the claim door now
//! reserves as the PUT did (one function, `start_hold`, both doors),
//! and a claim FOR someone else is admitted only for the executor the
//! step declares or a holder of `step-assign` ([`nominee`],
//! [`declared_executor`]). The PUT's refusal of Ready→Active waits for
//! the surfaces to move to the claim door.
//!
//! `""` and a blank are the same clear as `null`: every reader of the
//! holder here (and the dispatcher's assignee check) reads them as
//! nobody, so a rule that only knew `null` would leave `""` as the way
//! round it. And a blank is STORED as nobody ([`stored`]), because the
//! claim CAS is the one reader that did not read it that way.
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

/// The holder a step PUT stores: a blank is stored as `None` (backlog
/// 6ef4a36b, the review of car 781b9209). Every reader here reads a
/// blank as nobody, but both claim CASes admit only a NULL holder or
/// the claimant, so a stored `Some("")` was a holder to the one door
/// that hands a step out — a `""` release answered 204 and left a
/// ready step no one could ever claim (409 `holder: ""`).
pub fn stored(holder: Option<String>) -> Option<String> {
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

/// The holder a claim names when it is NOT the caller (design
/// 611fbffd): `claimed_for` read as a holder is read everywhere here —
/// a blank names nobody — and the caller's own id is an ordinary claim
/// for oneself, so neither is a claim on someone else's behalf.
pub fn nominee<'a>(claimed_for: Option<&'a str>, caller: &str) -> Option<&'a str> {
    named(claimed_for.map(str::trim)).filter(|n| *n != caller)
}

/// The executor the PROTOCOL declares for a step — the `individual`
/// audience (`{individual = "automation:boss-step"}`) of the step named
/// `spec_slug` in `pinned`, the Workflow row at the version the packet
/// was admitted under (or moved to, on the record, by `boss job
/// convert`). That actor may start the step for someone else without
/// the `step-assign` authority (design 611fbffd, Q1 (a)); a role,
/// station or department audience names no one, and a step with no
/// slug (an ad-hoc add, a pre-slug row) declares no one.
///
/// NEVER FROM THE STEP'S METADATA (the adversarial review of car
/// 611fbffd, 2026-09-26). This read `metadata.audience`, which any
/// step writer could PATCH: a caller with no `step-assign` wrote
/// `{"audience":{"individual":<self>}}` (204) and then claimed the
/// step for anyone (200), logged as an authorised claim-for. The row
/// is registry data no step write reaches, so it is the one place the
/// declaration is still the protocol's.
pub fn declared_executor(
    pinned: &crate::registry::WorkflowSpec,
    spec_slug: Option<&str>,
) -> Option<String> {
    let slug = spec_slug?;
    let step = pinned.steps.iter().find(|s| s.title == slug)?;
    match step.audience.as_ref()? {
        crate::audience::Audience::Individual(id) => named(Some(id)).map(str::to_string),
        _ => None,
    }
}

/// The 403 a claim for someone else gets from anyone the rule does not
/// admit, naming the rule and the two ways through it.
pub fn claim_for_refusal_body(step_id: &str, caller: &str, nominee: &str) -> Value {
    serde_json::json!({
        "error": "a claim for someone else (claimed_for) is made only by the step's \
                  declared executor or a holder of the step-assign authority",
        "step_id": step_id,
        "caller": caller,
        "claimed_for": nominee,
        "hint": "claim the step for yourself (omit claimed_for), or ask a holder of \
                 Update on step-assign to start it for them",
    })
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
    use super::{declared_executor, nominee, refuses, stored};

    #[test]
    fn a_nominee_is_someone_other_than_the_caller() {
        assert_eq!(nominee(None, "emp-a"), None);
        assert_eq!(nominee(Some(""), "emp-a"), None);
        assert_eq!(nominee(Some("  "), "emp-a"), None);
        assert_eq!(nominee(Some("emp-a"), "emp-a"), None);
        assert_eq!(nominee(Some(" emp-a "), "emp-a"), None);
        assert_eq!(nominee(Some("emp-b"), "emp-a"), Some("emp-b"));
    }

    #[test]
    fn only_an_individual_audience_in_the_pinned_row_declares_an_executor() {
        use crate::audience::Audience;
        use crate::registry::{StepSpec, WorkflowSpec};
        let step = |slug: &str, audience: Option<Audience>| StepSpec {
            title: slug.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            audience,
            ..Default::default()
        };
        let row = WorkflowSpec::platform_seed(
            "k",
            "K",
            "platform",
            vec![],
            vec![
                step(
                    "run",
                    Some(Audience::Individual("automation:boss-step".into())),
                ),
                step("review", Some(Audience::Role("platform-admin".into()))),
                step("blank", Some(Audience::Individual(" ".into()))),
                step("plain", None),
            ],
        );
        assert_eq!(
            declared_executor(&row, Some("run")),
            Some("automation:boss-step".into())
        );
        assert_eq!(declared_executor(&row, Some("review")), None);
        assert_eq!(declared_executor(&row, Some("blank")), None);
        assert_eq!(declared_executor(&row, Some("plain")), None);
        assert_eq!(declared_executor(&row, Some("gone")), None);
        assert_eq!(declared_executor(&row, None), None);
    }

    #[test]
    fn a_blank_holder_is_stored_as_nobody() {
        assert_eq!(stored(None), None);
        assert_eq!(stored(Some(String::new())), None);
        assert_eq!(stored(Some(" \t".into())), None);
        assert_eq!(stored(Some("emp-a".into())), Some("emp-a".into()));
    }
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
