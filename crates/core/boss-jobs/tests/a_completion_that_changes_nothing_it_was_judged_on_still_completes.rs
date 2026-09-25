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

/// What must keep working: the status-only completion car 5b30ccf9's
/// surfaces send with the ceremony's ticket, and a read-merge-write
/// body that sends every stored key back UNCHANGED — the rule is about
/// a change, not about the key being present.
#[tokio::test]
async fn a_completion_that_changes_nothing_it_was_judged_on_still_completes() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    // The sign-off surface: stamp with the ticket, then complete with
    // `{status:'completed'}` alone and the same ticket.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = complete(&app, SIGNED, Some(&s)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Completed);

    // A read-merge-write writer re-sends the plan exactly as stored.
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let resent = serde_json::json!({
        "status": "completed",
        "title": "Approve the plan",
        "metadata": step(GUARDED, None, false).metadata,
    })
    .to_string();
    let (status, body) = put(&app, GUARDED, Some(&t), &resent).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Completed);
}
