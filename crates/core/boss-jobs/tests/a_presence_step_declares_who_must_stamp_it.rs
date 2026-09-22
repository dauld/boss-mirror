//! A step that demands an assurance must also declare who stamps it.
//!
//! THE DEFECT (backlog 148549c5), and this is the half that let it be
//! exercised rather than the half that made it possible. The first
//! presence-assured step in the system declared
//! `assurance_required = "presence"` and left `sign_offs_required`
//! EMPTY. The sign-off surface renders per required role, so with none
//! there was nothing to render, and the UI offered the ordinary
//! complete control — which at the time had no assurance check at all.
//! Measured live on packet d5efbb3c, 2026-09-22: the step completed
//! with `sign_offs: []`, no ceremony, no refusal, and the host then ran
//! the verb it was gating.
//!
//! WHY THIS IS STILL WORTH PINNING NOW THAT THE PUT REFUSES. The two
//! halves do different jobs and neither replaces the other. The guard
//! in `update_step` means the control holds against any caller,
//! including one that never opens a browser. This declaration is what
//! gives a human the door that PRODUCES a presence — without it the
//! step is not bypassable, it is unusable, which is a different bug
//! with the same cause.
//!
//! SO THE RULE IS GENERAL, not a fix to one row: a step that demands
//! more than a session must name at least one role that can supply it.

use boss_core::job::Assurance;
use boss_jobs::registry::seedable_platform_workflows;

/// Every step in the bundle that raises its assurance above `Session`
/// declares a role that can stamp it.
#[test]
fn every_step_demanding_an_assurance_names_who_can_supply_it() {
    let mut checked = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    for wf in seedable_platform_workflows() {
        for step in &wf.steps {
            let required = step.assurance_required.unwrap_or_default();
            if required <= Assurance::Session {
                continue;
            }
            checked += 1;
            if step.sign_offs_required.is_empty() {
                offenders.push(format!("{}.{}", wf.kind, step.title));
            }
        }
    }

    assert!(
        checked > 0,
        "no step in the bundle demands an assurance above Session — either the field was \
         renamed or the approve step was lost, and this pin would otherwise pass while \
         checking nothing"
    );
    assert!(
        offenders.is_empty(),
        "these steps demand an assurance but name nobody who can stamp it: {offenders:?}. \
         The sign-off surface renders per required role, so a step with none offers the \
         ordinary complete control instead — which is how the first presence-assured step \
         in the system was completed with no ceremony (packet d5efbb3c)."
    );
}

/// And the ops-request approval in particular, because it is the one
/// standing in front of a verb that partitions block devices.
#[test]
fn the_ops_approval_is_stamped_by_platform_admin() {
    let wf = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol");
    let approve = wf
        .steps
        .iter()
        .find(|s| s.title == "approve")
        .expect("the approve step");

    assert_eq!(
        approve.assurance_required,
        Some(Assurance::Presence),
        "the approval still demands a passkey"
    );
    assert_eq!(
        approve.sign_offs_required,
        vec!["platform-admin".to_string()],
        "and names the role whose stamp satisfies it"
    );
}
