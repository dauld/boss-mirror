//! The ops-request protocol's `approve` step — the first
//! presence-assured step in the system.
//!
//! WHY (design 17835005, answered by David 2026-09-21). He approves a
//! destructive change with his passkey and the machine executes it. Q1
//! settled what the signature binds: **a rendered plan, not a verb
//! call.** A verb call authorises an intent whose target can still
//! resolve differently at execution time — which is exactly how
//! 2026-09-21 would have gone wrong. A second NVMe renumbered the
//! forge's devices, the new blank drive came up as `nvme0n1`, and the
//! live root moved to `nvme1n1p2`. Approving "format nvme0n1" would
//! have approved destroying the system disk.
//!
//! THE PLAN IS SIGNED WITHOUT A SECOND HASH EXISTING. `Presence` is
//! producible exactly one way — the gateway verifies a WebAuthn
//! assertion over `sha256(shape_hash || ":" || nonce)` where
//! `shape_hash` is `step_shape_hash(title, metadata)` of the step being
//! signed. Put the plan document in THIS step's metadata and the
//! signature covers the plan's bytes: signing the step's shape IS
//! signing the plan. Nothing to keep in agreement (§9a), and stronger
//! than a bespoke plan hash, because it binds the title and everything
//! else on the step too.
//!
//! AND DRIFT VOIDS IT FOR FREE (q4). The stamp records the shape hash it
//! was made against, and the sign-off endpoint recomputes the hash from
//! the step's CURRENT content before accepting a ticket. Change the plan
//! after the ceremony and the hash moves, so the stamp no longer matches
//! it — an approval cannot survive an edit it never saw.
//!
//! MEASURED BEFORE BUILDING: no published workflow used
//! `assurance_required` at all, and every step kind's `assurance_floor`
//! is `Session`. The whole presence stack — ceremony, ticket swap,
//! endpoint enforcement, the no-bypass rule — was built, tested and
//! demanded by nothing. This is the first row that demands it.

use boss_jobs::registry::seedable_platform_workflows;

fn ops_request() -> boss_jobs::registry::WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol is in the platform bundle")
}

fn step<'a>(
    wf: &'a boss_jobs::registry::WorkflowSpec,
    title: &str,
) -> &'a boss_jobs::registry::StepSpec {
    wf.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| {
            panic!(
                "no `{title}` step; steps are {:?}",
                wf.steps.iter().map(|s| &s.title).collect::<Vec<_>>()
            )
        })
}

/// THE APPROVAL DEMANDS PRESENCE, and that is the whole control.
///
/// `Session` here would mean an authenticated session could approve
/// partitioning a disk. The endpoint's own words: "an assurance level
/// with a bypass is a comment, not a control."
#[test]
fn the_approval_step_demands_presence_not_a_session() {
    let wf = ops_request();
    let approve = step(&wf, "approve");
    assert_eq!(
        approve.assurance_required,
        Some(boss_core::job::Assurance::Presence),
        "the approval of a destructive verb must require a passkey assertion, not a session"
    );
    assert_eq!(
        approve.kind, "sign-off",
        "the approval is a sign-off: the stamp is the record, and it carries the shape \
         hash the signature was made against"
    );
}

/// THE PLAN IS REQUIRED ON THE STEP BEING SIGNED — which is what makes
/// the signature cover it. A plan carried anywhere else on the packet
/// would not be inside `step_shape_hash`, and the passkey would be
/// authorising a step that says nothing about what will happen.
#[test]
fn the_plan_rides_on_the_step_the_signature_covers() {
    let wf = ops_request();
    let approve = step(&wf, "approve");
    let plan = approve
        .fields
        .iter()
        .find(|f| f.name == "plan")
        .expect("the approval step must carry the plan it approves");
    assert!(
        plan.required,
        "an approval with no plan on it is a signature over nothing"
    );

    // The hash is over title + metadata, so a changed plan changes it.
    // Demonstrated rather than asserted about: this is the mechanism the
    // whole design rests on, and it is one function call away.
    let a = boss_core::job::step_shape_hash(
        &approve.title,
        &serde_json::json!({ "plan": "{\"target_by_id\":\"/dev/disk/by-id/nvme-A\"}" }),
    );
    let b = boss_core::job::step_shape_hash(
        &approve.title,
        &serde_json::json!({ "plan": "{\"target_by_id\":\"/dev/disk/by-id/nvme-B\"}" }),
    );
    assert_ne!(
        a, b,
        "two different plans must hash differently, or a signature over one authorises \
         the other — which is the replay this design exists to prevent"
    );
    let again = boss_core::job::step_shape_hash(
        &approve.title,
        &serde_json::json!({ "plan": "{\"target_by_id\":\"/dev/disk/by-id/nvme-A\"}" }),
    );
    assert_eq!(
        a, again,
        "the same plan must hash the same, or an approval breaks between signing and \
         applying"
    );
}

/// EXECUTE WAITS FOR THE APPROVAL — but only where one is required.
///
/// Both halves are load-bearing. Without the wait, the approval is
/// decorative. Without the exemption, every read verb in the allowlist
/// stops working, which is 30-odd verbs and the whole reason the
/// operator stopped being the transport.
#[test]
fn execute_waits_for_the_approval_and_only_where_one_is_required() {
    let wf = ops_request();
    let execute = step(&wf, "execute");
    let when = &execute.ready_when;
    assert!(
        when.contains("steps.approve.done"),
        "execute must wait for the approval, or the approval decides nothing: {when}"
    );
    assert!(
        when.contains("requires_approval"),
        "execute must still run without an approval where none is required — an absent \
         flag reads false, which is what keeps every read verb working: {when}"
    );
    // It has to PARSE, or the step is never ready and every ops-request
    // wedges. A predicate is not prose.
    boss_expr::parse(when)
        .unwrap_or_else(|e| panic!("execute's predicate does not parse: {e}\n{when}"));

    let approve = step(&wf, "approve");
    boss_expr::parse(&approve.ready_when)
        .unwrap_or_else(|e| panic!("approve's predicate does not parse: {e}"));
    assert!(
        approve.ready_when.contains("requires_approval"),
        "the approval must be gated on the flag, or every host read grows an approval \
         step nobody asked for: {}",
        approve.ready_when
    );
}

/// THE FLAG IS ABSENT TODAY, SO THIS LANDS INERT — the same ordering
/// `commission-a-disk` used deliberately: the gate exists before the
/// power does. An ops-request filed right now has no
/// `requires_approval`, an absent value reads false in boolean
/// position, and `execute` is ready exactly when it was.
#[test]
fn an_ordinary_host_read_is_unaffected() {
    let wf = ops_request();
    let execute = step(&wf, "execute");
    let expr = boss_expr::parse(&execute.ready_when).expect("parses");

    // `references` yields one dotted PATH per identifier, as segments.
    let refs: Vec<String> = boss_expr::references(&expr)
        .into_iter()
        .map(|path| path.join("."))
        .collect();
    assert!(
        refs.iter().any(|r| r.contains("filed")),
        "execute still depends on filed: {refs:?}"
    );
    assert!(
        refs.iter().any(|r| r.contains("requires_approval")),
        "and on the flag that exempts it — an absent flag reads false, which is what \
         keeps every read verb filed today working unchanged: {refs:?}"
    );
    assert!(
        refs.iter().any(|r| r.contains("approve")),
        "and on the approval itself: {refs:?}"
    );
}
