//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

// ---------------------------------------------------------------------
// THE CONTENT JUDGED IS THE CONTENT COMPLETED (backlog c0b56fd9,
// adversarial review of car 5b30ccf9, 2026-09-25).
//
// A genuine ticket binds step, person and the step's shape hash, and
// `update_step` judged it against the step BEFORE the PUT's overlay. So
// on a presence step with no sign-off roles a valid ticket completed the
// step while the same PUT replaced the plan it had been minted over:
// 204, the stored plan replaced, and nobody's passkey ever saw the bytes
// that were completed. (On a sign-off step the same body was caught only
// incidentally, by the stale-stamp 409.) The test below is the real
// ticket the gateway would have issued — the forgery is not the point.
// ---------------------------------------------------------------------

/// DECIDED: a presence-gated step is not skipped through the PUT, with
/// or without a ticket. A skip satisfies `steps.<slug>.done` exactly as
/// a completion does (ops-request's `execute` waits on
/// `steps.approve.done`), but it walks round the completion contract —
/// the required-at-done `plan` and the sign-off stamps are judged only
/// on `completed`. The ticket carries no verb (step, person, shape,
/// nonce, expiry), so no fresh ticket could say "skip" rather than
/// "approve": the only safe skip is none. Declining is not a skip — it
/// is leaving the step open, or cancelling the packet.
#[tokio::test]
async fn a_presence_ticket_cannot_skip_the_step_it_was_minted_for() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = put(&app, GUARDED, Some(&t), r#"{"status":"skipped"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Ready);

    // The ticket that stamped the sign-off step cannot skip it either —
    // a skip would bypass the stamp the completion checks.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = put(&app, SIGNED, Some(&s), r#"{"status":"skipped"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // Scope control: an ordinary step still skips.
    let (status, body) = put(&app, ORDINARY, None, r#"{"status":"skipped"}"#).await;
    assert!(status.is_success(), "{status}: {body}");
}
